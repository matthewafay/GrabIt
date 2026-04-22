//! Text tool helpers.

use crate::editor::document::AnnotationNode;
use uuid::Uuid;

pub fn make(position: [f32; 2], text: String, color: [u8; 4], size_px: f32) -> AnnotationNode {
    AnnotationNode::Text {
        id: Uuid::new_v4(),
        position,
        text,
        color,
        size_px,
    }
}
