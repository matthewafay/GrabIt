//! Bake annotations into a base RGBA image for PNG export.
//!
//! Arrows, callouts, shapes, steps, stamps, and magnifiers all have a
//! `draw_*` function here. Text and numeric labels are drawn through
//! `ab_glyph` using the embedded JetBrains Mono face.

use crate::capture::CaptureMetadata;
use crate::editor::document::{
    AnnotationNode, Border, CaptureInfoPosition, CaptureInfoStyle, Edge, EdgeEffect, FieldKind,
    ShapeKind, StampSource,
};
use crate::editor::tools::stamp;
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

/// Apply every annotation in `annotations` to a copy of `base` and return
/// it. Blur regions sample from the untouched `base`, so they stay
/// non-destructive until this final flatten.
///
/// `metadata` is optional: callers without metadata (the cursor preview
/// path) pass `None` and any `CaptureInfo` nodes render as empty blocks.
pub fn flatten(
    base: &RgbaImage,
    annotations: &[AnnotationNode],
    metadata: Option<&CaptureMetadata>,
) -> RgbaImage {
    let mut out = base.clone();
    for node in annotations {
        match node {
            AnnotationNode::Arrow { start, end, color, thickness, .. } => {
                draw_arrow(&mut out, *start, *end, *color, *thickness);
            }
            AnnotationNode::Text { position, text, color, size_px, .. } => {
                draw_text(&mut out, font(), *position, text, *size_px, *color);
            }
            AnnotationNode::Callout {
                rect, tail, text, fill, stroke, stroke_width, text_color, text_size, ..
            } => {
                draw_callout(
                    &mut out, *rect, *tail, text, *fill, *stroke, *stroke_width,
                    *text_color, *text_size,
                );
            }
            AnnotationNode::Shape { shape, rect, stroke, stroke_width, fill, .. } => {
                draw_shape(&mut out, *shape, *rect, *stroke, *stroke_width, *fill);
            }
            AnnotationNode::Step { center, radius, number, fill, text_color, .. } => {
                draw_step(&mut out, *center, *radius, *number, *fill, *text_color);
            }
            AnnotationNode::Stamp { source, rect, .. } => {
                draw_stamp(&mut out, source, *rect);
            }
            AnnotationNode::Magnify {
                source_rect, target_rect, border, border_width, circular, ..
            } => {
                // NB: magnifier samples from the base image BEFORE any later
                // annotations — that matches the intuition that you're
                // zooming into the "real" pixels, not your own overlays.
                draw_magnify(
                    &mut out, base, *source_rect, *target_rect,
                    *border, *border_width, *circular,
                );
            }
            AnnotationNode::Blur { rect, radius_px, .. } => {
                draw_blur(&mut out, base, *rect, *radius_px);
            }
            AnnotationNode::CaptureInfo { position, fields, style, .. } => {
                draw_capture_info(&mut out, metadata, *position, fields, *style);
            }
        }
    }
    out
}

/// Apply document-level effects (torn edge, border + shadow) to an
/// already-flattened image. Returned image may be a different size (borders
/// and shadows extend the canvas).
pub fn apply_document_effects(
    flat: RgbaImage,
    edge_effect: Option<EdgeEffect>,
    border: Option<Border>,
) -> RgbaImage {
    let mut out = flat;
    if let Some(e) = edge_effect {
        out = apply_torn_edge(&out, e);
    }
    if let Some(b) = border {
        out = apply_border(&out, b);
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// Arrow
// ───────────────────────────────────────────────────────────────────────────

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
    let px = -uy;
    let py = ux;

    let head_len = (thickness * 4.0).max(14.0).min(len * 0.45);
    let head_half = head_len * 0.55;

    let shaft_ex = ex - ux * head_len;
    let shaft_ey = ey - uy * head_len;
    let ht = (thickness * 0.5).max(0.5);

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

// ───────────────────────────────────────────────────────────────────────────
// Shape (rect / ellipse)
// ───────────────────────────────────────────────────────────────────────────

pub fn draw_shape(
    canvas: &mut RgbaImage,
    shape: ShapeKind,
    rect: [f32; 4],
    stroke: [u8; 4],
    stroke_width: f32,
    fill: [u8; 4],
) {
    let r = normalise(rect);
    match shape {
        ShapeKind::Rect => {
            if fill[3] > 0 { fill_rect(canvas, r, fill); }
            stroke_rect(canvas, r, stroke_width, stroke);
        }
        ShapeKind::Ellipse => {
            if fill[3] > 0 { fill_ellipse(canvas, r, fill); }
            stroke_ellipse(canvas, r, stroke_width, stroke);
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Step marker
// ───────────────────────────────────────────────────────────────────────────

pub fn draw_step(
    canvas: &mut RgbaImage,
    center: [f32; 2],
    radius: f32,
    number: u32,
    fill: [u8; 4],
    text_color: [u8; 4],
) {
    fill_circle(canvas, center, radius, fill);
    stroke_circle(canvas, center, radius, (radius * 0.08).max(1.5), fill_darken(fill, 0.5));

    // Center the numeric label inside the circle. We measure the glyph
    // extents ourselves so small/large radii both read correctly.
    let label = number.to_string();
    let size_px = radius * 1.2;
    let (tw, th) = measure_text(font(), &label, size_px);
    let tx = center[0] - tw * 0.5;
    let ty = center[1] - th * 0.5;
    draw_text(canvas, font(), [tx, ty], &label, size_px, text_color);
}

// ───────────────────────────────────────────────────────────────────────────
// Callout
// ───────────────────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)] // Callout is structurally rich — 8 free params is the honest signature.
pub fn draw_callout(
    canvas: &mut RgbaImage,
    rect: [f32; 4],
    tail: [f32; 2],
    text: &str,
    fill: [u8; 4],
    stroke: [u8; 4],
    stroke_width: f32,
    text_color: [u8; 4],
    text_size: f32,
) {
    let r = normalise(rect);
    let cx = 0.5 * (r[0] + r[2]);
    let cy = 0.5 * (r[1] + r[3]);
    let half_w = 0.5 * (r[2] - r[0]);
    let half_h = 0.5 * (r[3] - r[1]);
    if half_w <= 0.0 || half_h <= 0.0 { return; }

    // Tail base: two points along the edge of the balloon nearest the tail tip.
    // Pick the edge by seeing which axis the tail is most outside on.
    let dx = tail[0] - cx;
    let dy = tail[1] - cy;
    let base_half = (half_w.min(half_h) * 0.35).max(6.0);
    let (b0, b1) = if dy.abs() * half_w >= dx.abs() * half_h {
        // Tail is mostly above/below — base runs horizontally on top/bottom.
        let y = if dy >= 0.0 { r[3] } else { r[1] };
        let tx = cx + dx.signum() * base_half;
        ([cx - base_half * 0.5, y], [tx, y])
    } else {
        // Tail is mostly left/right — base runs vertically on the side.
        let x = if dx >= 0.0 { r[2] } else { r[0] };
        let ty = cy + dy.signum() * base_half;
        ([x, cy - base_half * 0.5], [x, ty])
    };

    // Fill the balloon + tail so the tail is flush with the balloon edge.
    fill_rect(canvas, r, fill);
    fill_convex_polygon(canvas, &[b0, b1, tail], fill);
    // Stroke the whole outline — balloon rect + tail triangle.
    stroke_rect(canvas, r, stroke_width, stroke);
    stroke_segment(canvas, b0, tail, stroke_width, stroke);
    stroke_segment(canvas, b1, tail, stroke_width, stroke);

    // Text with a small inset.
    let pad = text_size * 0.35;
    draw_text(
        canvas,
        font(),
        [r[0] + pad, r[1] + pad],
        text,
        text_size,
        text_color,
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Stamp
// ───────────────────────────────────────────────────────────────────────────

pub fn draw_stamp(canvas: &mut RgbaImage, source: &StampSource, rect: [f32; 4]) {
    let Ok(img) = stamp::resolve(source) else { return };
    blit_scaled(canvas, &img, rect);
}

// ───────────────────────────────────────────────────────────────────────────
// Magnifier
// ───────────────────────────────────────────────────────────────────────────

pub fn draw_magnify(
    canvas: &mut RgbaImage,
    base: &RgbaImage,
    source_rect: [f32; 4],
    target_rect: [f32; 4],
    border: [u8; 4],
    border_width: f32,
    circular: bool,
) {
    let src = normalise(source_rect);
    let dst = normalise(target_rect);
    let dw = dst[2] - dst[0];
    let dh = dst[3] - dst[1];
    let sw = src[2] - src[0];
    let sh = src[3] - src[1];
    if dw < 1.0 || dh < 1.0 || sw < 1.0 || sh < 1.0 { return; }

    let (bw, bh) = base.dimensions();
    let cx = 0.5 * (dst[0] + dst[2]);
    let cy = 0.5 * (dst[1] + dst[3]);
    let rx = 0.5 * dw;
    let ry = 0.5 * dh;

    let x0 = dst[0].floor().max(0.0) as i32;
    let y0 = dst[1].floor().max(0.0) as i32;
    let x1 = dst[2].ceil().min(canvas.width() as f32) as i32;
    let y1 = dst[3].ceil().min(canvas.height() as f32) as i32;

    for y in y0..y1 {
        for x in x0..x1 {
            // Circular clipping.
            if circular {
                let ex = (x as f32 + 0.5 - cx) / rx;
                let ey = (y as f32 + 0.5 - cy) / ry;
                if ex * ex + ey * ey > 1.0 { continue; }
            }
            // Sample the base at the mapped point.
            let u = (x as f32 + 0.5 - dst[0]) / dw;
            let v = (y as f32 + 0.5 - dst[1]) / dh;
            let sx = src[0] + u * sw;
            let sy = src[1] + v * sh;
            let sxi = (sx as i32).clamp(0, bw as i32 - 1) as u32;
            let syi = (sy as i32).clamp(0, bh as i32 - 1) as u32;
            let p = base.get_pixel(sxi, syi).0;
            blend_pixel(canvas, x as u32, y as u32, p);
        }
    }

    // Border.
    if border[3] > 0 && border_width > 0.0 {
        if circular {
            stroke_ellipse(canvas, dst, border_width, border);
        } else {
            stroke_rect(canvas, dst, border_width, border);
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Text
// ───────────────────────────────────────────────────────────────────────────

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
                let a =
                    ((color[3] as f32) * coverage.clamp(0.0, 1.0)).round() as u8;
                blend_pixel(canvas, x as u32, y as u32, [color[0], color[1], color[2], a]);
            });
        }

        cursor_x += scaled.h_advance(glyph_id);
        prev_glyph = Some(glyph_id);
    }
}

/// Measure a single-line text run at `size_px`. Returns (width, height) in
/// image pixels. Height is cap-to-descender (ascent + descent).
pub fn measure_text(font: &FontArc, text: &str, size_px: f32) -> (f32, f32) {
    let scale = PxScale::from(size_px.max(6.0));
    let scaled = font.as_scaled(scale);
    let h = scaled.ascent() - scaled.descent();
    let mut w = 0.0f32;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        if ch == '\n' || ch == '\r' { continue; }
        let g = font.glyph_id(ch);
        if let Some(p) = prev { w += scaled.kern(p, g); }
        w += scaled.h_advance(g);
        prev = Some(g);
    }
    (w, h)
}

// ───────────────────────────────────────────────────────────────────────────
// Primitive raster helpers
// ───────────────────────────────────────────────────────────────────────────

fn normalise(r: [f32; 4]) -> [f32; 4] {
    [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])]
}

fn fill_rect(canvas: &mut RgbaImage, r: [f32; 4], color: [u8; 4]) {
    let (w, h) = canvas.dimensions();
    let x0 = (r[0].round() as i32).max(0);
    let y0 = (r[1].round() as i32).max(0);
    let x1 = (r[2].round() as i32).min(w as i32);
    let y1 = (r[3].round() as i32).min(h as i32);
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(canvas, x as u32, y as u32, color);
        }
    }
}

fn stroke_rect(canvas: &mut RgbaImage, r: [f32; 4], thickness: f32, color: [u8; 4]) {
    if thickness <= 0.0 || color[3] == 0 { return; }
    let t = thickness.max(0.5);
    stroke_segment(canvas, [r[0], r[1]], [r[2], r[1]], t, color);
    stroke_segment(canvas, [r[2], r[1]], [r[2], r[3]], t, color);
    stroke_segment(canvas, [r[2], r[3]], [r[0], r[3]], t, color);
    stroke_segment(canvas, [r[0], r[3]], [r[0], r[1]], t, color);
}

/// Thick line segment drawn as a capsule (rectangle body + round caps).
fn stroke_segment(
    canvas: &mut RgbaImage,
    a: [f32; 2],
    b: [f32; 2],
    thickness: f32,
    color: [u8; 4],
) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.001 {
        fill_circle(canvas, a, thickness * 0.5, color);
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;
    let h = thickness * 0.5;
    fill_convex_polygon(
        canvas,
        &[
            [a[0] + px * h, a[1] + py * h],
            [a[0] - px * h, a[1] - py * h],
            [b[0] - px * h, b[1] - py * h],
            [b[0] + px * h, b[1] + py * h],
        ],
        color,
    );
    // Round caps
    fill_circle(canvas, a, h, color);
    fill_circle(canvas, b, h, color);
}

fn fill_ellipse(canvas: &mut RgbaImage, r: [f32; 4], color: [u8; 4]) {
    let cx = 0.5 * (r[0] + r[2]);
    let cy = 0.5 * (r[1] + r[3]);
    let rx = 0.5 * (r[2] - r[0]).abs();
    let ry = 0.5 * (r[3] - r[1]).abs();
    if rx < 0.5 || ry < 0.5 { return; }
    let (w, h) = canvas.dimensions();
    let y0 = ((cy - ry).floor() as i32).max(0);
    let y1 = ((cy + ry).ceil() as i32).min(h as i32 - 1);
    for y in y0..=y1 {
        let ny = (y as f32 + 0.5 - cy) / ry;
        if ny.abs() > 1.0 { continue; }
        let dx = rx * (1.0 - ny * ny).sqrt();
        let x0 = ((cx - dx).round() as i32).max(0);
        let x1 = ((cx + dx).round() as i32).min(w as i32 - 1);
        for x in x0..=x1 {
            blend_pixel(canvas, x as u32, y as u32, color);
        }
    }
}

fn stroke_ellipse(canvas: &mut RgbaImage, r: [f32; 4], thickness: f32, color: [u8; 4]) {
    if thickness <= 0.0 || color[3] == 0 { return; }
    let cx = 0.5 * (r[0] + r[2]);
    let cy = 0.5 * (r[1] + r[3]);
    let rx = 0.5 * (r[2] - r[0]).abs();
    let ry = 0.5 * (r[3] - r[1]).abs();
    if rx < 0.5 || ry < 0.5 { return; }
    // Parametric march — good enough for the thicknesses we expect (<20px).
    let steps = ((rx.max(ry) * std::f32::consts::TAU).round() as i32).max(16);
    let t = thickness.max(0.5);
    let mut prev: Option<[f32; 2]> = None;
    for i in 0..=steps {
        let ang = (i as f32) * std::f32::consts::TAU / (steps as f32);
        let p = [cx + rx * ang.cos(), cy + ry * ang.sin()];
        if let Some(q) = prev {
            stroke_segment(canvas, q, p, t, color);
        }
        prev = Some(p);
    }
}

fn fill_circle(canvas: &mut RgbaImage, center: [f32; 2], radius: f32, color: [u8; 4]) {
    if radius <= 0.0 { return; }
    let r2 = radius * radius;
    let (w, h) = canvas.dimensions();
    let y0 = ((center[1] - radius).floor() as i32).max(0);
    let y1 = ((center[1] + radius).ceil() as i32).min(h as i32 - 1);
    for y in y0..=y1 {
        let dy = y as f32 + 0.5 - center[1];
        let dd = r2 - dy * dy;
        if dd < 0.0 { continue; }
        let dx = dd.sqrt();
        let x0 = ((center[0] - dx).round() as i32).max(0);
        let x1 = ((center[0] + dx).round() as i32).min(w as i32 - 1);
        for x in x0..=x1 {
            blend_pixel(canvas, x as u32, y as u32, color);
        }
    }
}

fn stroke_circle(canvas: &mut RgbaImage, center: [f32; 2], radius: f32, thickness: f32, color: [u8; 4]) {
    if thickness <= 0.0 || color[3] == 0 || radius <= 0.0 { return; }
    let outer = radius + thickness * 0.5;
    let inner = (radius - thickness * 0.5).max(0.0);
    let o2 = outer * outer;
    let i2 = inner * inner;
    let (w, h) = canvas.dimensions();
    let y0 = ((center[1] - outer).floor() as i32).max(0);
    let y1 = ((center[1] + outer).ceil() as i32).min(h as i32 - 1);
    for y in y0..=y1 {
        let dy = y as f32 + 0.5 - center[1];
        let dy2 = dy * dy;
        if dy2 > o2 { continue; }
        let xo = (o2 - dy2).sqrt();
        let xi_opt = if dy2 < i2 { Some((i2 - dy2).sqrt()) } else { None };
        let x_outer_l = ((center[0] - xo).round() as i32).max(0);
        let x_outer_r = ((center[0] + xo).round() as i32).min(w as i32 - 1);
        match xi_opt {
            None => {
                for x in x_outer_l..=x_outer_r {
                    blend_pixel(canvas, x as u32, y as u32, color);
                }
            }
            Some(xi) => {
                let x_inner_l = ((center[0] - xi).round() as i32).max(0);
                let x_inner_r = ((center[0] + xi).round() as i32).min(w as i32 - 1);
                for x in x_outer_l..x_inner_l {
                    blend_pixel(canvas, x as u32, y as u32, color);
                }
                for x in (x_inner_r + 1)..=x_outer_r {
                    blend_pixel(canvas, x as u32, y as u32, color);
                }
            }
        }
    }
}

fn fill_darken(c: [u8; 4], factor: f32) -> [u8; 4] {
    [
        ((c[0] as f32) * factor) as u8,
        ((c[1] as f32) * factor) as u8,
        ((c[2] as f32) * factor) as u8,
        c[3],
    ]
}

/// Scanline fill for a convex polygon.
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

        let lx = xs.first().copied().unwrap_or(0.0);
        let rx = xs.last().copied().unwrap_or(0.0);
        let x0 = (lx.round() as i32).max(0);
        let x1 = (rx.round() as i32).min(w - 1);
        for x in x0..=x1 {
            blend_pixel(canvas, x as u32, y as u32, color);
        }
    }
}

/// Alpha-blend a scaled RGBA image into `dst` at the specified rect.
/// Uses nearest-neighbour sampling — stamps are small and this keeps the
/// dep graph off `fast_image_resize` until M4.
fn blit_scaled(dst: &mut RgbaImage, src: &RgbaImage, rect: [f32; 4]) {
    let r = normalise(rect);
    let dw = r[2] - r[0];
    let dh = r[3] - r[1];
    if dw < 1.0 || dh < 1.0 { return; }
    let (sw, sh) = src.dimensions();
    let (dst_w, dst_h) = dst.dimensions();
    let x0 = (r[0].floor() as i32).max(0);
    let y0 = (r[1].floor() as i32).max(0);
    let x1 = (r[2].ceil() as i32).min(dst_w as i32);
    let y1 = (r[3].ceil() as i32).min(dst_h as i32);
    for y in y0..y1 {
        for x in x0..x1 {
            let u = (x as f32 + 0.5 - r[0]) / dw;
            let v = (y as f32 + 0.5 - r[1]) / dh;
            let sx = (u * sw as f32) as i32;
            let sy = (v * sh as f32) as i32;
            if sx < 0 || sy < 0 || sx >= sw as i32 || sy >= sh as i32 { continue; }
            let p = src.get_pixel(sx as u32, sy as u32).0;
            blend_pixel(dst, x as u32, y as u32, p);
        }
    }
}

fn blend_pixel(canvas: &mut RgbaImage, x: u32, y: u32, color: [u8; 4]) {
    let (w, h) = canvas.dimensions();
    if x >= w || y >= h { return; }
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

// ───────────────────────────────────────────────────────────────────────────
// Blur (M4)
// ───────────────────────────────────────────────────────────────────────────

/// Gaussian-blur the pixels of `base` inside `rect`, then blit the blurred
/// crop back onto `canvas`. This is the destructive half of feature #16 —
/// the `.grabit` keeps only the rect + radius; pixels aren't mutated until
/// flatten/export runs.
pub fn draw_blur(canvas: &mut RgbaImage, base: &RgbaImage, rect: [f32; 4], radius_px: f32) {
    let r = normalise(rect);
    let (bw, bh) = base.dimensions();
    let x0 = r[0].floor().max(0.0) as u32;
    let y0 = r[1].floor().max(0.0) as u32;
    let x1 = (r[2].ceil() as u32).min(bw);
    let y1 = (r[3].ceil() as u32).min(bh);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let w = x1 - x0;
    let h = y1 - y0;

    let mut crop = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let p = *base.get_pixel(x0 + x, y0 + y);
            crop.put_pixel(x, y, p);
        }
    }

    let sigma = radius_px.max(0.5);
    let blurred = imageproc::filter::gaussian_blur_f32(&crop, sigma);

    for y in 0..h {
        for x in 0..w {
            let p = blurred.get_pixel(x, y).0;
            blend_pixel(canvas, x0 + x, y0 + y, p);
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Capture-info stamp (M4)
// ───────────────────────────────────────────────────────────────────────────

/// Format the selected metadata fields as a list of lines.
pub fn capture_info_lines(
    metadata: Option<&CaptureMetadata>,
    fields: &[FieldKind],
) -> Vec<String> {
    let Some(m) = metadata else { return Vec::new() };
    fields
        .iter()
        .filter_map(|f| {
            let value = match f {
                FieldKind::Timestamp => {
                    m.captured_at.format("%Y-%m-%d %H:%M:%S").to_string()
                }
                FieldKind::WindowTitle => m.foreground_title.clone()?,
                FieldKind::ProcessName => m.foreground_process.clone()?,
                FieldKind::OsVersion => m.os_version.clone(),
                FieldKind::MonitorInfo => {
                    if m.monitors.is_empty() {
                        return None;
                    }
                    let parts: Vec<String> = m
                        .monitors
                        .iter()
                        .map(|mi| {
                            format!(
                                "{}x{}{}",
                                mi.rect.width,
                                mi.rect.height,
                                if mi.is_primary { "*" } else { "" }
                            )
                        })
                        .collect();
                    parts.join(" ")
                }
            };
            Some(format!("{}: {}", f.label(), value))
        })
        .collect()
}

/// Measure the total size of a multi-line text block at `size_px`. Used to
/// size the info banner before drawing.
fn measure_block(font: &FontArc, lines: &[String], size_px: f32) -> (f32, f32) {
    let scale = PxScale::from(size_px.max(6.0));
    let scaled = font.as_scaled(scale);
    let line_h = scaled.height() + scaled.line_gap();
    let mut max_w = 0.0f32;
    for line in lines {
        let (w, _) = measure_text(font, line, size_px);
        if w > max_w {
            max_w = w;
        }
    }
    let total_h = line_h * lines.len().max(1) as f32;
    (max_w, total_h)
}

/// Draw the capture-info banner. Fields with no data (e.g. no foreground
/// window) are skipped, so an empty info block renders as nothing.
pub fn draw_capture_info(
    canvas: &mut RgbaImage,
    metadata: Option<&CaptureMetadata>,
    position: CaptureInfoPosition,
    fields: &[FieldKind],
    style: CaptureInfoStyle,
) {
    let lines = capture_info_lines(metadata, fields);
    if lines.is_empty() {
        return;
    }
    let (cw, ch) = canvas.dimensions();
    let (block_w, block_h) = measure_block(font(), &lines, style.text_size);
    let pad = style.padding.max(0.0);
    let box_w = block_w + pad * 2.0;
    let box_h = block_h + pad * 2.0;
    let (x0, y0) = match position {
        CaptureInfoPosition::TopLeft => (0.0, 0.0),
        CaptureInfoPosition::TopRight => (cw as f32 - box_w, 0.0),
        CaptureInfoPosition::BottomLeft => (0.0, ch as f32 - box_h),
        CaptureInfoPosition::BottomRight => (cw as f32 - box_w, ch as f32 - box_h),
    };
    let x1 = x0 + box_w;
    let y1 = y0 + box_h;
    fill_rect(canvas, [x0, y0, x1, y1], style.fill);

    // Draw each line starting from the padded origin.
    let scale = PxScale::from(style.text_size.max(6.0));
    let scaled = font().as_scaled(scale);
    let line_h = scaled.height() + scaled.line_gap();
    let mut y = y0 + pad;
    for line in &lines {
        draw_text(canvas, font(), [x0 + pad, y], line, style.text_size, style.text_color);
        y += line_h;
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Torn edge (M4, feature #21)
// ───────────────────────────────────────────────────────────────────────────

/// Cut a jagged triangular profile out of one edge of `img`. The returned
/// image is the same size; pixels in the teeth region are set to fully
/// transparent so the image reads as torn paper.
pub fn apply_torn_edge(img: &RgbaImage, effect: EdgeEffect) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = img.clone();
    if w == 0 || h == 0 {
        return out;
    }
    let depth = effect.depth.max(1.0);
    let teeth = effect.teeth.max(4.0);

    // Deterministic pseudo-random jitter so the teeth aren't perfectly
    // regular; varying by position of the tooth, not by frame, so flatten
    // results are stable.
    let jitter = |n: u32| -> f32 {
        let mut s = n.wrapping_mul(2654435761).wrapping_add(1);
        s ^= s >> 16;
        s = s.wrapping_mul(0x85ebca6b);
        let u = (s & 0xFFFF) as f32 / 65536.0; // 0..1
        (u - 0.5) * 0.6 // +/- 0.3
    };

    match effect.edge {
        Edge::Top | Edge::Bottom => {
            for x in 0..w {
                // Tooth profile: sawtooth with jitter. `local` goes 0..teeth.
                let idx = x as f32 / teeth;
                let tooth_n = idx.floor() as u32;
                let frac = idx - idx.floor();
                // Triangle wave 0..1..0 across one tooth.
                let tri = 1.0 - (2.0 * frac - 1.0).abs();
                let j = 1.0 + jitter(tooth_n);
                let depth_here = depth * tri * j;
                let d = depth_here.max(0.0) as u32;
                for y in 0..d.min(h) {
                    let yy = match effect.edge {
                        Edge::Top => y,
                        Edge::Bottom => h - 1 - y,
                        _ => unreachable!(),
                    };
                    out.put_pixel(x, yy, Rgba([0, 0, 0, 0]));
                }
            }
        }
        Edge::Left | Edge::Right => {
            for y in 0..h {
                let idx = y as f32 / teeth;
                let tooth_n = idx.floor() as u32;
                let frac = idx - idx.floor();
                let tri = 1.0 - (2.0 * frac - 1.0).abs();
                let j = 1.0 + jitter(tooth_n);
                let depth_here = depth * tri * j;
                let d = depth_here.max(0.0) as u32;
                for x in 0..d.min(w) {
                    let xx = match effect.edge {
                        Edge::Left => x,
                        Edge::Right => w - 1 - x,
                        _ => unreachable!(),
                    };
                    out.put_pixel(xx, y, Rgba([0, 0, 0, 0]));
                }
            }
        }
    }

    out
}

// ───────────────────────────────────────────────────────────────────────────
// Border + drop shadow (M4, feature #22)
// ───────────────────────────────────────────────────────────────────────────

/// Pad `img` with a solid border band and optional drop shadow outside it.
/// Returns a new, larger RGBA image.
pub fn apply_border(img: &RgbaImage, border: Border) -> RgbaImage {
    let (w, h) = img.dimensions();
    let bw = border.width.max(0.0).round() as u32;
    let shadow_r = border.shadow_radius.max(0.0).round() as u32;
    let sox = border.shadow_offset[0].round() as i32;
    let soy = border.shadow_offset[1].round() as i32;

    // Total margins on each side account for both border and shadow spread
    // plus offset (in whichever direction each axis is positive).
    let pad_l = bw + shadow_r + (-sox).max(0) as u32;
    let pad_r = bw + shadow_r + sox.max(0) as u32;
    let pad_t = bw + shadow_r + (-soy).max(0) as u32;
    let pad_b = bw + shadow_r + soy.max(0) as u32;

    let new_w = w + pad_l + pad_r;
    let new_h = h + pad_t + pad_b;
    let mut out = RgbaImage::from_pixel(new_w, new_h, Rgba([0, 0, 0, 0]));

    let content_x = pad_l as i32;
    let content_y = pad_t as i32;

    // Draw drop shadow first (underneath the image + border).
    if shadow_r > 0 || border.shadow_color[3] > 0 && (sox != 0 || soy != 0) {
        // Solid shadow rect offset by (sox, soy), then gaussian-blurred.
        let shadow_rect_x = content_x - bw as i32 + sox;
        let shadow_rect_y = content_y - bw as i32 + soy;
        let shadow_rect_w = w + 2 * bw;
        let shadow_rect_h = h + 2 * bw;
        // Stamp a solid rectangle of the shadow color.
        let x0 = shadow_rect_x.max(0) as u32;
        let y0 = shadow_rect_y.max(0) as u32;
        let x1 = ((shadow_rect_x + shadow_rect_w as i32).min(new_w as i32)).max(0) as u32;
        let y1 = ((shadow_rect_y + shadow_rect_h as i32).min(new_h as i32)).max(0) as u32;
        if x1 > x0 && y1 > y0 {
            for y in y0..y1 {
                for x in x0..x1 {
                    out.put_pixel(x, y, Rgba(border.shadow_color));
                }
            }
            if shadow_r > 0 {
                let sigma = shadow_r as f32;
                out = imageproc::filter::gaussian_blur_f32(&out, sigma);
            }
        }
    }

    // Draw border band.
    if bw > 0 && border.color[3] > 0 {
        let bx0 = (content_x - bw as i32).max(0) as u32;
        let by0 = (content_y - bw as i32).max(0) as u32;
        let bx1 = ((content_x + w as i32 + bw as i32).min(new_w as i32)).max(0) as u32;
        let by1 = ((content_y + h as i32 + bw as i32).min(new_h as i32)).max(0) as u32;
        for y in by0..by1 {
            for x in bx0..bx1 {
                blend_pixel(&mut out, x, y, border.color);
            }
        }
    }

    // Blit original image on top (non-premultiplied over).
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            let dx = content_x + x as i32;
            let dy = content_y + y as i32;
            if dx < 0 || dy < 0 || dx >= new_w as i32 || dy >= new_h as i32 {
                continue;
            }
            // Force-write the image pixel — we want the original to survive
            // over the border band exactly (no alpha-blend inside the
            // content rect). Transparent source pixels (torn-edge holes)
            // still reveal the border/shadow underneath, so we use blend.
            blend_pixel(&mut out, dx as u32, dy as u32, p);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_rect_stays_in_bounds() {
        let mut img = RgbaImage::new(32, 32);
        draw_shape(
            &mut img,
            ShapeKind::Rect,
            [4.0, 4.0, 28.0, 28.0],
            [255, 0, 0, 255],
            2.0,
            [0, 255, 0, 128],
        );
        // No panic ≡ pass.
    }

    #[test]
    fn draw_magnify_samples_base() {
        // Build a simple 4x4 checker and verify drawing into the top-left
        // from a source rect doesn't panic and blends some pixels.
        let mut img = RgbaImage::new(16, 16);
        let mut base = RgbaImage::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                base.put_pixel(x, y, Rgba([v, v, v, 255]));
            }
        }
        draw_magnify(
            &mut img,
            &base,
            [0.0, 0.0, 4.0, 4.0],
            [0.0, 0.0, 12.0, 12.0],
            [255, 0, 0, 255],
            1.0,
            false,
        );
        let p = img.get_pixel(3, 3).0;
        // Some pixel in the target must have been written.
        assert!(p[3] > 0);
    }

    #[test]
    fn draw_blur_noop_in_bounds() {
        // Solid-colour base: after blur the pixels should still have the
        // same colour (blur of a uniform field is a no-op).
        let base = RgbaImage::from_pixel(32, 32, Rgba([200, 100, 50, 255]));
        let mut canvas = base.clone();
        draw_blur(&mut canvas, &base, [4.0, 4.0, 28.0, 28.0], 8.0);
        let p = canvas.get_pixel(16, 16).0;
        assert_eq!(p[3], 255);
        // R should be approximately 200 (within blur rounding).
        assert!((p[0] as i32 - 200).abs() <= 2);
    }

    #[test]
    fn draw_capture_info_renders() {
        use crate::capture::{CaptureMetadata, Rect};
        use chrono::TimeZone;
        let mut canvas = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 255]));
        let meta = CaptureMetadata {
            captured_at: chrono::Utc.with_ymd_and_hms(2026, 4, 21, 12, 0, 0).unwrap(),
            foreground_title: Some("Test".into()),
            foreground_process: Some("proc.exe".into()),
            os_version: "Windows 10.0".into(),
            monitors: vec![],
            capture_rect: Rect { x: 0, y: 0, width: 64, height: 64 },
        };
        draw_capture_info(
            &mut canvas,
            Some(&meta),
            CaptureInfoPosition::TopLeft,
            &[FieldKind::Timestamp, FieldKind::ProcessName],
            CaptureInfoStyle::default(),
        );
        // Top-left corner should have been filled with banner pixels.
        let p = canvas.get_pixel(1, 1).0;
        assert!(p[0] < 255 || p[1] < 255 || p[2] < 255);
    }

    #[test]
    fn apply_torn_edge_drops_pixels() {
        let base = RgbaImage::from_pixel(32, 32, Rgba([200, 100, 50, 255]));
        let torn = apply_torn_edge(&base, EdgeEffect {
            edge: Edge::Bottom,
            depth: 4.0,
            teeth: 8.0,
        });
        // Some bottom-row pixels must now be transparent.
        let mut any_transparent = false;
        for x in 0..32 {
            if torn.get_pixel(x, 31).0[3] == 0 {
                any_transparent = true;
                break;
            }
        }
        assert!(any_transparent);
    }

    #[test]
    fn apply_border_grows_image() {
        let base = RgbaImage::from_pixel(32, 32, Rgba([200, 100, 50, 255]));
        let out = apply_border(&base, Border {
            color: [0, 0, 0, 255],
            width: 4.0,
            shadow_radius: 0.0,
            shadow_offset: [0.0, 0.0],
            shadow_color: [0, 0, 0, 0],
        });
        assert_eq!(out.width(), 32 + 8);
        assert_eq!(out.height(), 32 + 8);
        // Border pixel is black.
        let b = out.get_pixel(1, 1).0;
        assert_eq!(b, [0, 0, 0, 255]);
        // Content pixel preserved.
        let c = out.get_pixel(4 + 16, 4 + 16).0;
        assert_eq!(c, [200, 100, 50, 255]);
    }
}
