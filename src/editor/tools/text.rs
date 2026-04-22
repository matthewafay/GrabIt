//! Text tool helpers.

use crate::editor::document::{AnnotationNode, TextAlign, TextListStyle};
use uuid::Uuid;

/// Build a Text annotation from the user-drawn text-box `rect`. The rect is
/// `[min_x, min_y, max_x, max_y]` in image-pixel coordinates; text wraps at
/// `max_x` and overflow past `max_y` still renders (the rect defines the
/// minimum render extent, not a clip box).
///
/// `frosted` / `shadow` / `align` / `list` are per-annotation style
/// toggles — see the schema docs on `AnnotationNode::Text`. A fresh text
/// box takes all four from the editor's current defaults.
#[allow(clippy::too_many_arguments)]
pub fn make(
    rect: [f32; 4],
    text: String,
    color: [u8; 4],
    size_px: f32,
    frosted: bool,
    shadow: bool,
    align: TextAlign,
    list: TextListStyle,
) -> AnnotationNode {
    AnnotationNode::Text {
        id: Uuid::new_v4(),
        rect,
        text,
        color,
        size_px,
        frosted,
        shadow,
        align,
        list,
    }
}
