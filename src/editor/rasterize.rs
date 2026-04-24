//! Bake annotations into a base RGBA image for PNG export.
//!
//! Arrows, callouts, shapes, steps, stamps, and magnifiers all have a
//! `draw_*` function here. Text and numeric labels are drawn through
//! `ab_glyph` using the embedded JetBrains Mono face.

use crate::capture::CaptureMetadata;
use crate::editor::document::{
    AnnotationNode, ArrowHeadStyle, ArrowLineStyle, Border, CaptureInfoPosition,
    CaptureInfoStyle, Edge, EdgeEffect, FieldKind, ShapeKind, StampSource,
    TextAlign, TextListStyle,
};
use crate::editor::tools::selection::sample_bezier;
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
            AnnotationNode::Arrow {
                start, end, color, thickness, shadow,
                line_style, head_style, control,
                ..
            } => {
                draw_arrow(
                    &mut out, *start, *end, *color, *thickness, *shadow,
                    *line_style, *head_style, *control,
                );
            }
            AnnotationNode::Text {
                rect, text, color, size_px, frosted, shadow, align, list, ..
            } => {
                // Effects compose underneath the glyphs. Order per spec:
                // shadow → frosted backdrop → text. Matches the live
                // preview in `app.rs::paint_text_effects_live`.
                if *shadow {
                    draw_text_shadow(&mut out, *rect);
                }
                if *frosted {
                    draw_text_frosted(&mut out, base, *rect);
                }
                draw_text_box(
                    &mut out, font(), *rect, text, *size_px, *color, *align, *list,
                );
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

/// Render an arrow (shaft + head) on `canvas`. If `shadow` is true, a
/// darkened tint of the arrow's colour is drawn first at a small offset.
/// Rasterized through 4× supersampling (16 samples per output pixel) so
/// oblique edges read as smooth rather than pixel-jagged. `control`
/// promotes the shaft from a straight line to a quadratic bezier.
#[allow(clippy::too_many_arguments)]
pub fn draw_arrow(
    canvas: &mut RgbaImage,
    start: [f32; 2],
    end: [f32; 2],
    color: [u8; 4],
    thickness: f32,
    shadow: bool,
    line_style: ArrowLineStyle,
    head_style: ArrowHeadStyle,
    control: Option<[f32; 2]>,
) {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    if (dx * dx + dy * dy) < 1.0 { return; }

    // AABB: pad generously for the head's perpendicular extent, the shaft
    // thickness, the shadow offset, and the control point if present (a
    // quadratic bezier is fully contained by the convex hull of its three
    // anchor points).
    let head_len_est = (thickness * 4.0).max(14.0);
    let head_half = head_len_est * 0.55;
    let shadow_off = if shadow { (thickness * 0.35).max(2.0) } else { 0.0 };
    let pad = (thickness * 0.5 + head_half).max(thickness + 2.0) + shadow_off + 2.0;

    let mut minx = start[0].min(end[0]);
    let mut miny = start[1].min(end[1]);
    let mut maxx = start[0].max(end[0]);
    let mut maxy = start[1].max(end[1]);
    if let Some(c) = control {
        minx = minx.min(c[0]);
        miny = miny.min(c[1]);
        maxx = maxx.max(c[0]);
        maxy = maxy.max(c[1]);
    }
    let (minx, miny, maxx, maxy) = (minx - pad, miny - pad, maxx + pad, maxy + pad);

    let (cw, ch) = canvas.dimensions();
    let x0 = (minx.floor() as i32).max(0) as u32;
    let y0 = (miny.floor() as i32).max(0) as u32;
    let x1 = (maxx.ceil() as i32).min(cw as i32).max(0) as u32;
    let y1 = (maxy.ceil() as i32).min(ch as i32).max(0) as u32;
    if x1 <= x0 || y1 <= y0 { return; }

    let sub_w = x1 - x0;
    let sub_h = y1 - y0;
    const SSAA: u32 = 4;
    let mut hi = RgbaImage::new(sub_w * SSAA, sub_h * SSAA);

    let shift = |p: [f32; 2]| -> [f32; 2] {
        [
            (p[0] - x0 as f32) * SSAA as f32,
            (p[1] - y0 as f32) * SSAA as f32,
        ]
    };
    draw_arrow_aliased(
        &mut hi,
        shift(start),
        shift(end),
        color,
        thickness * SSAA as f32,
        shadow,
        line_style,
        head_style,
        control.map(shift),
    );

    // Downsample SSAA×SSAA → 1 by averaging RGBA, then alpha-blend onto canvas.
    let n = (SSAA * SSAA) as u32;
    for dy in 0..sub_h {
        for dx in 0..sub_w {
            let mut sum: [u32; 4] = [0, 0, 0, 0];
            for oy in 0..SSAA {
                for ox in 0..SSAA {
                    let p = hi.get_pixel(dx * SSAA + ox, dy * SSAA + oy).0;
                    sum[0] += p[0] as u32;
                    sum[1] += p[1] as u32;
                    sum[2] += p[2] as u32;
                    sum[3] += p[3] as u32;
                }
            }
            let avg = [
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                (sum[3] / n) as u8,
            ];
            if avg[3] > 0 {
                blend_pixel(canvas, x0 + dx, y0 + dy, avg);
            }
        }
    }
}

/// Aliased arrow draw (no SSAA wrapper). Supports all line styles, head
/// styles, and an optional bezier curve. Called at 4× resolution inside the
/// SSAA sub-image.
#[allow(clippy::too_many_arguments)]
fn draw_arrow_aliased(
    canvas: &mut RgbaImage,
    start: [f32; 2],
    end: [f32; 2],
    color: [u8; 4],
    thickness: f32,
    shadow: bool,
    line_style: ArrowLineStyle,
    head_style: ArrowHeadStyle,
    control: Option<[f32; 2]>,
) {
    // Sample path (2 points for straight, 64 for curves) and compute
    // cumulative arc lengths so we can trim by distance.
    let path = if let Some(c) = control {
        sample_bezier(start, end, c, 64)
    } else {
        vec![start, end]
    };
    let cum = cumulative_lengths(&path);
    let total = *cum.last().unwrap_or(&0.0);
    if total < 1.0 { return; }

    let head_len = (thickness * 4.0).max(14.0).min(total * 0.45);
    let head_half = head_len * 0.55;

    // Trim the shaft's arc-length range so filled/outlined triangles
    // don't bleed through from underneath.
    let end_trim = match head_style {
        ArrowHeadStyle::FilledTriangle
        | ArrowHeadStyle::OutlineTriangle
        | ArrowHeadStyle::DoubleEnded => head_len,
        ArrowHeadStyle::LineOnly | ArrowHeadStyle::None => 0.0,
    };
    let start_trim = if matches!(head_style, ArrowHeadStyle::DoubleEnded) {
        head_len
    } else {
        0.0
    };
    let shaft_lo = start_trim;
    let shaft_hi = total - end_trim;

    // Tangent at path end (for head direction). For straight arrows this is
    // just (end - start) / len; for curves it's 2*(end - control) per the
    // quadratic bezier derivative at t=1.
    let tan_end = path_tangent_end(&path);
    let tan_start = path_tangent_start(&path);

    if shadow {
        let off = (thickness * 0.35).max(2.0);
        let shadow_col: [u8; 4] = [
            (color[0] as f32 * 0.3) as u8,
            (color[1] as f32 * 0.3) as u8,
            (color[2] as f32 * 0.3) as u8,
            170,
        ];
        let shadow_path: Vec<[f32; 2]> =
            path.iter().map(|p| [p[0] + off, p[1] + off]).collect();
        // cum unchanged — offsetting doesn't change arc lengths.
        draw_shaft_path(
            canvas, &shadow_path, &cum, thickness, shadow_col, line_style,
            shaft_lo, shaft_hi,
        );
        draw_head(
            canvas, [end[0] + off, end[1] + off],
            tan_end[0], tan_end[1], -tan_end[1], tan_end[0],
            head_len, head_half, thickness, shadow_col, head_style,
        );
        if matches!(head_style, ArrowHeadStyle::DoubleEnded) {
            draw_head(
                canvas, [start[0] + off, start[1] + off],
                tan_start[0], tan_start[1], -tan_start[1], tan_start[0],
                head_len, head_half, thickness, shadow_col,
                ArrowHeadStyle::FilledTriangle,
            );
        }
    }

    draw_shaft_path(canvas, &path, &cum, thickness, color, line_style, shaft_lo, shaft_hi);
    draw_head(
        canvas, end,
        tan_end[0], tan_end[1], -tan_end[1], tan_end[0],
        head_len, head_half, thickness, color, head_style,
    );
    if matches!(head_style, ArrowHeadStyle::DoubleEnded) {
        draw_head(
            canvas, start,
            tan_start[0], tan_start[1], -tan_start[1], tan_start[0],
            head_len, head_half, thickness, color,
            ArrowHeadStyle::FilledTriangle,
        );
    }
}

/// Cumulative polyline arc lengths. `cum[0] = 0`, `cum[i]` = distance
/// along the polyline from `path[0]` to `path[i]`.
fn cumulative_lengths(path: &[[f32; 2]]) -> Vec<f32> {
    let mut out = vec![0.0; path.len()];
    for i in 1..path.len() {
        let dx = path[i][0] - path[i - 1][0];
        let dy = path[i][1] - path[i - 1][1];
        out[i] = out[i - 1] + (dx * dx + dy * dy).sqrt();
    }
    out
}

/// Unit tangent at the path's final point, pointing forward (into the tip).
fn path_tangent_end(path: &[[f32; 2]]) -> [f32; 2] {
    let n = path.len();
    let a = path[n - 2];
    let b = path[n - 1];
    let d = [b[0] - a[0], b[1] - a[1]];
    let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
    [d[0] / len, d[1] / len]
}

/// Unit tangent at the path's first point, pointing BACKWARD (away from
/// the curve — into the start-side head tip for DoubleEnded).
fn path_tangent_start(path: &[[f32; 2]]) -> [f32; 2] {
    let a = path[0];
    let b = path[1];
    let d = [a[0] - b[0], a[1] - b[1]];
    let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
    [d[0] / len, d[1] / len]
}

/// Stroke a portion of a polyline (arc range `[lo, hi]`) with the given
/// line style. Round caps and dash pattern survive through curves because
/// we walk arc length rather than segments.
#[allow(clippy::too_many_arguments)]
fn draw_shaft_path(
    canvas: &mut RgbaImage,
    path: &[[f32; 2]],
    cum: &[f32],
    thickness: f32,
    color: [u8; 4],
    line_style: ArrowLineStyle,
    lo: f32,
    hi: f32,
) {
    match line_style {
        ArrowLineStyle::Solid => {
            emit_arc_span(canvas, path, cum, lo, hi, thickness, color);
        }
        ArrowLineStyle::Dashed => {
            stroke_dashed_along_path(canvas, path, cum, lo, hi, thickness, color, thickness * 2.0, thickness);
        }
        ArrowLineStyle::Dotted => {
            stroke_dashed_along_path(canvas, path, cum, lo, hi, thickness, color, thickness, thickness * 1.5);
        }
    }
}

/// Dashed stroke along a polyline arc. Walks in (dash + gap) periods;
/// for each period, emits a capsule-stroked arc span of length `dash`.
#[allow(clippy::too_many_arguments)]
fn stroke_dashed_along_path(
    canvas: &mut RgbaImage,
    path: &[[f32; 2]],
    cum: &[f32],
    arc_lo: f32,
    arc_hi: f32,
    thickness: f32,
    color: [u8; 4],
    dash: f32,
    gap: f32,
) {
    let period = (dash + gap).max(0.01);
    let mut t = arc_lo;
    while t < arc_hi {
        let lo = t;
        let hi = (t + dash).min(arc_hi);
        if hi > lo + 0.01 {
            emit_arc_span(canvas, path, cum, lo, hi, thickness, color);
        }
        t += period;
    }
}

/// Stroke every polyline segment that intersects the arc-length span
/// `[lo, hi]` with round-capped capsules. Partial-coverage segments are
/// trimmed to the arc span.
fn emit_arc_span(
    canvas: &mut RgbaImage,
    path: &[[f32; 2]],
    cum: &[f32],
    lo: f32,
    hi: f32,
    thickness: f32,
    color: [u8; 4],
) {
    if hi <= lo + 0.001 || path.len() < 2 { return; }
    let n = path.len();
    // Find first segment index whose end exceeds `lo`.
    let mut i = 1;
    while i < n && cum[i] <= lo { i += 1; }
    if i >= n { return; }

    let mut cur = lo;
    while i < n {
        let seg_end = cum[i];
        let end_pos = seg_end.min(hi);
        let a = interp_on_segment(&path[i - 1], &path[i], cum[i - 1], cum[i], cur);
        let b = interp_on_segment(&path[i - 1], &path[i], cum[i - 1], cum[i], end_pos);
        stroke_segment(canvas, a, b, thickness, color);
        if end_pos >= hi { return; }
        cur = end_pos;
        i += 1;
    }
}

/// Interpolate a point at arc distance `d` within a single polyline
/// segment. `a_cum` / `b_cum` are the segment endpoints' cumulative arc
/// lengths; `d` is expected to lie within `[a_cum, b_cum]`.
fn interp_on_segment(
    a: &[f32; 2], b: &[f32; 2],
    a_cum: f32, b_cum: f32,
    d: f32,
) -> [f32; 2] {
    let seg_len = (b_cum - a_cum).max(1e-6);
    let t = ((d - a_cum) / seg_len).clamp(0.0, 1.0);
    [a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]
}

/// Draw the head for one end of an arrow.
/// `tip` = the outermost point the head converges to.
/// `(ux, uy)` = forward unit vector pointing into the tip (from shaft toward tip).
/// `(px, py)` = perpendicular unit (90° CCW of forward).
#[allow(clippy::too_many_arguments)]
fn draw_head(
    canvas: &mut RgbaImage,
    tip: [f32; 2],
    ux: f32, uy: f32,
    px: f32, py: f32,
    head_len: f32,
    head_half: f32,
    thickness: f32,
    color: [u8; 4],
    head_style: ArrowHeadStyle,
) {
    let base_x = tip[0] - ux * head_len;
    let base_y = tip[1] - uy * head_len;
    let b = [base_x + px * head_half, base_y + py * head_half];
    let c = [base_x - px * head_half, base_y - py * head_half];
    match head_style {
        ArrowHeadStyle::FilledTriangle | ArrowHeadStyle::DoubleEnded => {
            fill_convex_polygon(canvas, &[tip, b, c], color);
        }
        ArrowHeadStyle::OutlineTriangle => {
            let ot = (thickness * 0.7).max(1.5);
            stroke_segment(canvas, tip, b, ot, color);
            stroke_segment(canvas, b, c, ot, color);
            stroke_segment(canvas, c, tip, ot, color);
        }
        ArrowHeadStyle::LineOnly => {
            let lt = thickness.max(1.5);
            stroke_segment(canvas, tip, b, lt, color);
            stroke_segment(canvas, tip, c, lt, color);
        }
        ArrowHeadStyle::None => {}
    }
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

/// Draw a numbered step marker. Rendered through 4× supersampling so the
/// circle edge + outline are smooth rather than pixel-jagged. Same
/// offscreen + downsample pattern as `draw_arrow`.
pub fn draw_step(
    canvas: &mut RgbaImage,
    center: [f32; 2],
    radius: f32,
    number: u32,
    fill: [u8; 4],
    text_color: [u8; 4],
) {
    if radius <= 0.0 { return; }
    let stroke = (radius * 0.08).max(1.5);
    let pad = stroke + 2.0;

    let (cw, ch) = canvas.dimensions();
    let x0 = ((center[0] - radius - pad).floor() as i32).max(0) as u32;
    let y0 = ((center[1] - radius - pad).floor() as i32).max(0) as u32;
    let x1 = ((center[0] + radius + pad).ceil() as i32).min(cw as i32).max(0) as u32;
    let y1 = ((center[1] + radius + pad).ceil() as i32).min(ch as i32).max(0) as u32;
    if x1 <= x0 || y1 <= y0 { return; }

    let sub_w = x1 - x0;
    let sub_h = y1 - y0;
    const SSAA: u32 = 4;
    let mut hi = RgbaImage::new(sub_w * SSAA, sub_h * SSAA);
    let s = SSAA as f32;
    let shifted_center = [
        (center[0] - x0 as f32) * s,
        (center[1] - y0 as f32) * s,
    ];
    draw_step_aliased(
        &mut hi,
        shifted_center,
        radius * s,
        number,
        fill,
        text_color,
    );

    let n = (SSAA * SSAA) as u32;
    for dy in 0..sub_h {
        for dx in 0..sub_w {
            let mut sum: [u32; 4] = [0, 0, 0, 0];
            for oy in 0..SSAA {
                for ox in 0..SSAA {
                    let p = hi.get_pixel(dx * SSAA + ox, dy * SSAA + oy).0;
                    sum[0] += p[0] as u32;
                    sum[1] += p[1] as u32;
                    sum[2] += p[2] as u32;
                    sum[3] += p[3] as u32;
                }
            }
            let avg = [
                (sum[0] / n) as u8,
                (sum[1] / n) as u8,
                (sum[2] / n) as u8,
                (sum[3] / n) as u8,
            ];
            if avg[3] > 0 {
                blend_pixel(canvas, x0 + dx, y0 + dy, avg);
            }
        }
    }
}

/// Aliased step draw (called at 4× resolution inside the SSAA sub-image).
fn draw_step_aliased(
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

/// Rasterize wrapping `text` into `canvas` starting at the top-left of
/// `rect` in image-pixel space. The wrap width is `rect.width()`; rect
/// height is advisory (defines the *minimum* extent) — text that
/// overflows vertically is still drawn (`blend_pixel` bounds-checks).
///
/// Wrapping rules: explicit `\n` starts a new line; otherwise we greedily
/// accumulate whitespace-delimited words and break when the next word
/// would exceed the right edge. If a single word is wider than the box,
/// it is hard-broken per glyph to avoid infinite loops.
#[allow(clippy::too_many_arguments)]
pub fn draw_text_box(
    canvas: &mut RgbaImage,
    font: &FontArc,
    rect: [f32; 4],
    text: &str,
    size_px: f32,
    color: [u8; 4],
    align: TextAlign,
    list: TextListStyle,
) {
    let r = normalise(rect);
    let x0 = r[0];
    let y0 = r[1];
    let box_width = (r[2] - r[0]).max(0.0);

    let scale = PxScale::from(size_px.max(6.0));
    let scaled = font.as_scaled(scale);
    let line_height = scaled.height() + scaled.line_gap();
    let ascent = scaled.ascent();

    // Render a run of glyphs on `baseline_y` starting at image-pixel
    // `start_x`. Returns nothing — for measuring, use `run_width` instead.
    let render_line = |canvas: &mut RgbaImage, line: &str, baseline_y: f32, start_x: f32| {
        let mut cursor_x = start_x;
        let mut prev_glyph: Option<ab_glyph::GlyphId> = None;
        for ch in line.chars() {
            if ch == '\r' {
                continue;
            }
            let glyph_id = font.glyph_id(ch);
            if let Some(prev) = prev_glyph {
                cursor_x += scaled.kern(prev, glyph_id);
            }
            let glyph = glyph_id.with_scale_and_position(
                scale,
                ab_glyph::point(cursor_x, baseline_y),
            );
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
                    let a = ((color[3] as f32) * coverage.clamp(0.0, 1.0)).round() as u8;
                    blend_pixel(canvas, x as u32, y as u32, [color[0], color[1], color[2], a]);
                });
            }
            cursor_x += scaled.h_advance(glyph_id);
            prev_glyph = Some(glyph_id);
        }
    };

    // Measure the x-advance of a run of glyphs (kerning included).
    let run_width = |run: &str| -> f32 {
        let mut w = 0.0f32;
        let mut prev: Option<ab_glyph::GlyphId> = None;
        for ch in run.chars() {
            if ch == '\r' { continue; }
            let g = font.glyph_id(ch);
            if let Some(p) = prev {
                w += scaled.kern(p, g);
            }
            w += scaled.h_advance(g);
            prev = Some(g);
        }
        w
    };

    let mut baseline = y0 + ascent;
    // Consecutive-non-empty paragraph counter for Numbered lists. Reset
    // per annotation; an empty paragraph does not consume a number.
    let mut number: u32 = 0;

    // Iterate paragraphs (explicit `\n`). Empty paragraphs still consume a line.
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            // Empty paragraph: advance one line, DON'T emit a marker, and
            // DON'T increment the numbered counter.
            baseline += line_height;
            continue;
        }

        // Build the marker string for this paragraph (if any).
        let marker: String = match list {
            TextListStyle::None => String::new(),
            TextListStyle::Bullet => "\u{2022} ".to_string(),
            TextListStyle::Numbered => {
                number += 1;
                format!("{}. ", number)
            }
        };
        let marker_width = run_width(&marker);
        // Effective body wrap width after reserving room for the marker
        // (and its hanging indent on continuation lines).
        let wrap_width = (box_width - marker_width).max(0.0);

        // Horizontal offset for a single visual body line after trimming
        // trailing whitespace so Center/Right don't over-shift lines that
        // ended with a space. Applied WITHIN the body column — the marker
        // column has already been reserved at `x0` by shifting the body
        // start by `marker_width`.
        let align_offset = |line: &str| -> f32 {
            match align {
                TextAlign::Left => 0.0,
                TextAlign::Center => {
                    let w = run_width(line.trim_end());
                    ((wrap_width - w) * 0.5).max(0.0)
                }
                TextAlign::Right => {
                    let w = run_width(line.trim_end());
                    (wrap_width - w).max(0.0)
                }
            }
        };

        // `first_line_baseline` is the baseline at which the marker for
        // this paragraph is drawn (only on the first visual line).
        let first_line_baseline = baseline;
        let mut emitted_first_line = false;
        let emit_line = |canvas: &mut RgbaImage, line: &str, baseline_y: f32| {
            let off = align_offset(line);
            // Body always starts at x0 + marker_width (the hanging indent),
            // then we shift by the alignment offset within the body column.
            render_line(canvas, line, baseline_y, x0 + marker_width + off);
        };

        // Build visual lines by greedy whitespace wrapping, against the
        // *body* wrap width (box_width − marker_width).
        let mut current = String::new();
        let mut remaining: &str = paragraph;
        while !remaining.is_empty() {
            let mut chars = remaining.char_indices();
            let mut start = 0usize;
            if current.is_empty() {
                while let Some((i, c)) = chars.clone().next() {
                    if c.is_whitespace() {
                        chars.next();
                        start = i + c.len_utf8();
                    } else {
                        break;
                    }
                }
            }
            let after = &remaining[start..];
            let word_end_rel = after
                .char_indices()
                .find(|(_, c)| c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(after.len());
            let word = &after[..word_end_rel];
            let post_word = &after[word_end_rel..];
            let ws_end_rel = post_word
                .char_indices()
                .find(|(_, c)| !c.is_whitespace())
                .map(|(i, _)| i)
                .unwrap_or(post_word.len());
            let trailing_ws = &post_word[..ws_end_rel];

            if word.is_empty() && trailing_ws.is_empty() {
                break;
            }

            if word.is_empty() {
                if !current.is_empty() {
                    emit_line(canvas, &current, baseline);
                    emitted_first_line = true;
                    baseline += line_height;
                    current.clear();
                }
                break;
            }

            let candidate_width = if current.is_empty() {
                run_width(word)
            } else {
                run_width(&current) + run_width(word)
            };

            if candidate_width <= wrap_width || current.is_empty() && run_width(word) <= wrap_width {
                current.push_str(word);
                current.push_str(trailing_ws);
                remaining = &remaining[start + word_end_rel + ws_end_rel..];
            } else if current.is_empty() {
                let mut used = 0usize;
                let mut width = 0.0f32;
                let mut prev: Option<ab_glyph::GlyphId> = None;
                for (idx, c) in word.char_indices() {
                    let g = font.glyph_id(c);
                    let mut adv = scaled.h_advance(g);
                    if let Some(p) = prev {
                        adv += scaled.kern(p, g);
                    }
                    if width + adv > wrap_width && used > 0 {
                        break;
                    }
                    width += adv;
                    used = idx + c.len_utf8();
                    prev = Some(g);
                }
                if used == 0 {
                    if let Some(c) = word.chars().next() {
                        used = c.len_utf8();
                    }
                }
                current.push_str(&word[..used]);
                emit_line(canvas, &current, baseline);
                emitted_first_line = true;
                baseline += line_height;
                current.clear();
                remaining = &remaining[start + used..];
            } else {
                let trimmed = current.trim_end().to_string();
                emit_line(canvas, &trimmed, baseline);
                emitted_first_line = true;
                baseline += line_height;
                current.clear();
            }
        }

        if !current.is_empty() {
            let trimmed = current.trim_end().to_string();
            emit_line(canvas, &trimmed, baseline);
            emitted_first_line = true;
            baseline += line_height;
        }

        // Draw the marker on the first visual line's baseline — ONLY if the
        // paragraph actually produced at least one visual line. (If the
        // paragraph was all whitespace and the loop above broke before
        // emitting anything, don't draw a stray bullet.)
        if !marker.is_empty() && emitted_first_line {
            render_line(canvas, &marker, first_line_baseline, x0);
        }
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
// Text effects: frosted backdrop + drop shadow (per-annotation, feature)
// ───────────────────────────────────────────────────────────────────────────

/// Drop-shadow offset in image pixels. Fixed defaults tuned to match the
/// preview's look (`paint_text_effects_live`).
const TEXT_SHADOW_OFFSET: [f32; 2] = [3.0, 4.0];
/// Gaussian sigma for the drop shadow (image pixels).
const TEXT_SHADOW_SIGMA: f32 = 8.0;
/// Shadow colour (sRGB RGBA).
const TEXT_SHADOW_COLOR: [u8; 4] = [0, 0, 0, 110];
/// Gaussian sigma for the frosted-glass backdrop (image pixels).
const TEXT_FROSTED_SIGMA: f32 = 10.0;

/// Render a soft dark drop shadow behind the Text rect. This is a
/// rect-level shadow — NOT a per-glyph blur — matching the live preview.
pub fn draw_text_shadow(canvas: &mut RgbaImage, rect: [f32; 4]) {
    let r = normalise(rect);
    let rw = (r[2] - r[0]).ceil() as i32;
    let rh = (r[3] - r[1]).ceil() as i32;
    if rw <= 0 || rh <= 0 {
        return;
    }

    // Work in an offscreen buffer padded by ~3σ on every side so the
    // gaussian has room to bleed without clipping.
    let pad = (TEXT_SHADOW_SIGMA * 3.0).ceil() as i32;
    let buf_w = (rw + pad * 2).max(1) as u32;
    let buf_h = (rh + pad * 2).max(1) as u32;
    let mut buf = RgbaImage::from_pixel(buf_w, buf_h, Rgba([0, 0, 0, 0]));
    // Stamp the solid shadow rect at `pad,pad` (inside the padded buffer).
    for y in 0..rh {
        for x in 0..rw {
            buf.put_pixel(
                (x + pad) as u32,
                (y + pad) as u32,
                Rgba(TEXT_SHADOW_COLOR),
            );
        }
    }
    let blurred = imageproc::filter::gaussian_blur_f32(&buf, TEXT_SHADOW_SIGMA);

    // Blit with offset: buffer origin lands at rect.min - pad + shadow_offset.
    let ox = (r[0] - pad as f32 + TEXT_SHADOW_OFFSET[0]).round() as i32;
    let oy = (r[1] - pad as f32 + TEXT_SHADOW_OFFSET[1]).round() as i32;
    for y in 0..buf_h {
        for x in 0..buf_w {
            let p = blurred.get_pixel(x, y).0;
            if p[3] == 0 {
                continue;
            }
            let dx = ox + x as i32;
            let dy = oy + y as i32;
            if dx < 0 || dy < 0 {
                continue;
            }
            blend_pixel(canvas, dx as u32, dy as u32, p);
        }
    }
}

/// Render a frosted-glass backdrop behind the Text rect: gaussian-blur the
/// base pixels inside the rect, then overlay a translucent white tint.
/// `base` MUST be the untouched base image, matching the Blur pattern —
/// sampling from the already-flattened `canvas` would leak earlier
/// annotations through the frost.
pub fn draw_text_frosted(canvas: &mut RgbaImage, base: &RgbaImage, rect: [f32; 4]) {
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
            crop.put_pixel(x, y, *base.get_pixel(x0 + x, y0 + y));
        }
    }
    let blurred = imageproc::filter::gaussian_blur_f32(&crop, TEXT_FROSTED_SIGMA);

    // Paint the blurred crop straight onto the canvas — no tint. Matches
    // the Blur tool's look so a frosted Text rect reads as "that area is
    // blurred" without the washed-out glass hue.
    for y in 0..h {
        for x in 0..w {
            let p = blurred.get_pixel(x, y).0;
            blend_pixel(canvas, x0 + x, y0 + y, p);
        }
    }
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
    let jitter = |n: u32, salt: u32| -> f32 {
        let mut s = n.wrapping_mul(2654435761).wrapping_add(salt.wrapping_mul(40503));
        s ^= s >> 16;
        s = s.wrapping_mul(0x85ebca6b);
        let u = (s & 0xFFFF) as f32 / 65536.0; // 0..1
        (u - 0.5) * 0.6 // +/- 0.3
    };

    for edge in effect.active_edges() {
        // Salting by edge so two adjacent tears don't share the same
        // jitter pattern — otherwise corners look suspiciously symmetric.
        let salt = match edge {
            Edge::Top => 1,
            Edge::Bottom => 2,
            Edge::Left => 3,
            Edge::Right => 4,
        };
        match edge {
            Edge::Top | Edge::Bottom => {
                for x in 0..w {
                    let idx = x as f32 / teeth;
                    let tooth_n = idx.floor() as u32;
                    let frac = idx - idx.floor();
                    let tri = 1.0 - (2.0 * frac - 1.0).abs();
                    let j = 1.0 + jitter(tooth_n, salt);
                    let depth_here = depth * tri * j;
                    let d = depth_here.max(0.0) as u32;
                    for y in 0..d.min(h) {
                        let yy = match edge {
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
                    let j = 1.0 + jitter(tooth_n, salt);
                    let depth_here = depth * tri * j;
                    let d = depth_here.max(0.0) as u32;
                    for x in 0..d.min(w) {
                        let xx = match edge {
                            Edge::Left => x,
                            Edge::Right => w - 1 - x,
                            _ => unreachable!(),
                        };
                        out.put_pixel(xx, y, Rgba([0, 0, 0, 0]));
                    }
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
    let mw = border.matte_width.max(0.0).round() as u32;
    let frame = bw + mw; // total frame thickness outside the image.
    let shadow_r = border.shadow_radius.max(0.0).round() as u32;
    let sox = border.shadow_offset[0].round() as i32;
    let soy = border.shadow_offset[1].round() as i32;

    // Total margins on each side: frame (outer + matte) + shadow spread
    // plus whichever-direction-positive offset.
    let pad_l = frame + shadow_r + (-sox).max(0) as u32;
    let pad_r = frame + shadow_r + sox.max(0) as u32;
    let pad_t = frame + shadow_r + (-soy).max(0) as u32;
    let pad_b = frame + shadow_r + soy.max(0) as u32;

    let new_w = w + pad_l + pad_r;
    let new_h = h + pad_t + pad_b;
    let mut out = RgbaImage::from_pixel(new_w, new_h, Rgba([0, 0, 0, 0]));

    let content_x = pad_l as i32;
    let content_y = pad_t as i32;

    // Draw drop shadow first (underneath the image + frame).
    if shadow_r > 0 || border.shadow_color[3] > 0 && (sox != 0 || soy != 0) {
        let shadow_rect_x = content_x - frame as i32 + sox;
        let shadow_rect_y = content_y - frame as i32 + soy;
        let shadow_rect_w = w + 2 * frame;
        let shadow_rect_h = h + 2 * frame;
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

    // Outer border band — fills the entire frame area (bw + mw). The
    // matte pass below overwrites the inner strip to produce the
    // two-band photo-frame look.
    if frame > 0 && border.color[3] > 0 {
        let bx0 = (content_x - frame as i32).max(0) as u32;
        let by0 = (content_y - frame as i32).max(0) as u32;
        let bx1 = ((content_x + w as i32 + frame as i32).min(new_w as i32)).max(0) as u32;
        let by1 = ((content_y + h as i32 + frame as i32).min(new_h as i32)).max(0) as u32;
        for y in by0..by1 {
            for x in bx0..bx1 {
                blend_pixel(&mut out, x, y, border.color);
            }
        }
    }

    // Matte band — the inner `mw`-wide ring hugging the image.
    if mw > 0 && border.matte_color[3] > 0 {
        let mx0 = (content_x - mw as i32).max(0) as u32;
        let my0 = (content_y - mw as i32).max(0) as u32;
        let mx1 = ((content_x + w as i32 + mw as i32).min(new_w as i32)).max(0) as u32;
        let my1 = ((content_y + h as i32 + mw as i32).min(new_h as i32)).max(0) as u32;
        for y in my0..my1 {
            for x in mx0..mx1 {
                blend_pixel(&mut out, x, y, border.matte_color);
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
            top: false,
            bottom: true,
            left: false,
            right: false,
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
    fn draw_text_box_renders_and_wraps() {
        // Text with an explicit newline + a long enough second line to
        // force a wrap on a narrow box. We just assert that at least one
        // pixel below the first baseline has been drawn — i.e. the text
        // wrapped / newlined into a second line.
        let mut canvas = RgbaImage::from_pixel(120, 120, Rgba([255, 255, 255, 255]));
        draw_text_box(
            &mut canvas,
            font(),
            [2.0, 2.0, 60.0, 60.0],
            "hi\nthis is a long sentence that wraps",
            14.0,
            [0, 0, 0, 255],
            TextAlign::Left,
            TextListStyle::None,
        );
        // Scan a band well below the first line for any non-white pixels.
        let mut any_second_line = false;
        for y in 28..56 {
            for x in 0..60 {
                let p = canvas.get_pixel(x, y).0;
                if p[0] < 250 && p[1] < 250 && p[2] < 250 {
                    any_second_line = true;
                    break;
                }
            }
            if any_second_line { break; }
        }
        assert!(any_second_line, "expected wrapped/newlined text on a second line");
    }

    #[test]
    fn draw_text_box_center_aligns_lines() {
        // A short single-line string inside a wide rect should produce
        // glyphs only in the middle band of the box when rendered with
        // TextAlign::Center. We scan the leftmost quarter for any
        // non-white pixel and assert there are none — that's the whole
        // "centering shifted the glyphs past x0" check, without needing
        // to know exact glyph advances.
        let mut canvas = RgbaImage::from_pixel(200, 40, Rgba([255, 255, 255, 255]));
        draw_text_box(
            &mut canvas,
            font(),
            [0.0, 0.0, 160.0, 40.0],
            "hi",
            14.0,
            [0, 0, 0, 255],
            TextAlign::Center,
            TextListStyle::None,
        );
        // Left quarter: from x=0 to x=40. Nothing should be drawn here.
        let mut any_left = false;
        for y in 0..40 {
            for x in 0..40 {
                let p = canvas.get_pixel(x, y).0;
                if p[0] < 250 && p[1] < 250 && p[2] < 250 {
                    any_left = true;
                    break;
                }
            }
            if any_left { break; }
        }
        assert!(
            !any_left,
            "center-aligned short line should not land in the left quarter",
        );
        // And, to confirm the text DID render somewhere, scan the middle
        // band for non-white pixels.
        let mut any_middle = false;
        for y in 0..40 {
            for x in 60..100 {
                let p = canvas.get_pixel(x, y).0;
                if p[0] < 250 && p[1] < 250 && p[2] < 250 {
                    any_middle = true;
                    break;
                }
            }
            if any_middle { break; }
        }
        assert!(any_middle, "expected centered glyphs in the middle band");
    }

    /// Helper: does any row of `canvas` within `y` range `[y0, y1)` have a
    /// non-white pixel in the leftmost band `[x0, x1)`? We use this to
    /// prove list markers render in the leftmost column.
    fn any_dark_in_band(
        canvas: &RgbaImage, x0: u32, x1: u32, y0: u32, y1: u32,
    ) -> bool {
        for y in y0..y1 {
            for x in x0..x1 {
                let p = canvas.get_pixel(x, y).0;
                if p[0] < 240 && p[1] < 240 && p[2] < 240 {
                    return true;
                }
            }
        }
        false
    }

    /// Bullets: two non-empty paragraphs each get a `\u{2022} ` prefix in
    /// the leftmost column; an empty paragraph between them does NOT.
    #[test]
    fn draw_text_box_bullet_prefixes_paragraphs() {
        let mut canvas = RgbaImage::from_pixel(160, 120, Rgba([255, 255, 255, 255]));
        draw_text_box(
            &mut canvas,
            font(),
            [0.0, 0.0, 160.0, 120.0],
            "first\n\nsecond",
            16.0,
            [0, 0, 0, 255],
            TextAlign::Left,
            TextListStyle::Bullet,
        );
        // First paragraph baseline lives roughly in [0, 24); second in
        // [40, 64) since the empty paragraph ate one line. Check the
        // leftmost 10px for inked marker pixels in both bands. The exact
        // marker-width is font-dependent; scanning 0..10 is conservative
        // enough to catch the bullet's left edge.
        assert!(
            any_dark_in_band(&canvas, 0, 10, 0, 24),
            "first paragraph should have a bullet in the leftmost column",
        );
        assert!(
            any_dark_in_band(&canvas, 0, 10, 40, 70),
            "second paragraph (after an empty paragraph) should also have a bullet",
        );
    }

    /// Numbered: counter ignores empty paragraphs. With `"a\n\nb"`, both
    /// non-empty paragraphs get numeric markers starting at 1.
    #[test]
    fn draw_text_box_numbered_counts_paragraphs() {
        let mut canvas = RgbaImage::from_pixel(160, 120, Rgba([255, 255, 255, 255]));
        draw_text_box(
            &mut canvas,
            font(),
            [0.0, 0.0, 160.0, 120.0],
            "a\n\nb",
            16.0,
            [0, 0, 0, 255],
            TextAlign::Left,
            TextListStyle::Numbered,
        );
        // Leftmost column should contain numeric glyph ink on both the
        // first paragraph and the second — one is `1. `, the other `2. `.
        assert!(
            any_dark_in_band(&canvas, 0, 10, 0, 24),
            "first numbered paragraph should have a marker in the leftmost column",
        );
        assert!(
            any_dark_in_band(&canvas, 0, 10, 40, 70),
            "second numbered paragraph should have a marker in the leftmost column",
        );
    }

    #[test]
    fn draw_text_shadow_paints_dark_pixels() {
        let mut canvas = RgbaImage::from_pixel(64, 64, Rgba([255, 255, 255, 255]));
        draw_text_shadow(&mut canvas, [10.0, 10.0, 40.0, 30.0]);
        // Some pixel near the shadow rect should be visibly darker.
        let mut any_dark = false;
        for y in 10..40 {
            for x in 10..50 {
                let p = canvas.get_pixel(x, y).0;
                if p[0] < 240 {
                    any_dark = true;
                    break;
                }
            }
            if any_dark { break; }
        }
        assert!(any_dark, "shadow should darken some pixels under the rect");
    }

    #[test]
    fn draw_text_frosted_preserves_base_hue() {
        // Pure-blur backdrop: solid red base should stay red (blur of a
        // uniform field is a no-op). This matches the Blur tool's look.
        let base = RgbaImage::from_pixel(32, 32, Rgba([255, 0, 0, 255]));
        let mut canvas = base.clone();
        draw_text_frosted(&mut canvas, &base, [4.0, 4.0, 28.0, 28.0]);
        let p = canvas.get_pixel(16, 16).0;
        assert!(p[0] > 240, "red should stay dominant");
        assert!(p[1] < 15, "green should stay near zero (no tint)");
        assert!(p[2] < 15, "blue should stay near zero (no tint)");
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
            matte_width: 0.0,
            matte_color: [245, 240, 230, 255],
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
