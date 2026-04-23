//! Arrow tool helpers.

use crate::editor::document::{AnnotationNode, ArrowHeadStyle, ArrowLineStyle};
use uuid::Uuid;

#[allow(clippy::too_many_arguments)] // Honest signature — every field seeds a node property.
pub fn make(
    start: [f32; 2],
    end: [f32; 2],
    color: [u8; 4],
    thickness: f32,
    shadow: bool,
    line_style: ArrowLineStyle,
    head_style: ArrowHeadStyle,
) -> AnnotationNode {
    AnnotationNode::Arrow {
        id: Uuid::new_v4(),
        start,
        end,
        color,
        thickness,
        shadow,
        line_style,
        head_style,
        control: None,
    }
}
