//! Magnify (loupe) tool helpers.

use crate::editor::document::AnnotationNode;
use uuid::Uuid;

/// Build a magnifier from a drag rectangle. The drag defines the TARGET
/// rect (where the zoom shows up); a default source rect is picked as the
/// target rect scaled down by 3x, centred on the same point. The user can
/// drag the source handle to move the sampled region afterwards.
pub fn make(
    target_rect: [f32; 4],
    source_rect: [f32; 4],
    border: [u8; 4],
    border_width: f32,
    circular: bool,
) -> AnnotationNode {
    AnnotationNode::Magnify {
        id: Uuid::new_v4(),
        source_rect,
        target_rect,
        border,
        border_width,
        circular,
    }
}

/// Default 3x zoom source rect placed near (but not inside) the target.
pub fn default_source_for_target(target: [f32; 4]) -> [f32; 4] {
    let w = (target[2] - target[0]).abs();
    let h = (target[3] - target[1]).abs();
    let sw = w / 3.0;
    let sh = h / 3.0;
    // Place the source above-and-left of the target so the two rects don't
    // overlap when the user lets go. Clamp so at least something is visible
    // for captures where the drag happened near the top-left corner.
    let sx = (target[0] - sw - 8.0).max(0.0);
    let sy = (target[1] - sh - 8.0).max(0.0);
    [sx, sy, sx + sw, sy + sh]
}
