//! Magnify (loupe) tool helpers.

use crate::editor::document::AnnotationNode;
use uuid::Uuid;

/// Build a magnifier. The user drags the SOURCE rect (what to zoom) and
/// the TARGET (zoom bubble) is derived as a 3× scaled callout placed next
/// to the source by `default_target_for_source`.
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

/// Default 3× zoom target rect placed beside the source drag. Prefers
/// below-and-right; falls back to above-and-left when that would clip
/// outside the base image.
pub fn default_target_for_source(source: [f32; 4], base_w: u32, base_h: u32) -> [f32; 4] {
    let sx0 = source[0].min(source[2]);
    let sy0 = source[1].min(source[3]);
    let sx1 = source[0].max(source[2]);
    let sy1 = source[1].max(source[3]);
    let sw = (sx1 - sx0).max(1.0);
    let sh = (sy1 - sy0).max(1.0);
    let tw = sw * 3.0;
    let th = sh * 3.0;
    let gap = 12.0;
    let bw = base_w as f32;
    let bh = base_h as f32;

    let mut tx = sx1 + gap;
    if tx + tw > bw {
        tx = (sx0 - gap - tw).max(0.0);
    }
    let mut ty = sy1 + gap;
    if ty + th > bh {
        ty = (sy0 - gap - th).max(0.0);
    }
    [tx, ty, tx + tw, ty + th]
}
