//! Blur tool — non-destructive gaussian blur over a rectangular region.
//!
//! The tool is a drag-to-create UX like `shape` / `magnify`. The user drags
//! a rect; a `Blur` annotation is appended. Blur is non-destructive: the
//! `.grabit` document only stores the rect + gaussian sigma. At flatten /
//! PNG-export time the pixels of `base` inside the rect are gaussian-blurred
//! and blitted back onto the output image.
//!
//! Live preview in the canvas is a cheap stippled overlay (semi-transparent
//! dots on a translucent rect) — per-pixel gaussian every frame is too slow
//! on a 4K image. This hints at the blur without fully previewing it.

use crate::editor::document::AnnotationNode;
use uuid::Uuid;

pub fn make(rect: [f32; 4], radius_px: f32) -> AnnotationNode {
    AnnotationNode::Blur {
        id: Uuid::new_v4(),
        rect,
        radius_px: radius_px.max(0.5),
    }
}
