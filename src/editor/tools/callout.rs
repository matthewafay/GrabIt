//! Callout (speech-balloon) tool helpers.

use crate::editor::document::AnnotationNode;
use uuid::Uuid;

/// Build a callout. The tail defaults to a position offset down-and-left of
/// the balloon so the pointer is visible immediately; the user can drag it
/// afterwards.
pub fn make(
    rect: [f32; 4],
    text: String,
    fill: [u8; 4],
    stroke: [u8; 4],
    stroke_width: f32,
    text_color: [u8; 4],
    text_size: f32,
) -> AnnotationNode {
    let tail = [
        rect[0] + 0.2 * (rect[2] - rect[0]),
        rect[3] + 0.35 * (rect[3] - rect[1]).max(40.0),
    ];
    AnnotationNode::Callout {
        id: Uuid::new_v4(),
        rect,
        tail,
        text,
        fill,
        stroke,
        stroke_width,
        text_color,
        text_size,
    }
}
