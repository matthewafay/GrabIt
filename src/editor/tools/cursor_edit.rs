//! Cursor-layer edit helpers.
//!
//! The captured cursor is serialised as `SerializedCursor { png, x, y,
//! width, height }`. When the user clicks on the cursor in the editor we
//! show rect handles and allow:
//! - Body drag (move x/y)
//! - Corner/edge drag (resize width/height)
//! - Delete key → `RemoveCursor` command (undoable)
//!
//! No "re-add" gesture is required per the spec — undo restores the cursor.

use crate::editor::document::SerializedCursor;

/// Apply a handle drag to the cursor's (x, y, w, h) tuple, returning the
/// new tuple. Inputs are in image-pixel coordinates.
pub fn apply_rect(c: &SerializedCursor, rect: [f32; 4]) -> (i32, i32, u32, u32) {
    let nx = rect[0].round() as i32;
    let ny = rect[1].round() as i32;
    let nw = (rect[2] - rect[0]).round().max(4.0) as u32;
    let nh = (rect[3] - rect[1]).round().max(4.0) as u32;
    // At least 4px in each dimension — any smaller and handles become
    // impossible to grab.
    let _ = c;
    (nx, ny, nw, nh)
}
