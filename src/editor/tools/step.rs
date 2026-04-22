//! Step (numbered marker) tool helpers.

use crate::editor::document::{AnnotationNode, Document};
use uuid::Uuid;

/// Next step number for the document. We use `max existing step + 1` rather
/// than a counter field on `Document` so undo/redo "just works" (no counter
/// to rewind) and imported/edited docs never duplicate numbers.
pub fn next_number(doc: &Document) -> u32 {
    let mut max = 0u32;
    for n in &doc.annotations {
        if let AnnotationNode::Step { number, .. } = n {
            if *number > max { max = *number; }
        }
    }
    max + 1
}

pub fn make(
    center: [f32; 2],
    radius: f32,
    number: u32,
    fill: [u8; 4],
    text_color: [u8; 4],
) -> AnnotationNode {
    AnnotationNode::Step {
        id: Uuid::new_v4(),
        center,
        radius,
        number,
        fill,
        text_color,
    }
}
