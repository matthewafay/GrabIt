//! Arrow tool helpers.

use crate::editor::document::AnnotationNode;
use uuid::Uuid;

pub fn make(start: [f32; 2], end: [f32; 2], color: [u8; 4], thickness: f32) -> AnnotationNode {
    AnnotationNode::Arrow {
        id: Uuid::new_v4(),
        start,
        end,
        color,
        thickness,
    }
}
