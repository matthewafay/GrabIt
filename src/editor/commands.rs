//! Command-pattern undo/redo for the editor.
//!
//! Each user action that mutates the document is expressed as a `Command`
//! with `apply(&mut Document)` + `revert(&mut Document)`. A bounded history
//! (200 entries) caps memory so 4K captures don't drag the app down even
//! after hundreds of small edits.
//!
//! Commands operate on the `Document`'s cursor layer and annotation list
//! only — the base image is immutable in the editor.
//!
//! Design notes:
//! - Each command carries enough state to be invertible without consulting
//!   the document at the time of `revert`. `UpdateAnnotation` captures both
//!   before and after. `RemoveAnnotation` captures the removed node and its
//!   index so `revert` reinserts it at the same Z position.
//! - The stack stores trait objects behind `Box`. Commands are not `Clone`,
//!   which is fine because each apply/revert pair is owned by the stack.

use crate::editor::document::{
    AnnotationNode, Border, Document, EdgeEffect, SerializedCursor,
};
use uuid::Uuid;

/// Maximum undo history length. Picked to keep memory bounded for 4K
/// captures with many annotations — each entry is O(bytes-per-annotation),
/// not O(full-document).
pub const HISTORY_LIMIT: usize = 200;

pub trait Command: Send {
    fn apply(&mut self, doc: &mut Document);
    fn revert(&mut self, doc: &mut Document);
}

/// A bounded apply/revert stack. Pushing a new command after an undo clears
/// the redo stack (standard linear-history semantics).
pub struct History {
    undo: Vec<Box<dyn Command>>,
    redo: Vec<Box<dyn Command>>,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Apply a command and push it onto the undo stack. Clears redo.
    pub fn push(&mut self, mut cmd: Box<dyn Command>, doc: &mut Document) {
        cmd.apply(doc);
        self.redo.clear();
        self.undo.push(cmd);
        if self.undo.len() > HISTORY_LIMIT {
            let excess = self.undo.len() - HISTORY_LIMIT;
            self.undo.drain(0..excess);
        }
    }

    pub fn can_undo(&self) -> bool { !self.undo.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo.is_empty() }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        if let Some(mut cmd) = self.undo.pop() {
            cmd.revert(doc);
            self.redo.push(cmd);
            true
        } else { false }
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        if let Some(mut cmd) = self.redo.pop() {
            cmd.apply(doc);
            self.undo.push(cmd);
            true
        } else { false }
    }

    #[allow(dead_code)] // Reserved for M5 (document reload clears history).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Concrete commands
// ───────────────────────────────────────────────────────────────────────────

/// Append a new annotation at the end of the z-stack.
pub struct AddAnnotation {
    node: Option<AnnotationNode>,
    inserted_id: Option<Uuid>,
}

impl AddAnnotation {
    pub fn new(node: AnnotationNode) -> Self {
        Self { node: Some(node), inserted_id: None }
    }
}

impl Command for AddAnnotation {
    fn apply(&mut self, doc: &mut Document) {
        if let Some(n) = self.node.take() {
            self.inserted_id = Some(n.id());
            doc.annotations.push(n);
        }
    }
    fn revert(&mut self, doc: &mut Document) {
        if let Some(id) = self.inserted_id.take() {
            if let Some(pos) = doc.annotations.iter().position(|n| n.id() == id) {
                self.node = Some(doc.annotations.remove(pos));
            }
        }
    }
}

/// Remove an existing annotation, remembering its z-index so revert restores
/// the same stacking order.
pub struct RemoveAnnotation {
    target_id: Uuid,
    removed: Option<(usize, AnnotationNode)>,
}

impl RemoveAnnotation {
    pub fn new(target_id: Uuid) -> Self {
        Self { target_id, removed: None }
    }
}

impl Command for RemoveAnnotation {
    fn apply(&mut self, doc: &mut Document) {
        if let Some(pos) = doc.annotations.iter().position(|n| n.id() == self.target_id) {
            let node = doc.annotations.remove(pos);
            self.removed = Some((pos, node));
        }
    }
    fn revert(&mut self, doc: &mut Document) {
        if let Some((pos, node)) = self.removed.take() {
            let idx = pos.min(doc.annotations.len());
            doc.annotations.insert(idx, node);
        }
    }
}

/// Replace an annotation wholesale (for transforms, restyles, text edits).
/// Carries both before and after so apply/revert are symmetric.
pub struct UpdateAnnotation {
    target_id: Uuid,
    before: Option<AnnotationNode>,
    after: Option<AnnotationNode>,
}

impl UpdateAnnotation {
    pub fn new(before: AnnotationNode, after: AnnotationNode) -> Self {
        Self {
            target_id: before.id(),
            before: Some(before),
            after: Some(after),
        }
    }
}

impl Command for UpdateAnnotation {
    fn apply(&mut self, doc: &mut Document) {
        if let Some(after) = self.after.take() {
            if let Some(slot) = doc.annotations.iter_mut().find(|n| n.id() == self.target_id) {
                // Swap current → before (save for revert), place after.
                let prev = std::mem::replace(slot, after);
                self.before = Some(prev);
            }
        }
    }
    fn revert(&mut self, doc: &mut Document) {
        if let Some(before) = self.before.take() {
            if let Some(slot) = doc.annotations.iter_mut().find(|n| n.id() == self.target_id) {
                let cur = std::mem::replace(slot, before);
                self.after = Some(cur);
            }
        }
    }
}

/// Remove the cursor layer. Revert restores the exact same bytes + position.
pub struct RemoveCursor {
    saved: Option<SerializedCursor>,
}

impl RemoveCursor {
    pub fn new() -> Self { Self { saved: None } }
}

impl Command for RemoveCursor {
    fn apply(&mut self, doc: &mut Document) {
        if let Some(c) = doc.cursor.take() {
            self.saved = Some(c);
        }
    }
    fn revert(&mut self, doc: &mut Document) {
        if let Some(c) = self.saved.take() {
            doc.cursor = Some(c);
        }
    }
}

/// Move/resize the cursor layer. Holds before/after positions and sizes.
pub struct UpdateCursor {
    before: Option<(i32, i32, u32, u32)>, // x, y, width, height
    after: Option<(i32, i32, u32, u32)>,
}

impl UpdateCursor {
    pub fn new(
        before: (i32, i32, u32, u32),
        after: (i32, i32, u32, u32),
    ) -> Self {
        Self { before: Some(before), after: Some(after) }
    }
}

impl Command for UpdateCursor {
    fn apply(&mut self, doc: &mut Document) {
        if let (Some(c), Some(a)) = (doc.cursor.as_mut(), self.after) {
            c.x = a.0; c.y = a.1; c.width = a.2; c.height = a.3;
        }
    }
    fn revert(&mut self, doc: &mut Document) {
        if let (Some(c), Some(b)) = (doc.cursor.as_mut(), self.before) {
            c.x = b.0; c.y = b.1; c.width = b.2; c.height = b.3;
        }
    }
}

/// Set / clear the document-level torn-edge effect (M4 feature #21).
pub struct SetEdgeEffect {
    before: Option<EdgeEffect>,
    after: Option<EdgeEffect>,
}

impl SetEdgeEffect {
    pub fn new(before: Option<EdgeEffect>, after: Option<EdgeEffect>) -> Self {
        Self { before, after }
    }
}

impl Command for SetEdgeEffect {
    fn apply(&mut self, doc: &mut Document) {
        doc.edge_effect = self.after;
    }
    fn revert(&mut self, doc: &mut Document) {
        doc.edge_effect = self.before;
    }
}

/// Set / clear the document-level border (M4 feature #22).
pub struct SetBorder {
    before: Option<Border>,
    after: Option<Border>,
}

impl SetBorder {
    pub fn new(before: Option<Border>, after: Option<Border>) -> Self {
        Self { before, after }
    }
}

impl Command for SetBorder {
    fn apply(&mut self, doc: &mut Document) {
        doc.border = self.after;
    }
    fn revert(&mut self, doc: &mut Document) {
        doc.border = self.before;
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Rect;
    use crate::editor::document::DOCUMENT_SCHEMA_VERSION;
    use chrono::TimeZone;

    fn empty_doc() -> Document {
        use image::{codecs::png::PngEncoder, ImageEncoder, RgbaImage};
        let img = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), 2, 2, image::ExtendedColorType::Rgba8)
            .unwrap();
        Document {
            schema_version: DOCUMENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            base_png: buf,
            base_width: 2,
            base_height: 2,
            cursor: None,
            annotations: Vec::new(),
            metadata: crate::capture::CaptureMetadata {
                captured_at: chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
                foreground_title: None,
                foreground_process: None,
                os_version: "t".into(),
                monitors: vec![],
                capture_rect: Rect { x: 0, y: 0, width: 2, height: 2 },
            },
            edge_effect: None,
            border: None,
        }
    }

    fn arrow(id: Uuid) -> AnnotationNode {
        AnnotationNode::Arrow {
            id,
            start: [0.0, 0.0],
            end: [1.0, 1.0],
            color: [10, 20, 30, 255],
            thickness: 2.0,
        }
    }

    #[test]
    fn add_then_undo_redo_identity() {
        let mut doc = empty_doc();
        let mut h = History::new();
        let id = Uuid::new_v4();
        h.push(Box::new(AddAnnotation::new(arrow(id))), &mut doc);
        assert_eq!(doc.annotations.len(), 1);
        assert!(h.undo(&mut doc));
        assert_eq!(doc.annotations.len(), 0);
        assert!(h.redo(&mut doc));
        assert_eq!(doc.annotations.len(), 1);
        assert_eq!(doc.annotations[0].id(), id);
    }

    #[test]
    fn remove_preserves_z_order() {
        let mut doc = empty_doc();
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        for id in &ids { doc.annotations.push(arrow(*id)); }
        let mut h = History::new();
        h.push(Box::new(RemoveAnnotation::new(ids[1])), &mut doc);
        assert_eq!(doc.annotations.len(), 2);
        h.undo(&mut doc);
        assert_eq!(doc.annotations.iter().map(|n| n.id()).collect::<Vec<_>>(), ids);
    }

    #[test]
    fn update_round_trips() {
        let mut doc = empty_doc();
        let id = Uuid::new_v4();
        doc.annotations.push(arrow(id));
        let mut h = History::new();
        let before = doc.annotations[0].clone();
        let mut after = before.clone();
        if let AnnotationNode::Arrow { thickness, .. } = &mut after {
            *thickness = 9.0;
        }
        h.push(Box::new(UpdateAnnotation::new(before, after)), &mut doc);
        if let AnnotationNode::Arrow { thickness, .. } = &doc.annotations[0] {
            assert!((thickness - 9.0).abs() < f32::EPSILON);
        }
        h.undo(&mut doc);
        if let AnnotationNode::Arrow { thickness, .. } = &doc.annotations[0] {
            assert!((thickness - 2.0).abs() < f32::EPSILON);
        }
        h.redo(&mut doc);
        if let AnnotationNode::Arrow { thickness, .. } = &doc.annotations[0] {
            assert!((thickness - 9.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn history_bounded() {
        let mut doc = empty_doc();
        let mut h = History::new();
        for _ in 0..(HISTORY_LIMIT + 10) {
            h.push(Box::new(AddAnnotation::new(arrow(Uuid::new_v4()))), &mut doc);
        }
        // Undo stack can only grow to HISTORY_LIMIT.
        assert_eq!(h.undo.len(), HISTORY_LIMIT);
    }

    #[test]
    fn cursor_remove_round_trips() {
        let mut doc = empty_doc();
        doc.cursor = Some(SerializedCursor {
            png: vec![1, 2, 3],
            width: 16,
            height: 16,
            x: 5,
            y: 7,
        });
        let mut h = History::new();
        h.push(Box::new(RemoveCursor::new()), &mut doc);
        assert!(doc.cursor.is_none());
        h.undo(&mut doc);
        assert!(doc.cursor.is_some());
        let c = doc.cursor.as_ref().unwrap();
        assert_eq!(c.x, 5);
        assert_eq!(c.width, 16);
    }

    #[test]
    fn new_push_clears_redo() {
        let mut doc = empty_doc();
        let mut h = History::new();
        h.push(Box::new(AddAnnotation::new(arrow(Uuid::new_v4()))), &mut doc);
        h.undo(&mut doc);
        assert!(h.can_redo());
        h.push(Box::new(AddAnnotation::new(arrow(Uuid::new_v4()))), &mut doc);
        assert!(!h.can_redo());
    }
}
