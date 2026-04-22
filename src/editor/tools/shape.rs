//! Shape (rect / ellipse) tool helpers.

use crate::editor::document::{AnnotationNode, ShapeKind};
use uuid::Uuid;

pub fn make(
    shape: ShapeKind,
    rect: [f32; 4],
    stroke: [u8; 4],
    stroke_width: f32,
    fill: [u8; 4],
) -> AnnotationNode {
    AnnotationNode::Shape {
        id: Uuid::new_v4(),
        shape,
        rect,
        stroke,
        stroke_width,
        fill,
    }
}
