//! Annotation tools.
//!
//! Each module owns the drag-to-create UX for a single annotation variant.
//! Shared selection / transform math lives in `selection.rs`.
//!
//! Tool lifecycle, high level:
//!   1. User picks the tool from the toolbar.
//!   2. On drag-start over the canvas the tool captures an "in-progress"
//!      state (e.g. two corners of a rect, a pending step number).
//!   3. On drag (or click for Text/Step) the preview is painted.
//!   4. On drag-end the tool builds an `AnnotationNode` and pushes an
//!      `AddAnnotation` command onto the history stack.
//!
//! Tools never mutate the document directly — they go through the
//! `History` so every action is undoable.

pub mod arrow;
pub mod blur;
pub mod callout;
pub mod capture_info;
pub mod cursor_edit;
pub mod magnify;
pub mod selection;
pub mod shape;
pub mod step;
pub mod text;

/// Enum of all interactive tools available in the editor toolbar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Select,
    Arrow,
    Text,
    Callout,
    Rect,
    Ellipse,
    Step,
    Magnify,
    /// M4: drag a rect; pixels inside get gaussian-blurred at export.
    Blur,
    /// M4: single-click add of a capture-info banner.
    CaptureInfo,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Arrow => "Arrow",
            Tool::Text => "Text",
            Tool::Callout => "Callout",
            Tool::Rect => "Rect",
            Tool::Ellipse => "Ellipse",
            Tool::Step => "Step",
            Tool::Magnify => "Magnify",
            Tool::Blur => "Blur",
            Tool::CaptureInfo => "Info",
        }
    }
}
