//! Selection + transform helpers shared across tools.
//!
//! What's here:
//! - `SelectionTarget` — identifies what is selected (an annotation id or
//!   the cursor layer).
//! - `bounds_of_node` / `bounds_of_cursor` — axis-aligned rects in image-
//!   pixel coordinates, used for hit-testing and handle placement.
//! - `handle_positions` — the 8 resize handles + the body-drag anchor (plus
//!   special-case handles for Callout tail and Magnify source).
//! - `apply_handle_drag` — compute a new rect given a starting rect, which
//!   handle was grabbed, and the cursor delta in image pixels.

use crate::editor::document::{AnnotationNode, SerializedCursor};
use uuid::Uuid;

/// What is currently selected in the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionTarget {
    Annotation(Uuid),
    Cursor,
}

/// Axis-aligned bounding rect in image-pixel coords: `[min_x, min_y, max_x, max_y]`.
pub type BBox = [f32; 4];

/// A draggable handle on a selected annotation.
///
/// `Body` is the whole-rect drag. The 8 resize handles are named by compass
/// direction. `CalloutTail` is the callout balloon's tail tip. `MagnifySource`
/// grabs the source rect of a `Magnify` node (center drag only — we don't
/// resize the source from the UI to keep the UX simple).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handle {
    Body,
    N, S, E, W,
    NE, NW, SE, SW,
    CalloutTail,
    MagnifySource,
    /// Arrow endpoints — arrows aren't rect-shaped.
    ArrowStart,
    ArrowEnd,
    /// Arrow mid-control handle. Dragging it bends a straight arrow into a
    /// quadratic bezier; dragging back near the line midpoint straightens.
    ArrowMid,
}

/// Compute the bounding rect for an annotation, or `None` if the node has
/// no natural rect (text extent depends on font metrics; for simplicity we
/// approximate text as an `size_px`-tall block starting at `position`).
pub fn bounds_of_node(node: &AnnotationNode) -> Option<BBox> {
    match node {
        AnnotationNode::Arrow { start, end, control, .. } => {
            // Include the bezier control point when present — the curve is
            // bounded by the convex hull of {start, control, end}.
            let (mut minx, mut miny) = (start[0].min(end[0]), start[1].min(end[1]));
            let (mut maxx, mut maxy) = (start[0].max(end[0]), start[1].max(end[1]));
            if let Some(c) = control {
                minx = minx.min(c[0]);
                miny = miny.min(c[1]);
                maxx = maxx.max(c[0]);
                maxy = maxy.max(c[1]);
            }
            Some([minx, miny, maxx, maxy])
        }
        AnnotationNode::Text { rect, .. } => {
            // Text is now a drag-created box — the stored rect IS the
            // bounds. Overflowing text past the bottom still renders but
            // selection/handles track the declared rect.
            Some(*rect)
        }
        AnnotationNode::Callout { rect, .. } => Some(*rect),
        AnnotationNode::Shape { rect, .. } => Some(*rect),
        AnnotationNode::Step { center, radius, .. } => {
            Some([center[0] - radius, center[1] - radius,
                  center[0] + radius, center[1] + radius])
        }
        AnnotationNode::Stamp { rect, .. } => Some(*rect),
        AnnotationNode::Magnify { target_rect, .. } => Some(*target_rect),
        AnnotationNode::Blur { rect, .. } => Some(*rect),
        // CaptureInfo's on-canvas extent depends on the runtime banner
        // dimensions. Returning None excludes it from the standard rect
        // handle flow; the editor treats it as a position-only node.
        AnnotationNode::CaptureInfo { .. } => None,
    }
}

pub fn bounds_of_cursor(c: &SerializedCursor) -> BBox {
    [
        c.x as f32,
        c.y as f32,
        (c.x + c.width as i32) as f32,
        (c.y + c.height as i32) as f32,
    ]
}

/// Normalise a bbox so `min < max`.
pub fn normalise(bbox: BBox) -> BBox {
    [
        bbox[0].min(bbox[2]),
        bbox[1].min(bbox[3]),
        bbox[0].max(bbox[2]),
        bbox[1].max(bbox[3]),
    ]
}

/// Eight handle positions + body center for a bbox, in image-pixel coords.
pub fn rect_handles(bbox: BBox) -> [(Handle, [f32; 2]); 8] {
    let [x0, y0, x1, y1] = bbox;
    let mx = 0.5 * (x0 + x1);
    let my = 0.5 * (y0 + y1);
    [
        (Handle::NW, [x0, y0]),
        (Handle::N,  [mx, y0]),
        (Handle::NE, [x1, y0]),
        (Handle::W,  [x0, my]),
        (Handle::E,  [x1, my]),
        (Handle::SW, [x0, y1]),
        (Handle::S,  [mx, y1]),
        (Handle::SE, [x1, y1]),
    ]
}

/// Given a starting rect and a handle drag from `(dx, dy)` in image pixels,
/// return the new rect. Body dragging moves both corners; the N/S/E/W
/// handles move one edge; corners move two edges.
pub fn drag_rect(bbox: BBox, handle: Handle, dx: f32, dy: f32) -> BBox {
    let [mut x0, mut y0, mut x1, mut y1] = bbox;
    match handle {
        Handle::Body => { x0 += dx; y0 += dy; x1 += dx; y1 += dy; }
        Handle::N  => { y0 += dy; }
        Handle::S  => { y1 += dy; }
        Handle::E  => { x1 += dx; }
        Handle::W  => { x0 += dx; }
        Handle::NE => { x1 += dx; y0 += dy; }
        Handle::NW => { x0 += dx; y0 += dy; }
        Handle::SE => { x1 += dx; y1 += dy; }
        Handle::SW => { x0 += dx; y1 += dy; }
        _ => {}
    }
    [x0, y0, x1, y1]
}

/// Point-in-bbox hit test. Bbox need not be normalised.
pub fn hit_bbox(p: [f32; 2], bbox: BBox) -> bool {
    let n = normalise(bbox);
    p[0] >= n[0] && p[0] <= n[2] && p[1] >= n[1] && p[1] <= n[3]
}

/// Sample a quadratic bezier from `start` through `control` to `end` into
/// `steps + 1` polyline points. Shared by rasterize, preview, and hit-test.
pub fn sample_bezier(
    start: [f32; 2],
    end: [f32; 2],
    control: [f32; 2],
    steps: usize,
) -> Vec<[f32; 2]> {
    let steps = steps.max(2);
    let mut out = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let u = 1.0 - t;
        out.push([
            u * u * start[0] + 2.0 * u * t * control[0] + t * t * end[0],
            u * u * start[1] + 2.0 * u * t * control[1] + t * t * end[1],
        ]);
    }
    out
}

/// Point-to-segment squared distance. Used for Arrow hit-testing.
pub fn dist2_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::EPSILON {
        let ex = p[0] - a[0];
        let ey = p[1] - a[1];
        return ex * ex + ey * ey;
    }
    let mut t = ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2;
    t = t.clamp(0.0, 1.0);
    let qx = a[0] + t * dx;
    let qy = a[1] + t * dy;
    let ex = p[0] - qx;
    let ey = p[1] - qy;
    ex * ex + ey * ey
}
