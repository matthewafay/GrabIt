//! Bake annotations into a base RGBA image for PNG export.
//!
//! Arrows are drawn with a small convex-polygon scanline rasterizer; text is
//! drawn through `ab_glyph` using a system font (Segoe UI first, then Arial,
//! then Tahoma). We keep the dependency surface minimal by reusing ab_glyph,
//! which egui already pulls in.

use crate::editor::document::AnnotationNode;
use crate::platform::fonts::JETBRAINS_MONO_REGULAR;
use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use std::sync::OnceLock;

static FONT: OnceLock<FontArc> = OnceLock::new();

fn font() -> &'static FontArc {
    FONT.get_or_init(|| {
        FontArc::try_from_slice(JETBRAINS_MONO_REGULAR)
            .expect("embedded JetBrains Mono TTF must parse")
    })
}

/// Apply every annotation in `annotations` to a copy of `base` and return it.
pub fn flatten(base: &RgbaImage, annotations: &[AnnotationNode]) -> RgbaImage {
    let mut out = base.clone();
    for node in annotations {
        match node {
            AnnotationNode::Arrow { start, end, color, thickness, .. } => {
                draw_arrow(&mut out, *start, *end, *color, *thickness);
            }
            AnnotationNode::Text { position, text, color, size_px, .. } => {
                draw_text(&mut out, font(), *position, text, *size_px, *color);
            }
        }
    }
    out
}

/// Render an arrow (shaft + triangular head) on `canvas`.
pub fn draw_arrow(
    canvas: &mut RgbaImage,
    start: [f32; 2],
    end: [f32; 2],
    color: [u8; 4],
    thickness: f32,
) {
    let sx = start[0]; let sy = start[1];
    let ex = end[0]; let ey = end[1];
    let dx = ex - sx;
    let dy = ey - sy;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 { return; }

    let ux = dx / len;
    let uy = dy / len;
    // Perpendicular unit vector (rotated 90°).
    let px = -uy;
    let py = ux;

    // Arrowhead size scales with thickness but is capped at 45% of the arrow
    // length so tiny arrows still read as arrows.
    let head_len = (thickness * 4.0).max(14.0).min(len * 0.45);
    let head_half = head_len * 0.55;

    let shaft_ex = ex - ux * head_len;
    let shaft_ey = ey - uy * head_len;
    let ht = (thickness * 0.5).max(0.5);

    // Shaft: four corners of a thick line.
    fill_convex_polygon(
        canvas,
        &[
            [sx + px * ht, sy + py * ht],
            [sx - px * ht, sy - py * ht],
            [shaft_ex - px * ht, shaft_ey - py * ht],
            [shaft_ex + px * ht, shaft_ey + py * ht],
        ],
        color,
    );

    // Head: tip + two base corners.
    fill_convex_polygon(
        canvas,
        &[
            [ex, ey],
            [shaft_ex + px * head_half, shaft_ey + py * head_half],
            [shaft_ex - px * head_half, shaft_ey - py * head_half],
        ],
        color,
    );
}

/// Scanline fill for a convex polygon. Points are given in pixel coords
/// (float, top-left origin). Alpha composites `color` over `canvas`.
fn fill_convex_polygon(canvas: &mut RgbaImage, points: &[[f32; 2]], color: [u8; 4]) {
    if points.len() < 3 { return; }
    let (w, h) = canvas.dimensions();
    let w = w as i32;
    let h = h as i32;

    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for p in points {
        if p[1] < min_y { min_y = p[1]; }
        if p[1] > max_y { max_y = p[1]; }
    }
    let y_start = (min_y.floor() as i32).max(0);
    let y_end = (max_y.ceil() as i32).min(h - 1);

    for y in y_start..=y_end {
        let yf = y as f32 + 0.5;

        // Collect edge intersections at y.
        let mut xs: Vec<f32> = Vec::with_capacity(4);
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            let (y0, y1) = (a[1], b[1]);
            if (y0 <= yf && yf < y1) || (y1 <= yf && yf < y0) {
                let t = (yf - y0) / (y1 - y0);
                xs.push(a[0] + t * (b[0] - a[0]));
            }
        }
        if xs.len() < 2 { continue; }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // For a convex polygon there are exactly 2 intersections; fill
        // between them. If more (degenerate), fill the outermost pair.
        let lx = xs.first().copied().unwrap_or(0.0);
        let rx = xs.last().copied().unwrap_or(0.0);
        let x0 = (lx.round() as i32).max(0);
        let x1 = (rx.round() as i32).min(w - 1);
        for x in x0..=x1 {
            blend_pixel(canvas, x as u32, y as u32, color);
        }
    }
}

/// Rasterize `text` into `canvas` starting at image-pixel `position`
/// (top-left of the first line's cap-height). `\n` starts a new line.
pub fn draw_text(
    canvas: &mut RgbaImage,
    font: &FontArc,
    position: [f32; 2],
    text: &str,
    size_px: f32,
    color: [u8; 4],
) {
    let scale = PxScale::from(size_px.max(6.0));
    let scaled = font.as_scaled(scale);
    let line_height = scaled.height() + scaled.line_gap();
    let ascent = scaled.ascent();

    let origin_x = position[0];
    let mut cursor_y = position[1] + ascent;
    let mut cursor_x = origin_x;
    let mut prev_glyph: Option<ab_glyph::GlyphId> = None;

    for ch in text.chars() {
        if ch == '\n' {
            cursor_x = origin_x;
            cursor_y += line_height;
            prev_glyph = None;
            continue;
        }
        if ch == '\r' {
            continue;
        }

        let glyph_id = font.glyph_id(ch);
        if let Some(prev) = prev_glyph {
            cursor_x += scaled.kern(prev, glyph_id);
        }

        let glyph =
            glyph_id.with_scale_and_position(scale, ab_glyph::point(cursor_x, cursor_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                if coverage <= 0.0 {
                    return;
                }
                let x = bounds.min.x as i32 + gx as i32;
                let y = bounds.min.y as i32 + gy as i32;
                if x < 0 || y < 0 {
                    return;
                }
                let (cw, ch) = canvas.dimensions();
                if (x as u32) >= cw || (y as u32) >= ch {
                    return;
                }
                // Multiply the stored color's alpha by glyph coverage for
                // smooth edges.
                let a =
                    ((color[3] as f32) * coverage.clamp(0.0, 1.0)).round() as u8;
                blend_pixel(canvas, x as u32, y as u32, [color[0], color[1], color[2], a]);
            });
        }

        cursor_x += scaled.h_advance(glyph_id);
        prev_glyph = Some(glyph_id);
    }
}

fn blend_pixel(canvas: &mut RgbaImage, x: u32, y: u32, color: [u8; 4]) {
    let dst = canvas.get_pixel_mut(x, y);
    let sa = color[3] as u32;
    if sa == 0 { return; }
    if sa == 255 {
        *dst = Rgba(color);
        return;
    }
    let inv = 255 - sa;
    let r = (color[0] as u32 * sa + dst.0[0] as u32 * inv) / 255;
    let g = (color[1] as u32 * sa + dst.0[1] as u32 * inv) / 255;
    let b = (color[2] as u32 * sa + dst.0[2] as u32 * inv) / 255;
    let a = sa + (dst.0[3] as u32 * inv) / 255;
    *dst = Rgba([r as u8, g as u8, b as u8, a.min(255) as u8]);
}
