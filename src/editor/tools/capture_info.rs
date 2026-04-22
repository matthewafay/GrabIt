//! Capture-info stamp tool — places a banner showing capture metadata.
//!
//! Unlike the other drag-to-create tools, this one is a single-click add:
//! the user picks position + visible fields in the toolbar, clicks
//! "Place info", and we append a `CaptureInfo` annotation. The actual text
//! is materialised at flatten/export time from `Document::metadata`.

use crate::editor::document::{
    AnnotationNode, CaptureInfoPosition, CaptureInfoStyle, FieldKind,
};
use uuid::Uuid;

pub fn make(
    position: CaptureInfoPosition,
    fields: Vec<FieldKind>,
    style: CaptureInfoStyle,
) -> AnnotationNode {
    AnnotationNode::CaptureInfo {
        id: Uuid::new_v4(),
        position,
        fields,
        style,
    }
}

/// Default ordered field list when the user first enables the tool.
pub fn default_fields() -> Vec<FieldKind> {
    vec![
        FieldKind::Timestamp,
        FieldKind::WindowTitle,
        FieldKind::ProcessName,
        FieldKind::OsVersion,
    ]
}
