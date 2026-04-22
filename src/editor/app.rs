//! eframe-based editor window.
//!
//! Tool palette (M3):
//! - Select  — click an annotation / cursor to grab it; drag handles to
//!   move/resize; Delete key removes it. Undoable.
//! - Arrow   — drag from tail to tip.
//! - Text    — click to place; Enter commits, Escape cancels. Clicking an
//!   existing text re-enters edit mode.
//! - Callout — drag a rect; commits a speech-balloon with placeholder text.
//! - Rect    — drag a rectangle.
//! - Ellipse — drag an ellipse bounding rect.
//! - Step    — click to place a numbered marker (auto-increment across doc).
//! - Magnify — drag a rect to create a loupe; source rect defaults adjacent.
//!
//! Undo/redo: every user action is a `Command` (see
//! `crate::editor::commands`). Ctrl+Z undoes, Ctrl+Y / Ctrl+Shift+Z redoes.
//! History is bounded at 200 entries.
//!
//! Rendering:
//! - The base image is uploaded once as an `egui::TextureHandle`.
//! - All annotation previews are drawn via `egui::Painter` primitives — no
//!   per-frame image re-rasterisation. Flattening only happens on export.

use crate::app::paths::AppPaths;
use crate::editor::commands::{
    AddAnnotation, Command, History, RemoveAnnotation, RemoveCursor, UpdateAnnotation,
    UpdateCursor, SetBorder, SetEdgeEffect,
};
use crate::editor::document::{
    AnnotationNode, CaptureInfoPosition, CaptureInfoStyle, Document, Edge,
    FieldKind, ShapeKind, StampSource,
};
use crate::capture::CaptureMetadata;
use crate::editor::rasterize;
use crate::editor::tools::{
    self, arrow as tool_arrow, blur as tool_blur, callout as tool_callout,
    capture_info as tool_capture_info, magnify as tool_magnify,
    selection::{
        bounds_of_cursor, bounds_of_node, dist2_to_segment, drag_rect, hit_bbox,
        normalise as norm_bbox, rect_handles, Handle, SelectionTarget,
    },
    shape as tool_shape, step as tool_step, text as tool_text, Tool,
};
use crate::hotkeys::bindings::parse_chord;
use crate::presets::{self, Preset, PresetStore, PresetTargetKind, PostAction};
use crate::styles::{QuickStyle, StyleStore, StyleToolKind, StyleValues};
use anyhow::{Context, Result};
use eframe::egui;
use image::RgbaImage;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// Quantized cache key for a live blur texture. Rounding the rect to an
/// 8-px grid and the radius to 2-unit buckets avoids rebuilding the blur
/// every frame while the user drags a handle.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct BlurKey { x0: i32, y0: i32, x1: i32, y1: i32, radius_q: i32 }

/// Cache key for a baked capture-info banner texture. Metadata doesn't
/// change within an editor session, so in practice the key only flips when
/// the user toggles fields, moves the banner, or edits the style struct.
/// We hash the style by its byte-level fields to keep the key `Eq`.
#[derive(Clone, PartialEq, Eq)]
struct CaptureInfoKey {
    fields: Vec<FieldKind>,
    position: CaptureInfoPosition,
    fill: [u8; 4],
    text_color: [u8; 4],
    text_size_bits: u32,
    padding_bits: u32,
}

impl CaptureInfoKey {
    fn new(
        fields: &[FieldKind],
        position: CaptureInfoPosition,
        style: CaptureInfoStyle,
    ) -> Self {
        Self {
            fields: fields.to_vec(),
            position,
            fill: style.fill,
            text_color: style.text_color,
            text_size_bits: style.text_size.to_bits(),
            padding_bits: style.padding.to_bits(),
        }
    }
}

/// Pending drag state for drag-to-create tools.
struct PendingDrag {
    start: [f32; 2],
    current: [f32; 2],
}

/// Text annotation in the middle of being typed.
struct PendingText {
    position: [f32; 2],
    buffer: String,
    editing_id: Option<Uuid>,
}

/// Currently-engaged handle on a selected annotation / the cursor.
struct ActiveHandle {
    target: SelectionTarget,
    handle: Handle,
    /// Starting rect of the thing being dragged (image-pixel coords), OR
    /// for arrows, `[start.x, start.y, end.x, end.y]`.
    start_rect: [f32; 4],
    /// For callouts: starting tail tip position.
    start_tail: Option<[f32; 2]>,
    /// For magnifier: starting source rect.
    start_source: Option<[f32; 4]>,
    /// Pre-drag snapshot so we only emit one command at drag-end.
    before: Option<AnnotationNode>,
    /// Pre-drag cursor tuple (x, y, w, h).
    before_cursor: Option<(i32, i32, u32, u32)>,
    /// Anchor pointer position in image-pixel coordinates.
    anchor: [f32; 2],
}

pub struct EditorApp {
    document: Document,
    history: History,

    /// Where to write the flattened PNG on Save.
    png_path: PathBuf,
    /// Where to write the `.grabit` sidecar.
    grabit_path: PathBuf,
    /// Whether to copy the flattened PNG to the clipboard on Save.
    copy_to_clipboard: bool,

    tool: Tool,
    pending_drag: Option<PendingDrag>,
    pending_text: Option<PendingText>,

    selection: Option<SelectionTarget>,
    active_handle: Option<ActiveHandle>,

    /// Currently selected draw color (sRGB RGBA).
    color: egui::Color32,
    /// Secondary color used as stroke for shapes/callouts when fill is
    /// visible; also used for magnifier border.
    stroke_color: egui::Color32,
    /// If true, Shape/Callout get a translucent fill; if false, outline-only.
    use_fill: bool,

    thickness: f32,
    text_size: f32,
    step_radius: f32,
    magnify_circular: bool,

    /// Blur sigma for new Blur annotations (image pixels).
    blur_radius: f32,
    /// Position + fields for the Capture-Info tool (the click places a
    /// banner configured with these settings).
    info_position: CaptureInfoPosition,
    info_fields: Vec<FieldKind>,

    /// Controls for the document-level resize / rotate polish.
    resize_width: u32,
    resize_height: u32,
    resize_lock_aspect: bool,

    texture: Option<egui::TextureHandle>,
    base_rgba: Option<RgbaImage>,
    /// Cached cursor texture + a hash of the source PNG so we don't redecode
    /// every frame. Cleared (and rebuilt on next paint) if the cursor PNG
    /// changes, e.g. after an undo that restores a different-sized cursor.
    cursor_texture: Option<egui::TextureHandle>,
    cursor_texture_key: Option<(usize, u32, u32)>,
    /// Per-blur-node cached gaussian textures for the live canvas preview.
    /// Rebuilt only when the quantised rect/radius key changes.
    blur_textures: HashMap<Uuid, (BlurKey, egui::TextureHandle)>,
    /// Per-capture-info-node cached baked banner textures. Keyed by the
    /// node's field-set + position + style; rebuilt when any of those change.
    /// Also holds the baked banner's pixel size so we can paint it at 1:1.
    capture_info_textures: HashMap<Uuid, (CaptureInfoKey, egui::TextureHandle, [u32; 2])>,

    dirty: bool,
    saved_once: bool,
    status: String,
    /// True while the "Unsaved changes" modal is on screen.
    close_prompt_shown: bool,

    // ── M5 state ────────────────────────────────────────────────────────
    /// Paths used by the presets + styles panels to read/write TOML files.
    paths: AppPaths,
    /// Loaded presets — mirrors what the main-thread AppState has. Reloaded
    /// from disk on every editor open. Edits here round-trip to disk
    /// immediately, then the user clicks "Apply hotkeys" which writes a
    /// marker file that the main thread polls for.
    preset_store: PresetStore,
    /// Draft preset being authored in the presets panel. `None` = no panel
    /// open; `Some(idx = usize::MAX)` = new preset not yet saved.
    preset_draft: Option<(usize, Preset)>,
    preset_status: String,
    /// Per-session counter bumped whenever the user applies a presets edit;
    /// the main thread watches `presets_reload_marker` and rebinds hotkeys.
    preset_dirty: bool,
    /// Quick styles store — edits round-trip to `styles.toml` immediately.
    style_store: StyleStore,
    /// Current "Save style as..." draft name for the Styles panel.
    style_draft_name: String,
    /// Whether the right-hand inspector shows presets or document effects.
    inspector_tab: InspectorTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorTab {
    Document,
    Presets,
    Styles,
}

impl EditorApp {
    pub fn new(
        document: Document,
        png_path: PathBuf,
        grabit_path: PathBuf,
        copy_to_clipboard: bool,
        paths: AppPaths,
    ) -> Self {
        let resize_width = document.base_width;
        let resize_height = document.base_height;
        let preset_store = PresetStore::load(&paths);
        let style_store = StyleStore::load(&paths);
        Self {
            document,
            history: History::new(),
            png_path,
            grabit_path,
            copy_to_clipboard,
            tool: Tool::Arrow,
            pending_drag: None,
            pending_text: None,
            selection: None,
            active_handle: None,
            color: egui::Color32::from_rgb(220, 40, 40),
            stroke_color: egui::Color32::from_rgb(10, 10, 10),
            use_fill: true,
            thickness: 6.0,
            text_size: 28.0,
            step_radius: 24.0,
            magnify_circular: true,
            blur_radius: 12.0,
            info_position: CaptureInfoPosition::BottomRight,
            info_fields: tool_capture_info::default_fields(),
            resize_width,
            resize_height,
            resize_lock_aspect: true,
            texture: None,
            base_rgba: None,
            cursor_texture: None,
            cursor_texture_key: None,
            blur_textures: HashMap::new(),
            capture_info_textures: HashMap::new(),
            dirty: false,
            saved_once: false,
            status: String::new(),
            close_prompt_shown: false,
            paths,
            preset_store,
            preset_draft: None,
            preset_status: String::new(),
            preset_dirty: false,
            style_store,
            style_draft_name: String::new(),
            inspector_tab: InspectorTab::Document,
        }
    }

    fn ensure_image_loaded(&mut self, ctx: &egui::Context) -> bool {
        if self.texture.is_some() && self.base_rgba.is_some() {
            return true;
        }
        match image::load_from_memory(&self.document.base_png) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let size = [rgba.width() as usize, rgba.height() as usize];
                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                self.texture = Some(ctx.load_texture(
                    "grabit-base",
                    color_image,
                    egui::TextureOptions::LINEAR,
                ));
                self.base_rgba = Some(rgba);
                true
            }
            Err(e) => {
                warn!("failed to decode base image: {e}");
                false
            }
        }
    }

    fn push_command(&mut self, cmd: Box<dyn Command>) {
        self.history.push(cmd, &mut self.document);
        self.dirty = true;
    }

    fn undo(&mut self) {
        if self.history.undo(&mut self.document) {
            self.dirty = true;
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.document) {
            self.dirty = true;
        }
    }

    fn commit_pending_text(&mut self) {
        let Some(pt) = self.pending_text.take() else { return };
        let trimmed = pt.buffer.trim();
        let editing = pt.editing_id;

        if editing.is_none() && trimmed.is_empty() {
            return;
        }

        match editing {
            Some(id) => {
                if let Some(before) = self
                    .document
                    .annotations
                    .iter()
                    .find(|n| n.id() == id)
                    .cloned()
                {
                    if trimmed.is_empty() {
                        self.push_command(Box::new(RemoveAnnotation::new(id)));
                    } else {
                        let after = match &before {
                            AnnotationNode::Text { id, position, color, size_px, .. } => {
                                AnnotationNode::Text {
                                    id: *id,
                                    position: *position,
                                    text: trimmed.to_string(),
                                    color: *color,
                                    size_px: *size_px,
                                }
                            }
                            _ => before.clone(),
                        };
                        self.push_command(Box::new(UpdateAnnotation::new(before, after)));
                    }
                }
            }
            None => {
                let node = tool_text::make(
                    pt.position,
                    trimmed.to_string(),
                    color_to_rgba(self.color),
                    self.text_size,
                );
                self.push_command(Box::new(AddAnnotation::new(node)));
            }
        }
    }

    fn cancel_pending_text(&mut self) {
        self.pending_text = None;
    }

    fn copy_to_clipboard_only(&mut self) -> Result<()> {
        let base = self
            .base_rgba
            .as_ref()
            .context("base image not decoded")?
            .clone();
        let cursor_composite = self.compose_cursor(base);
        let flat = rasterize::flatten(
            &cursor_composite,
            &self.document.annotations,
            Some(&self.document.metadata),
        );
        let flat = rasterize::apply_document_effects(
            flat,
            self.document.edge_effect,
            self.document.border,
        );
        let flat = self.apply_export_resize(flat);
        copy_rgba_to_clipboard(&flat).context("copy to clipboard")?;
        self.status = "Copied to clipboard".to_string();
        Ok(())
    }

    fn save(&mut self) -> Result<()> {
        let base = self
            .base_rgba
            .as_ref()
            .context("base image not decoded")?
            .clone();
        let cursor_composite = self.compose_cursor(base);
        let flat = rasterize::flatten(
            &cursor_composite,
            &self.document.annotations,
            Some(&self.document.metadata),
        );
        let flat = rasterize::apply_document_effects(
            flat,
            self.document.edge_effect,
            self.document.border,
        );
        // Resize pass (feature #23 polish) — applies only at export, so
        // annotations keep their crisp vector geometry while the user was
        // editing; resizing happens last on the flattened RGBA.
        let flat = self.apply_export_resize(flat);

        flat.save_with_format(&self.png_path, image::ImageFormat::Png)
            .with_context(|| format!("write {}", self.png_path.display()))?;
        info!("saved {}", self.png_path.display());

        if let Err(e) = crate::editor::document::save(&self.document, &self.grabit_path) {
            warn!("grabit sidecar save failed: {e}");
        }

        if self.copy_to_clipboard {
            if let Err(e) = copy_rgba_to_clipboard(&flat) {
                warn!("clipboard copy failed: {e}");
            }
        }

        self.dirty = false;
        self.saved_once = true;
        self.status = format!("Saved to {}", self.png_path.display());
        Ok(())
    }

    /// Composite the cursor layer (if any, with its current position/size)
    /// onto `base`. Used by save + clipboard-copy paths so exports reflect
    /// any cursor edits the user made.
    fn compose_cursor(&self, mut base: RgbaImage) -> RgbaImage {
        if let Some(c) = &self.document.cursor {
            if let Ok(cur) = image::load_from_memory(&c.png) {
                let cursor_img = cur.to_rgba8();
                let rect = [
                    c.x as f32,
                    c.y as f32,
                    (c.x + c.width as i32) as f32,
                    (c.y + c.height as i32) as f32,
                ];
                // Blit via rasterize helper: we draw the cursor as an
                // inline stamp to reuse the alpha-blend path.
                let tmp = AnnotationNode::Stamp {
                    id: Uuid::new_v4(),
                    source: StampSource::Inline { png: c.png.clone() },
                    rect,
                };
                let _ = cursor_img; // keep decoded for potential future inline drawing
                base = rasterize::flatten(&base, &[tmp], None);
            }
        }
        base
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for t in [
                Tool::Select, Tool::Arrow, Tool::Text, Tool::Callout,
                Tool::Rect, Tool::Ellipse, Tool::Step, Tool::Magnify,
                Tool::Blur, Tool::CaptureInfo,
            ] {
                if ui.selectable_label(self.tool == t, t.label()).clicked() {
                    // Switching away from the Text tool: persist any in-progress
                    // text so the user doesn't lose their typing.
                    if self.tool == Tool::Text && t != Tool::Text {
                        self.commit_pending_text();
                    }
                    self.tool = t;
                    self.pending_drag = None;
                }
            }
            ui.separator();

            ui.label("Color");
            ui.color_edit_button_srgba(&mut self.color);

            if matches!(self.tool, Tool::Rect | Tool::Ellipse | Tool::Callout | Tool::Magnify) {
                ui.label("Stroke");
                ui.color_edit_button_srgba(&mut self.stroke_color);
                ui.checkbox(&mut self.use_fill, "Fill");
            }

            match self.tool {
                Tool::Arrow => {
                    ui.label("Thickness");
                    ui.add(egui::Slider::new(&mut self.thickness, 1.0..=40.0));
                }
                Tool::Text => {
                    ui.label("Size");
                    ui.add(egui::Slider::new(&mut self.text_size, 8.0..=128.0));
                }
                Tool::Rect | Tool::Ellipse | Tool::Callout => {
                    ui.label("Stroke width");
                    ui.add(egui::Slider::new(&mut self.thickness, 1.0..=24.0));
                    if self.tool == Tool::Callout {
                        ui.label("Text size");
                        ui.add(egui::Slider::new(&mut self.text_size, 8.0..=64.0));
                    }
                }
                Tool::Step => {
                    ui.label("Radius");
                    ui.add(egui::Slider::new(&mut self.step_radius, 10.0..=80.0));
                }
                Tool::Magnify => {
                    ui.checkbox(&mut self.magnify_circular, "Circular");
                    ui.label("Border");
                    ui.add(egui::Slider::new(&mut self.thickness, 0.0..=10.0));
                }
                Tool::Blur => {
                    ui.label("Radius");
                    ui.add(egui::Slider::new(&mut self.blur_radius, 1.0..=64.0));
                }
                Tool::CaptureInfo => {
                    egui::ComboBox::from_id_salt("info-pos")
                        .selected_text(self.info_position.label())
                        .show_ui(ui, |ui| {
                            for p in [
                                CaptureInfoPosition::TopLeft,
                                CaptureInfoPosition::TopRight,
                                CaptureInfoPosition::BottomLeft,
                                CaptureInfoPosition::BottomRight,
                            ] {
                                ui.selectable_value(&mut self.info_position, p, p.label());
                            }
                        });
                    for f in [
                        FieldKind::Timestamp,
                        FieldKind::WindowTitle,
                        FieldKind::ProcessName,
                        FieldKind::OsVersion,
                        FieldKind::MonitorInfo,
                    ] {
                        let mut on = self.info_fields.contains(&f);
                        if ui.checkbox(&mut on, f.label()).changed() {
                            if on {
                                if !self.info_fields.contains(&f) {
                                    self.info_fields.push(f);
                                }
                            } else {
                                self.info_fields.retain(|x| *x != f);
                            }
                        }
                    }
                    if ui.button("Place info").clicked() {
                        let node = tool_capture_info::make(
                            self.info_position,
                            self.info_fields.clone(),
                            CaptureInfoStyle::default(),
                        );
                        self.push_command(Box::new(AddAnnotation::new(node)));
                    }
                }
                Tool::Select => {}
            }
        });

        ui.horizontal(|ui| {
            let undo_enabled = self.history.can_undo();
            if ui.add_enabled(undo_enabled, egui::Button::new("Undo (Ctrl+Z)")).clicked() {
                self.undo();
            }
            let redo_enabled = self.history.can_redo();
            if ui.add_enabled(redo_enabled, egui::Button::new("Redo (Ctrl+Y)")).clicked() {
                self.redo();
            }

            let del_enabled = self.selection.is_some();
            if ui.add_enabled(del_enabled, egui::Button::new("Delete (Del)")).clicked() {
                self.delete_selection();
            }

            ui.separator();

            if ui.button("Copy to clipboard").clicked() {
                if let Err(e) = self.copy_to_clipboard_only() {
                    self.status = format!("Copy failed: {e}");
                }
            }

            let save_label = if self.dirty || !self.saved_once {
                "Save (Ctrl+S)"
            } else {
                "Saved \u{2713}"
            };
            if ui.button(save_label).clicked() {
                if let Err(e) = self.save() {
                    self.status = format!("Save failed: {e}");
                }
            }
        });

        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }

    /// Right-hand inspector for document-level effects: torn edge, border,
    /// and the non-destructive resize/rotate handles (feature #23 polish).
    fn document_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Document");
            ui.separator();

            // ── Torn edge ──────────────────────────────────────────────
            ui.label(egui::RichText::new("Torn edge").strong());
            let mut has_edge = self.document.edge_effect.is_some();
            if ui.checkbox(&mut has_edge, "Enable torn edge").changed() {
                let before = self.document.edge_effect;
                let after = if has_edge {
                    Some(self.document.edge_effect.unwrap_or_default())
                } else {
                    None
                };
                self.push_command(Box::new(SetEdgeEffect::new(before, after)));
            }
            if let Some(mut e) = self.document.edge_effect {
                let mut changed = false;
                egui::ComboBox::from_id_salt("edge-side")
                    .selected_text(match e.edge {
                        Edge::Top => "Top",
                        Edge::Bottom => "Bottom",
                        Edge::Left => "Left",
                        Edge::Right => "Right",
                    })
                    .show_ui(ui, |ui| {
                        for (lbl, side) in [
                            ("Top", Edge::Top),
                            ("Bottom", Edge::Bottom),
                            ("Left", Edge::Left),
                            ("Right", Edge::Right),
                        ] {
                            if ui.selectable_value(&mut e.edge, side, lbl).changed() {
                                changed = true;
                            }
                        }
                    });
                if ui
                    .add(egui::Slider::new(&mut e.depth, 1.0..=80.0).text("Depth"))
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .add(egui::Slider::new(&mut e.teeth, 4.0..=80.0).text("Tooth"))
                    .changed()
                {
                    changed = true;
                }
                if changed {
                    let before = self.document.edge_effect;
                    self.push_command(Box::new(SetEdgeEffect::new(before, Some(e))));
                }
            }

            ui.add_space(8.0);

            // ── Border ──────────────────────────────────────────────────
            ui.label(egui::RichText::new("Border").strong());
            let mut has_border = self.document.border.is_some();
            if ui.checkbox(&mut has_border, "Enable border").changed() {
                let before = self.document.border;
                let after = if has_border {
                    Some(self.document.border.unwrap_or_default())
                } else {
                    None
                };
                self.push_command(Box::new(SetBorder::new(before, after)));
            }
            if let Some(mut b) = self.document.border {
                let mut changed = false;
                let mut color = egui::Color32::from_rgba_unmultiplied(
                    b.color[0], b.color[1], b.color[2], b.color[3],
                );
                ui.horizontal(|ui| {
                    ui.label("Color");
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        b.color = [color.r(), color.g(), color.b(), color.a()];
                        changed = true;
                    }
                });
                if ui
                    .add(egui::Slider::new(&mut b.width, 0.0..=40.0).text("Width"))
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .add(
                        egui::Slider::new(&mut b.shadow_radius, 0.0..=40.0)
                            .text("Shadow blur"),
                    )
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .add(
                        egui::Slider::new(&mut b.shadow_offset[0], -20.0..=20.0)
                            .text("Shadow dx"),
                    )
                    .changed()
                {
                    changed = true;
                }
                if ui
                    .add(
                        egui::Slider::new(&mut b.shadow_offset[1], -20.0..=20.0)
                            .text("Shadow dy"),
                    )
                    .changed()
                {
                    changed = true;
                }
                if changed {
                    let before = self.document.border;
                    self.push_command(Box::new(SetBorder::new(before, Some(b))));
                }
            }

            ui.add_space(8.0);

            // ── Resize / rotate polish ──────────────────────────────────
            ui.label(egui::RichText::new("Resize / rotate").strong());
            ui.checkbox(&mut self.resize_lock_aspect, "Lock aspect ratio");
            let base_w = self.document.base_width.max(1) as f32;
            let base_h = self.document.base_height.max(1) as f32;
            let aspect = base_w / base_h;
            let mut w = self.resize_width as i32;
            let mut h = self.resize_height as i32;
            ui.horizontal(|ui| {
                ui.label("W");
                if ui.add(egui::DragValue::new(&mut w).range(1..=32768)).changed() {
                    self.resize_width = w.max(1) as u32;
                    if self.resize_lock_aspect {
                        self.resize_height =
                            ((self.resize_width as f32 / aspect).round().max(1.0)) as u32;
                    }
                }
                ui.label("H");
                if ui.add(egui::DragValue::new(&mut h).range(1..=32768)).changed() {
                    self.resize_height = h.max(1) as u32;
                    if self.resize_lock_aspect {
                        self.resize_width =
                            ((self.resize_height as f32 * aspect).round().max(1.0)) as u32;
                    }
                }
            });
            ui.label(format!(
                "Base: {}x{} (ratio {:.3})",
                self.document.base_width, self.document.base_height, aspect
            ));
            if ui.button("Reset to base size").clicked() {
                self.resize_width = self.document.base_width;
                self.resize_height = self.document.base_height;
            }
            ui.horizontal(|ui| {
                if ui.button("Rotate 90° \u{21BB}").clicked() {
                    self.rotate_base_cw();
                }
                if ui.button("Rotate -90° \u{21BA}").clicked() {
                    self.rotate_base_ccw();
                }
            });
            ui.label(
                egui::RichText::new(
                    "Resize applies at PNG export; Shift+R rotates 90°.",
                )
                .small()
                .weak(),
            );
        });
    }

    // ───────────────────────────────────────────────────────────────────
    // M5 panels: Presets (#3, #4) and Quick Styles (#19).
    //
    // Edits round-trip through TOML on disk. The editor is a worker-thread
    // window, separate from the main thread that owns the HotkeyRegistrar.
    // When the user changes a preset's hotkey, we save the preset file and
    // drop a tiny marker (`presets/.refresh`) — the main thread's event
    // loop notices it on its next tick, re-reads the preset store, and
    // calls `Registrar::refresh_hotkeys`. This keeps the editor loosely
    // coupled to the Win32 message pump.
    // ───────────────────────────────────────────────────────────────────

    fn presets_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Presets");
            ui.label(
                egui::RichText::new(
                    "Named capture configs, optionally bound to a global hotkey.",
                )
                .small()
                .weak(),
            );
            ui.separator();

            // Existing presets list with "Capture", "Edit", "Duplicate",
            // "Delete" per entry.
            let mut pending_action: Option<PresetAction> = None;
            for (idx, p) in self.preset_store.presets.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.strong(&p.name);
                    if !p.hotkey.is_empty() {
                        ui.label(egui::RichText::new(&p.hotkey).monospace().weak());
                    }
                });
                ui.horizontal(|ui| {
                    if ui.small_button("Capture now").clicked() {
                        pending_action = Some(PresetAction::CaptureNow(p.name.clone()));
                    }
                    if ui.small_button("Edit").clicked() {
                        pending_action = Some(PresetAction::Edit(idx));
                    }
                    if ui.small_button("Duplicate").clicked() {
                        pending_action = Some(PresetAction::Duplicate(idx));
                    }
                    if ui.small_button("Delete").clicked() {
                        pending_action = Some(PresetAction::Delete(idx));
                    }
                });
                ui.label(
                    egui::RichText::new(format!(
                        "{}  \u{00b7}  delay {}ms  \u{00b7}  cursor: {}  \u{00b7}  {}",
                        p.target.label(),
                        p.delay_ms,
                        if p.include_cursor { "on" } else { "off" },
                        p.post_action.label()
                    ))
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
            }

            if let Some(action) = pending_action {
                self.apply_preset_action(action);
            }

            ui.separator();
            if ui.button("New preset").clicked() {
                self.preset_draft = Some((usize::MAX, Preset::default()));
            }

            // Draft / edit form
            if let Some((idx, mut draft)) = self.preset_draft.take() {
                ui.separator();
                ui.strong(if idx == usize::MAX { "New preset" } else { "Edit preset" });

                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut draft.name);
                });

                egui::ComboBox::from_label("Target")
                    .selected_text(draft.target.label())
                    .show_ui(ui, |ui| {
                        for k in PresetTargetKind::ALL {
                            ui.selectable_value(&mut draft.target, k, k.label());
                        }
                    });

                if draft.target == PresetTargetKind::ExactDims {
                    ui.horizontal(|ui| {
                        ui.label("W");
                        let mut w = draft.width as i32;
                        if ui.add(egui::DragValue::new(&mut w).range(1..=32768)).changed() {
                            draft.width = w.max(1) as u32;
                        }
                        ui.label("H");
                        let mut h = draft.height as i32;
                        if ui.add(egui::DragValue::new(&mut h).range(1..=32768)).changed() {
                            draft.height = h.max(1) as u32;
                        }
                    });
                }

                ui.horizontal(|ui| {
                    ui.label("Delay (ms)");
                    let mut d = draft.delay_ms as i32;
                    if ui.add(egui::DragValue::new(&mut d).range(0..=60_000)).changed() {
                        draft.delay_ms = d.max(0) as u32;
                    }
                });
                ui.checkbox(&mut draft.include_cursor, "Include cursor");

                egui::ComboBox::from_label("Post action")
                    .selected_text(draft.post_action.label())
                    .show_ui(ui, |ui| {
                        for a in PostAction::ALL {
                            ui.selectable_value(&mut draft.post_action, a, a.label());
                        }
                    });

                ui.horizontal(|ui| {
                    ui.label("Filename template");
                    ui.text_edit_singleline(&mut draft.filename_template);
                });
                ui.label(
                    egui::RichText::new("Tokens: {timestamp}, {window}")
                        .small()
                        .weak(),
                );

                ui.horizontal(|ui| {
                    ui.label("Subfolder");
                    ui.text_edit_singleline(&mut draft.subfolder);
                });

                ui.horizontal(|ui| {
                    ui.label("Hotkey");
                    ui.text_edit_singleline(&mut draft.hotkey);
                });
                if !draft.hotkey.trim().is_empty() {
                    match parse_chord(&draft.hotkey) {
                        Ok((canon, _)) => {
                            draft.hotkey = canon;
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::RED, format!("Invalid: {e}"));
                        }
                    }
                }

                ui.horizontal(|ui| {
                    if ui.button("Save preset").clicked() {
                        self.save_preset_draft(idx, draft.clone());
                        self.preset_draft = None;
                    } else if ui.button("Cancel").clicked() {
                        self.preset_draft = None;
                    } else {
                        self.preset_draft = Some((idx, draft));
                    }
                });
            }

            if !self.preset_status.is_empty() {
                ui.separator();
                ui.label(&self.preset_status);
            }
        });
    }

    fn apply_preset_action(&mut self, action: PresetAction) {
        match action {
            PresetAction::Edit(i) => {
                if let Some(p) = self.preset_store.presets.get(i).cloned() {
                    self.preset_draft = Some((i, p));
                }
            }
            PresetAction::Duplicate(i) => {
                if let Some(p) = self.preset_store.presets.get(i).cloned() {
                    let mut copy = p;
                    copy.name = format!("{} copy", copy.name);
                    copy.hotkey.clear(); // fresh copy needs its own chord
                    self.preset_draft = Some((usize::MAX, copy));
                }
            }
            PresetAction::Delete(i) => {
                if i < self.preset_store.presets.len() {
                    let removed = self.preset_store.presets.remove(i);
                    if let Err(e) = presets::delete_preset_file(&self.paths, &removed.slug()) {
                        warn!("delete preset file: {e}");
                    }
                    self.preset_status =
                        format!("Deleted preset {:?}", removed.name);
                    self.mark_presets_dirty();
                }
            }
            PresetAction::CaptureNow(name) => {
                // The editor thread can't reach the main dispatcher
                // directly, but it can drop a marker file the main thread
                // polls for. For "capture now" from the editor we write
                // a one-shot marker whose payload is the preset name.
                let marker = self.paths.data_dir.join(".capture_preset");
                if let Err(e) = std::fs::write(&marker, &name) {
                    self.preset_status = format!("Capture request failed: {e}");
                } else {
                    self.preset_status =
                        format!("Capture queued: {name} (run this hotkey from the tray to confirm).");
                }
            }
        }
    }

    fn save_preset_draft(&mut self, idx: usize, draft: Preset) {
        if draft.name.trim().is_empty() {
            self.preset_status = "Preset name cannot be empty.".into();
            self.preset_draft = Some((idx, draft));
            return;
        }
        // Validate hotkey (empty is OK — unbound preset).
        if !draft.hotkey.trim().is_empty() {
            if let Err(e) = parse_chord(&draft.hotkey) {
                self.preset_status = format!("Invalid hotkey '{}': {e}", draft.hotkey);
                self.preset_draft = Some((idx, draft));
                return;
            }
        }
        // Write the preset file. On rename, delete the old slug's file so
        // we don't leave orphaned TOML behind.
        let old_slug = if idx == usize::MAX {
            None
        } else {
            self.preset_store.presets.get(idx).map(|p| p.slug())
        };
        if let Err(e) = presets::save_preset(&self.paths, &draft) {
            self.preset_status = format!("Save failed: {e}");
            return;
        }
        if let Some(old) = old_slug {
            if old != draft.slug() {
                let _ = presets::delete_preset_file(&self.paths, &old);
            }
        }

        // Reload from disk for a canonical view, then mark dirty so the
        // main thread refreshes its hotkey bindings on the next tick.
        self.preset_store = PresetStore::load(&self.paths);
        self.preset_status = format!("Saved preset {:?}", draft.name);
        self.mark_presets_dirty();
    }

    fn mark_presets_dirty(&mut self) {
        self.preset_dirty = true;
        // Drop a marker file the main-thread event loop polls for. The
        // payload is unused — its mere existence triggers a reload.
        let marker = self.paths.data_dir.join(".presets_refresh");
        if let Err(e) = std::fs::write(&marker, "") {
            debug!("write presets refresh marker: {e}");
        }
    }

    fn styles_panel(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Quick styles");
            ui.label(
                egui::RichText::new(
                    "Save the active tool's current knobs under a name; re-apply with one click.",
                )
                .small()
                .weak(),
            );
            ui.separator();

            // Styles grouped by tool.
            let mut pending_apply: Option<(StyleToolKind, String)> = None;
            let mut pending_delete: Option<(StyleToolKind, String)> = None;
            for tool in StyleToolKind::ALL {
                let entries: Vec<QuickStyle> =
                    self.style_store.for_tool(tool).cloned().collect();
                if entries.is_empty() {
                    continue;
                }
                ui.strong(tool.label());
                for s in &entries {
                    ui.horizontal(|ui| {
                        ui.label(&s.name);
                        if ui.small_button("Apply").clicked() {
                            pending_apply = Some((tool, s.name.clone()));
                        }
                        if ui.small_button("\u{d7}").on_hover_text("Delete").clicked() {
                            pending_delete = Some((tool, s.name.clone()));
                        }
                    });
                }
                ui.add_space(4.0);
            }
            if let Some((tool, name)) = pending_apply {
                self.apply_style(tool, &name);
            }
            if let Some((tool, name)) = pending_delete {
                self.style_store.remove(tool, &name);
                if let Err(e) = self.style_store.save(&self.paths) {
                    warn!("styles save after delete: {e}");
                }
            }

            ui.separator();

            // Save-current-tool-as form.
            let active_kind = tool_to_style_kind(self.tool);
            if let Some(kind) = active_kind {
                ui.label(format!("Active tool: {}", kind.label()));
                ui.horizontal(|ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.style_draft_name);
                });
                if ui.button("Save current as quick style").clicked() {
                    if self.style_draft_name.trim().is_empty() {
                        self.status = "Style name cannot be empty.".into();
                    } else {
                        let values = self.capture_active_style_values(kind);
                        self.style_store.upsert(QuickStyle {
                            name: self.style_draft_name.trim().to_string(),
                            tool: kind,
                            values,
                        });
                        if let Err(e) = self.style_store.save(&self.paths) {
                            self.status = format!("Styles save failed: {e}");
                        } else {
                            self.status = format!(
                                "Saved style {:?} for {}",
                                self.style_draft_name,
                                kind.label()
                            );
                            self.style_draft_name.clear();
                        }
                    }
                }
            } else {
                ui.label("Select a styleable tool to save a quick style.");
            }
        });
    }

    fn capture_active_style_values(&self, kind: StyleToolKind) -> StyleValues {
        let color = color32_to_rgba(self.color);
        let stroke_color = color32_to_rgba(self.stroke_color);
        match kind {
            StyleToolKind::Arrow => StyleValues {
                color: Some(color),
                thickness: Some(self.thickness),
                ..Default::default()
            },
            StyleToolKind::Text => StyleValues {
                color: Some(color),
                text_size: Some(self.text_size),
                ..Default::default()
            },
            StyleToolKind::Callout => StyleValues {
                color: Some(color),
                stroke_color: Some(stroke_color),
                use_fill: Some(self.use_fill),
                thickness: Some(self.thickness),
                text_size: Some(self.text_size),
                ..Default::default()
            },
            StyleToolKind::Rect | StyleToolKind::Ellipse => StyleValues {
                color: Some(color),
                stroke_color: Some(stroke_color),
                use_fill: Some(self.use_fill),
                thickness: Some(self.thickness),
                ..Default::default()
            },
            StyleToolKind::Step => StyleValues {
                color: Some(color),
                step_radius: Some(self.step_radius),
                ..Default::default()
            },
            StyleToolKind::Blur => StyleValues {
                blur_radius: Some(self.blur_radius),
                ..Default::default()
            },
            StyleToolKind::Magnify => StyleValues {
                stroke_color: Some(stroke_color),
                thickness: Some(self.thickness),
                magnify_circular: Some(self.magnify_circular),
                ..Default::default()
            },
        }
    }

    fn apply_style(&mut self, kind: StyleToolKind, name: &str) {
        let Some(style) = self
            .style_store
            .for_tool(kind)
            .find(|s| s.name == name)
            .cloned()
        else {
            return;
        };
        let v = &style.values;
        if let Some(c) = v.color {
            self.color = rgba_to_color32(c);
        }
        if let Some(c) = v.stroke_color {
            self.stroke_color = rgba_to_color32(c);
        }
        if let Some(f) = v.use_fill {
            self.use_fill = f;
        }
        if let Some(t) = v.thickness {
            self.thickness = t;
        }
        if let Some(t) = v.text_size {
            self.text_size = t;
        }
        if let Some(r) = v.step_radius {
            self.step_radius = r;
        }
        if let Some(c) = v.magnify_circular {
            self.magnify_circular = c;
        }
        if let Some(b) = v.blur_radius {
            self.blur_radius = b;
        }
        // Also switch the active tool to match, so the style takes effect
        // on the very next drawing action.
        if let Some(t) = style_kind_to_tool(kind) {
            self.tool = t;
        }
        self.status = format!("Applied style {:?}", style.name);
    }

    /// Rotate the base image 90° clockwise in place. Annotations are kept
    /// as-is — this is a destructive document-level operation, which is
    /// consistent with how crop already works at M2. Pushing a full image
    /// snapshot into the history is overkill for undo; the rotate is a
    /// directly-reversible operation so we just allow a reverse-rotate.
    fn rotate_base_cw(&mut self) {
        let Some(base) = self.base_rgba.as_ref().cloned() else { return };
        let rotated = image::imageops::rotate90(&base);
        self.replace_base(rotated);
    }

    fn rotate_base_ccw(&mut self) {
        let Some(base) = self.base_rgba.as_ref().cloned() else { return };
        let rotated = image::imageops::rotate270(&base);
        self.replace_base(rotated);
    }

    /// Resize the flattened export to the user-configured width/height.
    /// No-op when both axes match `flat.dimensions()`. Uses a high-quality
    /// Lanczos3 filter since this runs once at export, not every frame.
    fn apply_export_resize(&self, flat: RgbaImage) -> RgbaImage {
        let (cur_w, cur_h) = flat.dimensions();
        let w = self.resize_width.max(1);
        let h = self.resize_height.max(1);
        if w == cur_w && h == cur_h {
            return flat;
        }
        image::imageops::resize(&flat, w, h, image::imageops::FilterType::Lanczos3)
    }

    fn replace_base(&mut self, new_base: RgbaImage) {
        use image::codecs::png::PngEncoder;
        use image::ImageEncoder;
        let mut buf = Vec::new();
        if PngEncoder::new(&mut buf)
            .write_image(
                new_base.as_raw(),
                new_base.width(),
                new_base.height(),
                image::ExtendedColorType::Rgba8,
            )
            .is_err()
        {
            return;
        }
        self.document.base_png = buf;
        self.document.base_width = new_base.width();
        self.document.base_height = new_base.height();
        self.base_rgba = Some(new_base);
        // Force the GPU texture to be rebuilt next paint.
        self.texture = None;
        self.resize_width = self.document.base_width;
        self.resize_height = self.document.base_height;
        self.dirty = true;
        // Rotating invalidates undo/redo coordinates — clear history so a
        // stray undo can't place an annotation off the image.
        self.history.clear();
    }

    fn delete_selection(&mut self) {
        let Some(sel) = self.selection.take() else { return };
        match sel {
            SelectionTarget::Annotation(id) => {
                self.push_command(Box::new(RemoveAnnotation::new(id)));
            }
            SelectionTarget::Cursor => {
                self.push_command(Box::new(RemoveCursor::new()));
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let Some(texture) = self.texture.as_ref().cloned() else {
            ui.centered_and_justified(|ui| ui.label("Loading capture\u{2026}"));
            return;
        };
        let img_w = self.document.base_width as f32;
        let img_h = self.document.base_height as f32;
        if img_w <= 0.0 || img_h <= 0.0 {
            return;
        }

        let available = ui.available_size();
        let scale = (available.x / img_w).min(available.y / img_h).clamp(0.01, 1.0);
        let display = egui::vec2(img_w * scale, img_h * scale);

        let (response, painter) = ui.allocate_painter(display, egui::Sense::click_and_drag());
        let rect = response.rect;

        // Document-level border preview (shadow behind + frame around the
        // base image). The frame sits outside `rect` so it doesn't occlude
        // annotations — matches the export where `apply_border` extends the
        // canvas outward.
        self.paint_border_preview(&painter, rect, scale);

        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        let to_image = |p: egui::Pos2| -> [f32; 2] {
            let local = p - rect.min;
            [
                (local.x / scale).clamp(0.0, img_w),
                (local.y / scale).clamp(0.0, img_h),
            ]
        };
        let to_canvas = |p: [f32; 2]| -> egui::Pos2 {
            egui::pos2(rect.min.x + p[0] * scale, rect.min.y + p[1] * scale)
        };

        // Draw cursor layer (if present) in preview.
        let cursor_geom = self.document.cursor.as_ref().map(|c| {
            (c.x as f32, c.y as f32, c.width as f32, c.height as f32)
        });
        if let Some((cx, cy, cw, ch)) = cursor_geom {
            if let Some(tex) = self.cursor_texture(ui.ctx()) {
                let r = egui::Rect::from_min_size(
                    to_canvas([cx, cy]),
                    egui::vec2(cw * scale, ch * scale),
                );
                painter.image(
                    tex.id(),
                    r,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // Draw existing annotations. Blur, Magnify, and CaptureInfo need
        // `&mut self` for their cached textures, so we defer them into local
        // vectors and handle them after the immutable pass.
        let editing_id = self.pending_text.as_ref().and_then(|pt| pt.editing_id);
        let mut blurs: Vec<(Uuid, [f32; 4], f32)> = Vec::new();
        #[allow(clippy::type_complexity)]
        let mut magnifies: Vec<(Uuid, [f32; 4], [f32; 4], [u8; 4], f32, bool)> = Vec::new();
        let mut infos: Vec<(Uuid, Vec<FieldKind>, CaptureInfoPosition, CaptureInfoStyle)> =
            Vec::new();
        for node in &self.document.annotations {
            if Some(node.id()) == editing_id { continue; }
            match node {
                AnnotationNode::Blur { id, rect, radius_px } => {
                    blurs.push((*id, *rect, *radius_px));
                }
                AnnotationNode::Magnify {
                    id, source_rect, target_rect, border, border_width, circular, ..
                } => {
                    magnifies.push((
                        *id,
                        *source_rect,
                        *target_rect,
                        *border,
                        *border_width,
                        *circular,
                    ));
                }
                AnnotationNode::CaptureInfo { id, fields, position, style, .. } => {
                    infos.push((*id, fields.clone(), *position, *style));
                }
                _ => {
                    draw_node_preview(&painter, node, scale, &to_canvas);
                }
            }
        }
        let ctx = ui.ctx().clone();
        for (id, rect, radius) in blurs {
            self.paint_blur_live(&ctx, &painter, id, rect, radius, &to_canvas);
        }
        for (_id, src, dst, border, border_w, circular) in magnifies {
            self.paint_magnify_live(
                &painter, src, dst, border, border_w, circular, scale, &to_canvas,
            );
        }
        for (id, fields, position, style) in infos {
            self.paint_capture_info_live(
                &ctx, &painter, id, &fields, position, style, scale, rect,
            );
        }

        // Handle input — must come BEFORE drawing the selection overlay so
        // the overlay reflects the latest state.
        match self.tool {
            Tool::Select => self.handle_select_input(&response, &to_image),
            Tool::Arrow => self.handle_arrow_input(&response, &to_image),
            Tool::Text => self.handle_text_input(&response, &to_image, &to_canvas, ui),
            Tool::Callout | Tool::Rect | Tool::Ellipse | Tool::Magnify
            | Tool::Blur => {
                self.handle_drag_rect_input(&response, &to_image);
            }
            Tool::Step => self.handle_step_input(&response, &to_image),
            // Capture-info is placed from the toolbar's "Place info" button,
            // not a canvas drag. The canvas ignores clicks.
            Tool::CaptureInfo => {}
        }

        // Draw in-progress drag preview.
        if let Some(d) = &self.pending_drag {
            let s = to_canvas(d.start);
            let e = to_canvas(d.current);
            let r = egui::Rect::from_two_pos(s, e);
            match self.tool {
                Tool::Arrow => {
                    draw_arrow_preview(
                        &painter, s, e, self.color, self.thickness * scale,
                    );
                }
                Tool::Rect | Tool::Magnify => {
                    painter.rect_stroke(r, 0.0, egui::Stroke::new(
                        (self.thickness * scale).max(1.0),
                        self.color,
                    ));
                }
                Tool::Blur => {
                    // Cheap pixelated hint — filled rect with a dashed border.
                    painter.rect_filled(
                        r, 0.0,
                        egui::Color32::from_rgba_unmultiplied(120, 140, 200, 80),
                    );
                    painter.rect_stroke(
                        r, 0.0,
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 140, 200)),
                    );
                }
                Tool::Ellipse => {
                    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(40);
                    let cx = r.center().x;
                    let cy = r.center().y;
                    let rx = 0.5 * r.width();
                    let ry = 0.5 * r.height();
                    for i in 0..40 {
                        let a = (i as f32) * std::f32::consts::TAU / 40.0;
                        pts.push(egui::pos2(cx + rx * a.cos(), cy + ry * a.sin()));
                    }
                    pts.push(pts[0]);
                    painter.add(egui::Shape::line(pts, egui::Stroke::new(
                        (self.thickness * scale).max(1.0),
                        self.color,
                    )));
                }
                Tool::Callout => {
                    painter.rect_stroke(r, 4.0, egui::Stroke::new(
                        (self.thickness * scale).max(1.0),
                        self.color,
                    ));
                }
                _ => {}
            }
        }

        // Torn-edge overlay. Painted AFTER annotations so teeth cut through
        // them, matching the rasterize order (edge-effect runs before
        // border on export). The panel-fill colour visually approximates
        // transparency against the editor chrome.
        self.paint_torn_preview(&painter, ui, rect, scale);

        // Selection overlay.
        if let Some(sel) = self.selection {
            self.draw_selection_handles(&painter, sel, &to_canvas);
        }

        // Floating text editor overlay.
        if self.pending_text.is_some() {
            let canvas_size = self.text_size * scale;
            let anchor = {
                let pt = self.pending_text.as_ref().unwrap();
                to_canvas(pt.position)
            };
            let text_color = self.color;

            let ctx = ui.ctx().clone();
            let area_resp = egui::Area::new(egui::Id::new("grabit-pending-text"))
                .order(egui::Order::Foreground)
                .fixed_pos(anchor)
                .show(&ctx, |ui| {
                    let pt = self.pending_text.as_mut().unwrap();
                    let edit = egui::TextEdit::singleline(&mut pt.buffer)
                        .font(egui::FontId::monospace(canvas_size.max(12.0)))
                        .text_color(text_color)
                        .desired_width(280.0)
                        .hint_text("Type then Enter\u{2026}");
                    let response = ui.add(edit);
                    response.request_focus();
                    response
                });

            let commit_enter = area_resp.inner.lost_focus()
                && ctx.input(|i| i.key_pressed(egui::Key::Enter));
            let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
            if escape {
                self.cancel_pending_text();
            } else if commit_enter {
                self.commit_pending_text();
            }
        }
    }

    fn handle_arrow_input(
        &mut self,
        response: &egui::Response,
        to_image: &dyn Fn(egui::Pos2) -> [f32; 2],
    ) {
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_image(pos);
                self.pending_drag = Some(PendingDrag { start: p, current: p });
            }
        }
        if response.dragged() {
            if let (Some(current), Some(pd)) =
                (response.interact_pointer_pos(), self.pending_drag.as_mut())
            {
                pd.current = to_image(current);
            }
        }
        if response.drag_stopped() {
            if let Some(d) = self.pending_drag.take() {
                let dx = d.current[0] - d.start[0];
                let dy = d.current[1] - d.start[1];
                if dx * dx + dy * dy >= 4.0 {
                    let node = tool_arrow::make(
                        d.start, d.current, color_to_rgba(self.color), self.thickness,
                    );
                    self.push_command(Box::new(AddAnnotation::new(node)));
                }
            }
        }
    }

    fn handle_text_input(
        &mut self,
        response: &egui::Response,
        to_image: &dyn Fn(egui::Pos2) -> [f32; 2],
        to_canvas: &dyn Fn([f32; 2]) -> egui::Pos2,
        ui: &mut egui::Ui,
    ) {
        if !response.clicked() { return; }
        let Some(click_pos) = response.interact_pointer_pos() else { return };

        // Hit-test existing text first so a click on placed text re-enters
        // edit mode. We use egui's own font layout so the hit rect matches
        // the on-screen glyph metrics.
        let mut hit: Option<usize> = None;
        for (i, node) in self.document.annotations.iter().enumerate().rev() {
            if let AnnotationNode::Text { position, text, size_px, .. } = node {
                let font_id = egui::FontId::monospace(*size_px);
                let galley = ui.ctx().fonts(|f| {
                    f.layout_no_wrap(text.clone(), font_id, egui::Color32::WHITE)
                });
                let r = egui::Rect::from_min_size(to_canvas(*position), galley.size());
                if r.contains(click_pos) { hit = Some(i); break; }
            }
        }

        self.commit_pending_text();

        if let Some(idx) = hit {
            if let AnnotationNode::Text { id, position, text, color, size_px } =
                self.document.annotations[idx].clone()
            {
                self.color = egui::Color32::from_rgba_unmultiplied(
                    color[0], color[1], color[2], color[3],
                );
                self.text_size = size_px;
                self.pending_text = Some(PendingText {
                    position,
                    buffer: text,
                    editing_id: Some(id),
                });
            }
        } else {
            self.pending_text = Some(PendingText {
                position: to_image(click_pos),
                buffer: String::new(),
                editing_id: None,
            });
        }
    }

    fn handle_step_input(
        &mut self,
        response: &egui::Response,
        to_image: &dyn Fn(egui::Pos2) -> [f32; 2],
    ) {
        if !response.clicked() { return; }
        let Some(pos) = response.interact_pointer_pos() else { return };
        let number = tool_step::next_number(&self.document);
        let fill = color_to_rgba(self.color);
        // White label when the fill is dark, black when light.
        let lum = 0.299 * self.color.r() as f32
                + 0.587 * self.color.g() as f32
                + 0.114 * self.color.b() as f32;
        let text_color = if lum < 160.0 { [255, 255, 255, 255] } else { [0, 0, 0, 255] };
        let node = tool_step::make(to_image(pos), self.step_radius, number, fill, text_color);
        self.push_command(Box::new(AddAnnotation::new(node)));
    }

    fn handle_drag_rect_input(
        &mut self,
        response: &egui::Response,
        to_image: &dyn Fn(egui::Pos2) -> [f32; 2],
    ) {
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_image(pos);
                self.pending_drag = Some(PendingDrag { start: p, current: p });
            }
        }
        if response.dragged() {
            if let (Some(current), Some(pd)) =
                (response.interact_pointer_pos(), self.pending_drag.as_mut())
            {
                pd.current = to_image(current);
            }
        }
        if response.drag_stopped() {
            if let Some(d) = self.pending_drag.take() {
                let rect = [d.start[0], d.start[1], d.current[0], d.current[1]];
                let r = norm_bbox(rect);
                if (r[2] - r[0]) * (r[3] - r[1]) < 16.0 { return; }
                let node = match self.tool {
                    Tool::Rect => tool_shape::make(
                        ShapeKind::Rect,
                        r,
                        color_to_rgba(self.stroke_color_for_current()),
                        self.thickness,
                        if self.use_fill { color_to_rgba_alpha(self.color, 80) } else { [0, 0, 0, 0] },
                    ),
                    Tool::Ellipse => tool_shape::make(
                        ShapeKind::Ellipse,
                        r,
                        color_to_rgba(self.stroke_color_for_current()),
                        self.thickness,
                        if self.use_fill { color_to_rgba_alpha(self.color, 80) } else { [0, 0, 0, 0] },
                    ),
                    Tool::Callout => tool_callout::make(
                        r,
                        "Note".to_string(),
                        if self.use_fill { color_to_rgba(self.color) } else { [255, 255, 230, 230] },
                        color_to_rgba(self.stroke_color),
                        self.thickness,
                        [0, 0, 0, 255],
                        self.text_size,
                    ),
                    Tool::Magnify => tool_magnify::make(
                        r,
                        tool_magnify::default_source_for_target(r),
                        color_to_rgba(self.stroke_color),
                        self.thickness,
                        self.magnify_circular,
                    ),
                    Tool::Blur => tool_blur::make(r, self.blur_radius),
                    _ => return,
                };
                self.push_command(Box::new(AddAnnotation::new(node)));
            }
        }
    }

    fn stroke_color_for_current(&self) -> egui::Color32 {
        self.stroke_color
    }

    fn handle_select_input(
        &mut self,
        response: &egui::Response,
        to_image: &dyn Fn(egui::Pos2) -> [f32; 2],
    ) {
        // On drag-start, figure out what (if anything) was grabbed.
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_image(pos);
                self.active_handle = self.pick_handle(p);
            }
        }

        // Live update during drag.
        if response.dragged() {
            if let (Some(cur), Some(ah)) =
                (response.interact_pointer_pos(), self.active_handle.as_mut())
            {
                let cur_img = to_image(cur);
                let dx = cur_img[0] - ah.anchor[0];
                let dy = cur_img[1] - ah.anchor[1];

                match ah.target {
                    SelectionTarget::Annotation(id) => {
                        if let Some(slot) = self.document.annotations.iter_mut().find(|n| n.id() == id) {
                            apply_drag_to_node(slot, ah, dx, dy);
                        }
                    }
                    SelectionTarget::Cursor => {
                        let new_rect = drag_rect(ah.start_rect, ah.handle, dx, dy);
                        if let Some(c) = self.document.cursor.as_mut() {
                            let (nx, ny, nw, nh) = tools::cursor_edit::apply_rect(c, new_rect);
                            c.x = nx; c.y = ny; c.width = nw; c.height = nh;
                        }
                    }
                }
            }
        }

        // On drag end, emit a single command so the whole gesture is a
        // single undo step.
        if response.drag_stopped() {
            if let Some(ah) = self.active_handle.take() {
                match ah.target {
                    SelectionTarget::Annotation(id) => {
                        if let (Some(before), Some(after)) = (
                            ah.before,
                            self.document.annotations.iter().find(|n| n.id() == id).cloned(),
                        ) {
                            // Check whether anything actually changed.
                            if format!("{:?}", before) != format!("{:?}", after) {
                                // Apply produces the "after" state by swapping; we
                                // need to temporarily put `before` back so the
                                // command's apply() sees the right transition.
                                if let Some(slot) = self.document.annotations.iter_mut().find(|n| n.id() == id) {
                                    *slot = before.clone();
                                }
                                self.push_command(Box::new(UpdateAnnotation::new(before, after)));
                            }
                        }
                    }
                    SelectionTarget::Cursor => {
                        if let (Some(before), Some(cursor)) = (ah.before_cursor, self.document.cursor.as_ref()) {
                            let after = (cursor.x, cursor.y, cursor.width, cursor.height);
                            if before != after {
                                // Revert to `before` state, then push command.
                                if let Some(c) = self.document.cursor.as_mut() {
                                    c.x = before.0; c.y = before.1; c.width = before.2; c.height = before.3;
                                }
                                self.push_command(Box::new(UpdateCursor::new(before, after)));
                            }
                        }
                    }
                }
            }
        }

        // Clicks that aren't drags: pick the annotation under the cursor.
        if response.clicked() && self.active_handle.is_none() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_image(pos);
                self.selection = self.pick_target(p);
            }
        }
    }

    /// Pick an annotation / cursor at image-pixel point `p`, topmost first.
    fn pick_target(&self, p: [f32; 2]) -> Option<SelectionTarget> {
        // Cursor is drawn below annotations but pickable.
        for node in self.document.annotations.iter().rev() {
            if hit_node(node, p) {
                return Some(SelectionTarget::Annotation(node.id()));
            }
        }
        if let Some(c) = &self.document.cursor {
            if hit_bbox(p, bounds_of_cursor(c)) {
                return Some(SelectionTarget::Cursor);
            }
        }
        None
    }

    /// Given the current `selection`, pick which handle (if any) is at `p`.
    /// Also starts a body-drag if the click lands inside the selection rect.
    /// Returns `None` if the click misses the selection.
    fn pick_handle(&mut self, p: [f32; 2]) -> Option<ActiveHandle> {
        // First, if there's no selection, try to select something at p.
        if self.selection.is_none() {
            self.selection = self.pick_target(p);
        }
        let sel = self.selection?;

        let handle_r = 8.0; // hit radius in image pixels
        let (bbox, start_rect, start_tail, start_source, before_node, before_cursor)
            = match sel {
            SelectionTarget::Annotation(id) => {
                let node = self.document.annotations.iter().find(|n| n.id() == id)?;
                let b = bounds_of_node(node)?;
                let sr = match node {
                    AnnotationNode::Arrow { start, end, .. } => {
                        [start[0], start[1], end[0], end[1]]
                    }
                    _ => b,
                };
                let tail = if let AnnotationNode::Callout { tail, .. } = node {
                    Some(*tail)
                } else { None };
                let source = if let AnnotationNode::Magnify { source_rect, .. } = node {
                    Some(*source_rect)
                } else { None };
                (b, sr, tail, source, Some(node.clone()), None)
            }
            SelectionTarget::Cursor => {
                let c = self.document.cursor.as_ref()?;
                let b = bounds_of_cursor(c);
                (b, b, None, None, None, Some((c.x, c.y, c.width, c.height)))
            }
        };

        // Arrow endpoints (only for arrow annotations).
        if let SelectionTarget::Annotation(id) = sel {
            if let Some(AnnotationNode::Arrow { start, end, .. }) =
                self.document.annotations.iter().find(|n| n.id() == id)
            {
                if dist_sq(p, *start) <= handle_r * handle_r {
                    return Some(ActiveHandle {
                        target: sel,
                        handle: Handle::ArrowStart,
                        start_rect,
                        start_tail,
                        start_source,
                        before: before_node.clone(),
                        before_cursor,
                        anchor: p,
                    });
                }
                if dist_sq(p, *end) <= handle_r * handle_r {
                    return Some(ActiveHandle {
                        target: sel,
                        handle: Handle::ArrowEnd,
                        start_rect,
                        start_tail,
                        start_source,
                        before: before_node.clone(),
                        before_cursor,
                        anchor: p,
                    });
                }
                // Body drag on arrows grabs the midpoint.
                let mid = [0.5 * (start[0] + end[0]), 0.5 * (start[1] + end[1])];
                if dist_sq(p, mid) <= 12.0 * 12.0 {
                    return Some(ActiveHandle {
                        target: sel,
                        handle: Handle::Body,
                        start_rect,
                        start_tail,
                        start_source,
                        before: before_node,
                        before_cursor,
                        anchor: p,
                    });
                }
                return None;
            }
        }

        // Callout tail.
        if let Some(tail) = start_tail {
            if dist_sq(p, tail) <= handle_r * handle_r {
                return Some(ActiveHandle {
                    target: sel,
                    handle: Handle::CalloutTail,
                    start_rect,
                    start_tail,
                    start_source,
                    before: before_node,
                    before_cursor,
                    anchor: p,
                });
            }
        }

        // Magnify source rect — body-drag only.
        if let Some(src) = start_source {
            if hit_bbox(p, src) {
                return Some(ActiveHandle {
                    target: sel,
                    handle: Handle::MagnifySource,
                    start_rect,
                    start_tail,
                    start_source,
                    before: before_node,
                    before_cursor,
                    anchor: p,
                });
            }
        }

        // Rect handles.
        for (h, hp) in rect_handles(bbox) {
            if dist_sq(p, hp) <= handle_r * handle_r {
                return Some(ActiveHandle {
                    target: sel,
                    handle: h,
                    start_rect,
                    start_tail,
                    start_source,
                    before: before_node,
                    before_cursor,
                    anchor: p,
                });
            }
        }

        // Body-drag fallback.
        if hit_bbox(p, bbox) {
            return Some(ActiveHandle {
                target: sel,
                handle: Handle::Body,
                start_rect,
                start_tail,
                start_source,
                before: before_node,
                before_cursor,
                anchor: p,
            });
        }

        None
    }

    fn draw_selection_handles(
        &self,
        painter: &egui::Painter,
        sel: SelectionTarget,
        to_canvas: &dyn Fn([f32; 2]) -> egui::Pos2,
    ) {
        let bbox = match sel {
            SelectionTarget::Annotation(id) => {
                let Some(n) = self.document.annotations.iter().find(|n| n.id() == id) else { return };
                bounds_of_node(n)
            }
            SelectionTarget::Cursor => {
                self.document.cursor.as_ref().map(bounds_of_cursor)
            }
        };
        let Some(bbox) = bbox else { return };

        let min = to_canvas([bbox[0], bbox[1]]);
        let max = to_canvas([bbox[2], bbox[3]]);
        let rect = egui::Rect::from_two_pos(min, max);
        painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(60, 160, 255)));

        let handle_color = egui::Color32::WHITE;
        let handle_stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(60, 160, 255));
        for (_, hp) in rect_handles(bbox) {
            let c = to_canvas(hp);
            painter.circle(c, 5.0, handle_color, handle_stroke);
        }

        // Per-variant extras.
        if let SelectionTarget::Annotation(id) = sel {
            if let Some(node) = self.document.annotations.iter().find(|n| n.id() == id) {
                match node {
                    AnnotationNode::Callout { tail, .. } => {
                        let c = to_canvas(*tail);
                        painter.circle(
                            c, 5.0, egui::Color32::from_rgb(255, 220, 60), handle_stroke,
                        );
                    }
                    AnnotationNode::Magnify { source_rect, .. } => {
                        let r = egui::Rect::from_two_pos(
                            to_canvas([source_rect[0], source_rect[1]]),
                            to_canvas([source_rect[2], source_rect[3]]),
                        );
                        painter.rect_stroke(
                            r, 0.0,
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 220, 60)),
                        );
                    }
                    AnnotationNode::Arrow { start, end, .. } => {
                        painter.circle(to_canvas(*start), 5.0, handle_color, handle_stroke);
                        painter.circle(to_canvas(*end), 5.0, handle_color, handle_stroke);
                    }
                    _ => {}
                }
            }
        }
    }

    fn cursor_texture(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let c = self.document.cursor.as_ref()?;
        let key = (c.png.len(), c.width, c.height);
        if self.cursor_texture_key != Some(key) {
            let img = image::load_from_memory(&c.png).ok()?.to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [img.width() as usize, img.height() as usize],
                img.as_raw(),
            );
            self.cursor_texture = Some(ctx.load_texture(
                "grabit-cursor", ci, egui::TextureOptions::LINEAR,
            ));
            self.cursor_texture_key = Some(key);
        }
        self.cursor_texture.clone()
    }

    fn paint_blur_live(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        id: Uuid,
        rect: [f32; 4],
        radius: f32,
        to_canvas: &impl Fn([f32; 2]) -> egui::Pos2,
    ) {
        let Some(base) = self.base_rgba.as_ref() else { return };
        let key = blur_key(rect, radius);
        let needs = self.blur_textures.get(&id).map(|(k, _)| *k != key).unwrap_or(true);
        if needs {
            match build_blur_texture(ctx, base, rect, radius) {
                Some(tex) => { self.blur_textures.insert(id, (key, tex)); }
                None => { self.blur_textures.remove(&id); return; }
            }
        }
        let Some((_, tex)) = self.blur_textures.get(&id) else { return };
        let r = egui::Rect::from_two_pos(
            to_canvas([rect[0], rect[1]]),
            to_canvas([rect[2], rect[3]]),
        );
        painter.image(
            tex.id(),
            r,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    /// Paint a live magnifier preview by UV-mapping the base texture into the
    /// target rect. Matches the export's rectangular + circular paths.
    #[allow(clippy::too_many_arguments)] // Mirrors the signature of rasterize::draw_magnify.
    fn paint_magnify_live(
        &self,
        painter: &egui::Painter,
        source_rect: [f32; 4],
        target_rect: [f32; 4],
        border: [u8; 4],
        border_width: f32,
        circular: bool,
        scale: f32,
        to_canvas: &dyn Fn([f32; 2]) -> egui::Pos2,
    ) {
        let Some(tex) = self.texture.as_ref() else { return };
        let img_w = self.document.base_width as f32;
        let img_h = self.document.base_height as f32;
        if img_w <= 0.0 || img_h <= 0.0 { return; }

        // Source UVs into the base texture.
        let sx0 = source_rect[0].min(source_rect[2]);
        let sy0 = source_rect[1].min(source_rect[3]);
        let sx1 = source_rect[0].max(source_rect[2]);
        let sy1 = source_rect[1].max(source_rect[3]);
        let u0 = (sx0 / img_w).clamp(0.0, 1.0);
        let v0 = (sy0 / img_h).clamp(0.0, 1.0);
        let u1 = (sx1 / img_w).clamp(0.0, 1.0);
        let v1 = (sy1 / img_h).clamp(0.0, 1.0);

        let dst_min = to_canvas([
            target_rect[0].min(target_rect[2]),
            target_rect[1].min(target_rect[3]),
        ]);
        let dst_max = to_canvas([
            target_rect[0].max(target_rect[2]),
            target_rect[1].max(target_rect[3]),
        ]);
        let dst = egui::Rect::from_min_max(dst_min, dst_max);

        let border_color = egui_color(border);
        let stroke_w = (border_width * scale).max(1.0);

        if circular {
            // Build a tessellated circle (fan-like strip around the perimeter)
            // with UVs sampled from the source rect.
            const N: usize = 48;
            let cx = dst.center().x;
            let cy = dst.center().y;
            let rx = 0.5 * dst.width();
            let ry = 0.5 * dst.height();
            let uc = 0.5 * (u0 + u1);
            let vc = 0.5 * (v0 + v1);
            let du = 0.5 * (u1 - u0);
            let dv = 0.5 * (v1 - v0);

            let mut mesh = egui::Mesh::with_texture(tex.id());
            // Centre vertex.
            mesh.vertices.push(egui::epaint::Vertex {
                pos: egui::pos2(cx, cy),
                uv: egui::pos2(uc, vc),
                color: egui::Color32::WHITE,
            });
            for i in 0..N {
                let a = (i as f32) * std::f32::consts::TAU / (N as f32);
                let ca = a.cos();
                let sa = a.sin();
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: egui::pos2(cx + rx * ca, cy + ry * sa),
                    uv: egui::pos2(uc + du * ca, vc + dv * sa),
                    color: egui::Color32::WHITE,
                });
            }
            for i in 0..N as u32 {
                let a = i + 1;
                let b = (i + 1) % N as u32 + 1;
                mesh.indices.extend_from_slice(&[0, a, b]);
            }
            painter.add(egui::Shape::mesh(mesh));

            if border[3] > 0 && border_width > 0.0 {
                // Stroke circle (ellipse) on top.
                let mut pts: Vec<egui::Pos2> = Vec::with_capacity(N + 1);
                for i in 0..N {
                    let a = (i as f32) * std::f32::consts::TAU / (N as f32);
                    pts.push(egui::pos2(cx + rx * a.cos(), cy + ry * a.sin()));
                }
                pts.push(pts[0]);
                painter.add(egui::Shape::line(
                    pts,
                    egui::Stroke::new(stroke_w, border_color),
                ));
            }
        } else {
            let uv = egui::Rect::from_min_max(egui::pos2(u0, v0), egui::pos2(u1, v1));
            painter.image(tex.id(), dst, uv, egui::Color32::WHITE);
            if border[3] > 0 && border_width > 0.0 {
                painter.rect_stroke(
                    dst, 0.0,
                    egui::Stroke::new(stroke_w, border_color),
                );
            }
        }
    }

    /// Paint a live capture-info banner preview using a cached baked texture
    /// produced by `rasterize::draw_capture_info` into a transparent buffer.
    #[allow(clippy::too_many_arguments)] // Mirrors the rasterize banner signature.
    fn paint_capture_info_live(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        id: Uuid,
        fields: &[FieldKind],
        position: CaptureInfoPosition,
        style: CaptureInfoStyle,
        scale: f32,
        canvas_rect: egui::Rect,
    ) {
        let key = CaptureInfoKey::new(fields, position, style);
        let needs = self
            .capture_info_textures
            .get(&id)
            .map(|(k, _, _)| *k != key)
            .unwrap_or(true);
        if needs {
            match build_capture_info_texture(ctx, &self.document.metadata, fields, position, style)
            {
                Some((tex, size)) => {
                    self.capture_info_textures.insert(id, (key, tex, size));
                }
                None => {
                    self.capture_info_textures.remove(&id);
                    return;
                }
            }
        }
        let Some((_, tex, size)) = self.capture_info_textures.get(&id) else { return };
        let [pw, ph] = *size;
        if pw == 0 || ph == 0 {
            return;
        }
        // Position the banner inside the canvas rect using the same anchor
        // rule as `rasterize::draw_capture_info` (0-px margin — banner is
        // flush with the edge), then paint it at image-pixel size * scale.
        let banner_w = pw as f32 * scale;
        let banner_h = ph as f32 * scale;
        let anchor = match position {
            CaptureInfoPosition::TopLeft => canvas_rect.left_top(),
            CaptureInfoPosition::TopRight => {
                egui::pos2(canvas_rect.right() - banner_w, canvas_rect.top())
            }
            CaptureInfoPosition::BottomLeft => {
                egui::pos2(canvas_rect.left(), canvas_rect.bottom() - banner_h)
            }
            CaptureInfoPosition::BottomRight => {
                egui::pos2(
                    canvas_rect.right() - banner_w,
                    canvas_rect.bottom() - banner_h,
                )
            }
        };
        let r = egui::Rect::from_min_size(anchor, egui::vec2(banner_w, banner_h));
        painter.image(
            tex.id(),
            r,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    /// Paint a live preview of the document-level border + drop shadow.
    /// Shadow is faked with a stack of concentric translucent rects (the
    /// export uses a real gaussian via `apply_border`).
    fn paint_border_preview(
        &self,
        painter: &egui::Painter,
        canvas_rect: egui::Rect,
        scale: f32,
    ) {
        let Some(b) = self.document.border else { return };

        // Shadow first — behind the base image. Offset in image pixels
        // scaled to canvas.
        if b.shadow_color[3] > 0 && (b.shadow_radius > 0.0 || b.shadow_offset != [0.0, 0.0]) {
            let sox = b.shadow_offset[0] * scale;
            let soy = b.shadow_offset[1] * scale;
            let shadow_r = b.shadow_radius * scale;
            // The exported shadow is the (base rect + border) silhouette,
            // offset and blurred. Start with the border-outer rect shifted.
            let bw = b.width * scale;
            let base_outer = egui::Rect::from_min_max(
                egui::pos2(canvas_rect.min.x - bw, canvas_rect.min.y - bw),
                egui::pos2(canvas_rect.max.x + bw, canvas_rect.max.y + bw),
            );
            let shadow_rect = base_outer.translate(egui::vec2(sox, soy));
            // Fake softness: 4 concentric rects, each inset by 25% of
            // shadow_r, with decreasing opacity. Gives a passable gaussian
            // halo without blurring on the CPU every frame.
            let layers = 4i32;
            for i in 0..layers {
                let t = i as f32 / layers as f32; // 0..1
                let outset = shadow_r * (1.0 - t); // outermost = full radius
                let alpha = (b.shadow_color[3] as f32
                    * (0.35 * (1.0 - t) + 0.15))
                    .clamp(0.0, 255.0) as u8;
                let c = egui::Color32::from_rgba_unmultiplied(
                    b.shadow_color[0],
                    b.shadow_color[1],
                    b.shadow_color[2],
                    alpha,
                );
                let rr = shadow_rect.expand(outset);
                painter.rect_filled(rr, 0.0, c);
            }
        }

        // Border frame: stroke outside the base canvas rect, width scaled.
        if b.width > 0.0 && b.color[3] > 0 {
            let bw = b.width * scale;
            // A stroke centred on the outer edge draws half-in, half-out.
            // Push the stroke centre out by bw/2 so the band sits fully
            // outside the base image, matching the export.
            let outer = canvas_rect.expand(bw * 0.5);
            let color = egui_color(b.color);
            painter.rect_stroke(outer, 0.0, egui::Stroke::new(bw, color));
        }
    }

    /// Paint a jagged torn-edge silhouette on top of the canvas rect in the
    /// panel background color, matching the tooth math in `apply_torn_edge`.
    fn paint_torn_preview(
        &self,
        painter: &egui::Painter,
        ui: &egui::Ui,
        canvas_rect: egui::Rect,
        scale: f32,
    ) {
        let Some(e) = self.document.edge_effect else { return };
        let bg = ui.style().visuals.panel_fill;

        let w = self.document.base_width;
        let h = self.document.base_height;
        if w == 0 || h == 0 {
            return;
        }
        let depth = e.depth.max(1.0);
        let teeth = e.teeth.max(4.0);

        // Identical hash as rasterize::apply_torn_edge so teeth line up.
        let jitter = |n: u32| -> f32 {
            let mut s = n.wrapping_mul(2654435761).wrapping_add(1);
            s ^= s >> 16;
            s = s.wrapping_mul(0x85ebca6b);
            let u = (s & 0xFFFF) as f32 / 65536.0;
            (u - 0.5) * 0.6
        };

        // For each image-pixel column/row compute the cut depth, then paint
        // a thin canvas-space rect covering that strip. `scale` maps image
        // pixels → canvas pixels.
        match e.edge {
            Edge::Top | Edge::Bottom => {
                for x in 0..w {
                    let idx = x as f32 / teeth;
                    let tooth_n = idx.floor() as u32;
                    let frac = idx - idx.floor();
                    let tri = 1.0 - (2.0 * frac - 1.0).abs();
                    let j = 1.0 + jitter(tooth_n);
                    let d = (depth * tri * j).max(0.0);
                    if d <= 0.0 {
                        continue;
                    }
                    let col_min_x = canvas_rect.min.x + x as f32 * scale;
                    let col_max_x = canvas_rect.min.x + (x + 1) as f32 * scale;
                    let (ty0, ty1) = match e.edge {
                        Edge::Top => (canvas_rect.min.y, canvas_rect.min.y + d * scale),
                        Edge::Bottom => (canvas_rect.max.y - d * scale, canvas_rect.max.y),
                        _ => unreachable!(),
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(col_min_x, ty0),
                            egui::pos2(col_max_x, ty1),
                        ),
                        0.0,
                        bg,
                    );
                }
            }
            Edge::Left | Edge::Right => {
                for y in 0..h {
                    let idx = y as f32 / teeth;
                    let tooth_n = idx.floor() as u32;
                    let frac = idx - idx.floor();
                    let tri = 1.0 - (2.0 * frac - 1.0).abs();
                    let j = 1.0 + jitter(tooth_n);
                    let d = (depth * tri * j).max(0.0);
                    if d <= 0.0 {
                        continue;
                    }
                    let row_min_y = canvas_rect.min.y + y as f32 * scale;
                    let row_max_y = canvas_rect.min.y + (y + 1) as f32 * scale;
                    let (tx0, tx1) = match e.edge {
                        Edge::Left => (canvas_rect.min.x, canvas_rect.min.x + d * scale),
                        Edge::Right => (canvas_rect.max.x - d * scale, canvas_rect.max.x),
                        _ => unreachable!(),
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(tx0, row_min_y),
                            egui::pos2(tx1, row_max_y),
                        ),
                        0.0,
                        bg,
                    );
                }
            }
        }
    }
}

fn blur_key(rect: [f32; 4], radius: f32) -> BlurKey {
    let q = |v: f32| ((v / 8.0).round() as i32) * 8;
    let x0 = q(rect[0].min(rect[2]));
    let y0 = q(rect[1].min(rect[3]));
    let x1 = q(rect[0].max(rect[2]));
    let y1 = q(rect[1].max(rect[3]));
    let radius_q = ((radius / 2.0).round() as i32) * 2;
    BlurKey { x0, y0, x1, y1, radius_q }
}

/// Rasterize the capture-info banner into a transparent RGBA buffer sized
/// tightly to the banner contents, upload as a texture, and return it plus
/// the pixel size. The caller paints it scaled to the canvas.
fn build_capture_info_texture(
    ctx: &egui::Context,
    metadata: &CaptureMetadata,
    fields: &[FieldKind],
    position: CaptureInfoPosition,
    style: CaptureInfoStyle,
) -> Option<(egui::TextureHandle, [u32; 2])> {
    let lines = rasterize::capture_info_lines(Some(metadata), fields);
    if lines.is_empty() {
        return None;
    }
    // Determine the banner's own pixel size by drawing into a throwaway
    // TopLeft banner on a canvas large enough to hold it, then cropping
    // back to just the banner pixels. We size the scratch canvas generously
    // and rely on `draw_capture_info`'s own sizing logic.
    let pad = style.padding.max(0.0).round() as u32 * 2 + 4;
    let size_hint = style.text_size.max(6.0).round() as u32;
    let scratch_w = (256u32 + pad).max(64);
    let scratch_h = ((size_hint + 4) * lines.len() as u32 + pad).max(32);
    // Probe with a wide scratch, then measure by looking for non-zero
    // alpha in the TopLeft corner.
    let mut canvas = RgbaImage::from_pixel(
        (scratch_w * 2).max(320),
        (scratch_h * 2).max(160),
        image::Rgba([0, 0, 0, 0]),
    );
    rasterize::draw_capture_info(
        &mut canvas,
        Some(metadata),
        CaptureInfoPosition::TopLeft,
        fields,
        style,
    );
    // Find the bounding box of non-zero-alpha pixels — this is the banner.
    let (cw, ch) = canvas.dimensions();
    let mut max_x: u32 = 0;
    let mut max_y: u32 = 0;
    let mut found = false;
    for y in 0..ch {
        for x in 0..cw {
            if canvas.get_pixel(x, y).0[3] > 0 {
                if x > max_x { max_x = x; }
                if y > max_y { max_y = y; }
                found = true;
            }
        }
    }
    if !found {
        return None;
    }
    let bw = max_x + 1;
    let bh = max_y + 1;

    // Crop to just the banner.
    let mut banner = RgbaImage::new(bw, bh);
    for y in 0..bh {
        for x in 0..bw {
            banner.put_pixel(x, y, *canvas.get_pixel(x, y));
        }
    }
    let _ = position; // Position is used at paint-time, not here.
    let ci = egui::ColorImage::from_rgba_unmultiplied(
        [bw as usize, bh as usize],
        banner.as_raw(),
    );
    Some((
        ctx.load_texture("grabit-capture-info", ci, egui::TextureOptions::LINEAR),
        [bw, bh],
    ))
}

fn build_blur_texture(
    ctx: &egui::Context,
    base: &RgbaImage,
    rect: [f32; 4],
    radius: f32,
) -> Option<egui::TextureHandle> {
    let (bw, bh) = base.dimensions();
    let x0 = rect[0].min(rect[2]).floor().max(0.0) as u32;
    let y0 = rect[1].min(rect[3]).floor().max(0.0) as u32;
    let x1 = (rect[0].max(rect[2]).ceil() as u32).min(bw);
    let y1 = (rect[1].max(rect[3]).ceil() as u32).min(bh);
    if x1 <= x0 || y1 <= y0 { return None; }
    let w = x1 - x0;
    let h = y1 - y0;
    let mut crop = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            crop.put_pixel(x, y, *base.get_pixel(x0 + x, y0 + y));
        }
    }
    let sigma = radius.max(0.5);
    let blurred = imageproc::filter::gaussian_blur_f32(&crop, sigma);
    let ci = egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        blurred.as_raw(),
    );
    Some(ctx.load_texture("grabit-blur", ci, egui::TextureOptions::LINEAR))
}

impl EditorApp {
    fn show_close_prompt(&mut self, ctx: &egui::Context) {
        #[derive(Clone, Copy)]
        enum Action { Save, Discard, Cancel }
        let mut action: Option<Action> = None;

        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("You have unsaved changes. Save before closing?");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() { action = Some(Action::Save); }
                    if ui.button("Discard").clicked() { action = Some(Action::Discard); }
                    if ui.button("Cancel").clicked() { action = Some(Action::Cancel); }
                });
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            action = Some(Action::Cancel);
        }

        match action {
            Some(Action::Save) => match self.save() {
                Ok(()) => {
                    self.close_prompt_shown = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => {
                    self.status = format!("Save failed: {e}");
                    self.close_prompt_shown = false;
                }
            },
            Some(Action::Discard) => {
                self.dirty = false;
                self.saved_once = true;
                self.close_prompt_shown = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(Action::Cancel) => {
                self.close_prompt_shown = false;
            }
            None => {}
        }
    }
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_image_loaded(ctx);

        // Intercept window-close while there are unsaved edits and show a
        // Save / Discard / Cancel modal. Once dirty flips to false (Save
        // succeeds or Discard picked) the next close passes through.
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_prompt_shown = true;
        }
        if self.close_prompt_shown {
            self.show_close_prompt(ctx);
        }

        // Keyboard shortcuts.
        let (do_undo, do_redo, do_save, do_delete, do_rotate) = ctx.input_mut(|i| {
            let undo = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL, egui::Key::Z,
            ));
            // Redo: Ctrl+Shift+Z OR Ctrl+Y.
            let redo = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z,
            )) || i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL, egui::Key::Y,
            ));
            let save = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL, egui::Key::S,
            ));
            let del = i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace);
            // Shift+R rotates the base 90° CW. Pure R would clash with a
            // future typed-into-text shortcut; Shift+R is clearly modal.
            let rotate = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::SHIFT, egui::Key::R,
            ));
            (undo, redo, save, del, rotate)
        });

        if do_undo { self.undo(); }
        if do_redo { self.redo(); }
        if do_rotate && self.pending_text.is_none() { self.rotate_base_cw(); }
        if do_save {
            if let Err(e) = self.save() {
                self.status = format!("Save failed: {e}");
            }
        }
        // Only delete when nothing else is focused (so Backspace in the
        // text editor doesn't wipe the selection).
        if do_delete && self.pending_text.is_none() && self.selection.is_some() {
            self.delete_selection();
        }

        egui::TopBottomPanel::top("grabit-toolbar").show(ctx, |ui| self.toolbar(ui));
        egui::SidePanel::right("grabit-document-effects")
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.inspector_tab,
                        InspectorTab::Document,
                        "Document",
                    );
                    ui.selectable_value(
                        &mut self.inspector_tab,
                        InspectorTab::Presets,
                        "Presets",
                    );
                    ui.selectable_value(
                        &mut self.inspector_tab,
                        InspectorTab::Styles,
                        "Styles",
                    );
                });
                ui.separator();
                match self.inspector_tab {
                    InspectorTab::Document => self.document_panel(ui),
                    InspectorTab::Presets => self.presets_panel(ui),
                    InspectorTab::Styles => self.styles_panel(ui),
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| self.canvas(ui));
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty {
            debug!("editor closing with unsaved changes; persisting now");
            let _ = self.save();
        } else if !self.saved_once {
            let _ = self.save();
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Drawing helpers for the live preview. These all use `egui::Painter`
// primitives — no per-frame RgbaImage rasterisation, so 50 annotations at
// 4K stays well above 30 fps.
// ───────────────────────────────────────────────────────────────────────────

fn draw_node_preview(
    painter: &egui::Painter,
    node: &AnnotationNode,
    scale: f32,
    to_canvas: &dyn Fn([f32; 2]) -> egui::Pos2,
) {
    match node {
        AnnotationNode::Arrow { start, end, color, thickness, .. } => {
            draw_arrow_preview(
                painter,
                to_canvas(*start),
                to_canvas(*end),
                egui_color(*color),
                thickness * scale,
            );
        }
        AnnotationNode::Text { position, text, color, size_px, .. } => {
            painter.text(
                to_canvas(*position),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::monospace(size_px * scale),
                egui_color(*color),
            );
        }
        AnnotationNode::Callout {
            rect, tail, text, fill, stroke, stroke_width, text_color, text_size, ..
        } => {
            let r = egui::Rect::from_two_pos(
                to_canvas([rect[0], rect[1]]),
                to_canvas([rect[2], rect[3]]),
            );
            painter.rect_filled(r, 4.0, egui_color(*fill));
            painter.rect_stroke(
                r, 4.0,
                egui::Stroke::new((*stroke_width * scale).max(1.0), egui_color(*stroke)),
            );
            // Tail: a triangle from balloon edge to the tail tip.
            let cx = 0.5 * (rect[0] + rect[2]);
            let cy = 0.5 * (rect[1] + rect[3]);
            let dx = tail[0] - cx;
            let dy = tail[1] - cy;
            let half_w = 0.5 * (rect[2] - rect[0]).abs();
            let half_h = 0.5 * (rect[3] - rect[1]).abs();
            let base_half = (half_w.min(half_h) * 0.35).max(6.0);
            let tri = if dy.abs() * half_w >= dx.abs() * half_h {
                let y = if dy >= 0.0 { rect[3] } else { rect[1] };
                let tx = cx + dx.signum() * base_half;
                vec![
                    to_canvas([cx - base_half * 0.5, y]),
                    to_canvas([tx, y]),
                    to_canvas(*tail),
                ]
            } else {
                let x = if dx >= 0.0 { rect[2] } else { rect[0] };
                let ty = cy + dy.signum() * base_half;
                vec![
                    to_canvas([x, cy - base_half * 0.5]),
                    to_canvas([x, ty]),
                    to_canvas(*tail),
                ]
            };
            painter.add(egui::Shape::convex_polygon(tri, egui_color(*fill), egui::Stroke::new((*stroke_width * scale).max(1.0), egui_color(*stroke))));
            painter.text(
                to_canvas([rect[0] + text_size * 0.35, rect[1] + text_size * 0.35]),
                egui::Align2::LEFT_TOP,
                text,
                egui::FontId::monospace(text_size * scale),
                egui_color(*text_color),
            );
        }
        AnnotationNode::Shape { shape, rect, stroke, stroke_width, fill, .. } => {
            let r = egui::Rect::from_two_pos(
                to_canvas([rect[0], rect[1]]),
                to_canvas([rect[2], rect[3]]),
            );
            match shape {
                ShapeKind::Rect => {
                    if fill[3] > 0 {
                        painter.rect_filled(r, 0.0, egui_color(*fill));
                    }
                    painter.rect_stroke(
                        r, 0.0,
                        egui::Stroke::new((*stroke_width * scale).max(1.0), egui_color(*stroke)),
                    );
                }
                ShapeKind::Ellipse => {
                    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(48);
                    let cx = r.center().x;
                    let cy = r.center().y;
                    let rx = 0.5 * r.width();
                    let ry = 0.5 * r.height();
                    for i in 0..48 {
                        let a = (i as f32) * std::f32::consts::TAU / 48.0;
                        pts.push(egui::pos2(cx + rx * a.cos(), cy + ry * a.sin()));
                    }
                    if fill[3] > 0 {
                        painter.add(egui::Shape::convex_polygon(
                            pts.clone(), egui_color(*fill),
                            egui::Stroke::new((*stroke_width * scale).max(1.0), egui_color(*stroke)),
                        ));
                    } else {
                        pts.push(pts[0]);
                        painter.add(egui::Shape::line(
                            pts,
                            egui::Stroke::new((*stroke_width * scale).max(1.0), egui_color(*stroke)),
                        ));
                    }
                }
            }
        }
        AnnotationNode::Step { center, radius, number, fill, text_color, .. } => {
            let c = to_canvas(*center);
            let r = radius * scale;
            painter.circle_filled(c, r, egui_color(*fill));
            painter.circle_stroke(
                c, r,
                egui::Stroke::new((r * 0.08).max(1.0), egui_color(darken(*fill))),
            );
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                number.to_string(),
                egui::FontId::monospace(r * 1.2),
                egui_color(*text_color),
            );
        }
        AnnotationNode::Stamp { source, rect, .. } => {
            // Previewing the stamp as a filled rect with a dashed outline is
            // good enough — the actual PNG is drawn only on flatten/export.
            // Drawing the stamp pixels in the preview would require
            // uploading another texture per stamp; not worth it at M3.
            let r = egui::Rect::from_two_pos(
                to_canvas([rect[0], rect[1]]),
                to_canvas([rect[2], rect[3]]),
            );
            let label = match source {
                StampSource::Builtin { name } => name.as_str(),
                StampSource::Inline { .. } => "inline",
            };
            painter.rect_stroke(
                r, 2.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 180, 40)),
            );
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                format!("\u{2605} {label}"),
                egui::FontId::monospace((r.height() * 0.25).clamp(10.0, 28.0)),
                egui::Color32::from_rgb(255, 200, 80),
            );
        }
        AnnotationNode::Blur { rect, .. } => {
            // Stippled overlay: filled translucent rect + dashed outline.
            // This hints at the blur without a per-frame gaussian blur.
            let r = egui::Rect::from_two_pos(
                to_canvas([rect[0], rect[1]]),
                to_canvas([rect[2], rect[3]]),
            );
            painter.rect_filled(
                r, 0.0,
                egui::Color32::from_rgba_unmultiplied(120, 140, 200, 100),
            );
            painter.rect_stroke(
                r, 0.0,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(120, 140, 200)),
            );
            // Pseudo-pixellated hint: draw a short "BLUR" label.
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "BLUR",
                egui::FontId::monospace((r.height() * 0.25).clamp(10.0, 28.0)),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220),
            );
        }
        AnnotationNode::CaptureInfo { position, fields, style, .. } => {
            // Preview anchor: a small corner marker + a sample of what the
            // baked banner will look like (monospace, no real metadata
            // because we don't have it in this function).
            let _ = (fields, style);
            let label = match position {
                CaptureInfoPosition::TopLeft => "INFO \u{25F0}",
                CaptureInfoPosition::TopRight => "INFO \u{25F1}",
                CaptureInfoPosition::BottomLeft => "INFO \u{25F2}",
                CaptureInfoPosition::BottomRight => "INFO \u{25F3}",
            };
            // Draw a small indicator in the target corner.
            let img_rect = painter.clip_rect();
            let anchor = match position {
                CaptureInfoPosition::TopLeft => img_rect.left_top() + egui::vec2(8.0, 8.0),
                CaptureInfoPosition::TopRight => img_rect.right_top() + egui::vec2(-80.0, 8.0),
                CaptureInfoPosition::BottomLeft => img_rect.left_bottom() + egui::vec2(8.0, -26.0),
                CaptureInfoPosition::BottomRight => img_rect.right_bottom() + egui::vec2(-80.0, -26.0),
            };
            let _ = scale;
            painter.text(
                anchor,
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(240, 240, 240),
            );
        }
        AnnotationNode::Magnify {
            source_rect, target_rect, border, border_width, circular, ..
        } => {
            let dst = egui::Rect::from_two_pos(
                to_canvas([target_rect[0], target_rect[1]]),
                to_canvas([target_rect[2], target_rect[3]]),
            );
            let src = egui::Rect::from_two_pos(
                to_canvas([source_rect[0], source_rect[1]]),
                to_canvas([source_rect[2], source_rect[3]]),
            );
            // Source rect: dashed yellow. We approximate dashing as a thin
            // solid stroke to stay simple.
            painter.rect_stroke(src, 0.0, egui::Stroke::new(
                1.5, egui::Color32::from_rgb(255, 220, 60),
            ));
            // Target rect: filled translucent indicator + border.
            if *circular {
                let mut pts: Vec<egui::Pos2> = Vec::with_capacity(48);
                let cx = dst.center().x;
                let cy = dst.center().y;
                let rx = 0.5 * dst.width();
                let ry = 0.5 * dst.height();
                for i in 0..48 {
                    let a = (i as f32) * std::f32::consts::TAU / 48.0;
                    pts.push(egui::pos2(cx + rx * a.cos(), cy + ry * a.sin()));
                }
                pts.push(pts[0]);
                painter.add(egui::Shape::line(pts, egui::Stroke::new(
                    (border_width * scale).max(1.0),
                    egui_color(*border),
                )));
            } else {
                painter.rect_stroke(
                    dst, 0.0,
                    egui::Stroke::new((border_width * scale).max(1.0), egui_color(*border)),
                );
            }
            // Connector line from source to target centre.
            painter.line_segment(
                [src.center(), dst.center()],
                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 220, 60, 180)),
            );
        }
    }
}

fn draw_arrow_preview(
    painter: &egui::Painter,
    start: egui::Pos2,
    end: egui::Pos2,
    color: egui::Color32,
    thickness_px: f32,
) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 { return; }
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;

    let head_len = (thickness_px * 4.0).max(10.0).min(len * 0.45);
    let head_half = head_len * 0.55;
    let shaft_end = egui::pos2(end.x - ux * head_len, end.y - uy * head_len);
    let ht = (thickness_px * 0.5).max(0.5);

    let shaft = vec![
        egui::pos2(start.x + px * ht, start.y + py * ht),
        egui::pos2(start.x - px * ht, start.y - py * ht),
        egui::pos2(shaft_end.x - px * ht, shaft_end.y - py * ht),
        egui::pos2(shaft_end.x + px * ht, shaft_end.y + py * ht),
    ];
    painter.add(egui::Shape::convex_polygon(shaft, color, egui::Stroke::NONE));

    let head = vec![
        end,
        egui::pos2(shaft_end.x + px * head_half, shaft_end.y + py * head_half),
        egui::pos2(shaft_end.x - px * head_half, shaft_end.y - py * head_half),
    ];
    painter.add(egui::Shape::convex_polygon(head, color, egui::Stroke::NONE));
}

fn apply_drag_to_node(node: &mut AnnotationNode, ah: &ActiveHandle, dx: f32, dy: f32) {
    match node {
        AnnotationNode::Arrow { start, end, .. } => {
            match ah.handle {
                Handle::ArrowStart => {
                    start[0] = ah.start_rect[0] + dx;
                    start[1] = ah.start_rect[1] + dy;
                }
                Handle::ArrowEnd => {
                    end[0] = ah.start_rect[2] + dx;
                    end[1] = ah.start_rect[3] + dy;
                }
                Handle::Body => {
                    start[0] = ah.start_rect[0] + dx;
                    start[1] = ah.start_rect[1] + dy;
                    end[0] = ah.start_rect[2] + dx;
                    end[1] = ah.start_rect[3] + dy;
                }
                _ => {}
            }
        }
        AnnotationNode::Text { position, .. } => {
            if ah.handle == Handle::Body {
                position[0] = ah.start_rect[0] + dx;
                position[1] = ah.start_rect[1] + dy;
            }
        }
        AnnotationNode::Callout { rect, tail, .. } => {
            if ah.handle == Handle::CalloutTail {
                tail[0] = ah.start_tail.map(|t| t[0]).unwrap_or(tail[0]) + dx;
                tail[1] = ah.start_tail.map(|t| t[1]).unwrap_or(tail[1]) + dy;
            } else {
                *rect = drag_rect(ah.start_rect, ah.handle, dx, dy);
            }
        }
        AnnotationNode::Shape { rect, .. }
        | AnnotationNode::Stamp { rect, .. }
        | AnnotationNode::Blur { rect, .. } => {
            *rect = drag_rect(ah.start_rect, ah.handle, dx, dy);
        }
        AnnotationNode::CaptureInfo { .. } => {
            // Banner position is anchor-based, not rect-based. No drag.
        }
        AnnotationNode::Step { center, radius, .. } => {
            // Body-drag moves center; any corner handle scales radius by the
            // larger delta. Keep this simple: recompute bbox then derive.
            let new_rect = drag_rect(ah.start_rect, ah.handle, dx, dy);
            center[0] = 0.5 * (new_rect[0] + new_rect[2]);
            center[1] = 0.5 * (new_rect[1] + new_rect[3]);
            let w = 0.5 * (new_rect[2] - new_rect[0]).abs();
            let h = 0.5 * (new_rect[3] - new_rect[1]).abs();
            if ah.handle != Handle::Body {
                *radius = w.max(h).max(4.0);
            }
        }
        AnnotationNode::Magnify { source_rect, target_rect, .. } => {
            match ah.handle {
                Handle::MagnifySource => {
                    let start = ah.start_source.unwrap_or(*source_rect);
                    source_rect[0] = start[0] + dx;
                    source_rect[1] = start[1] + dy;
                    source_rect[2] = start[2] + dx;
                    source_rect[3] = start[3] + dy;
                }
                _ => {
                    *target_rect = drag_rect(ah.start_rect, ah.handle, dx, dy);
                }
            }
        }
    }
}

fn hit_node(node: &AnnotationNode, p: [f32; 2]) -> bool {
    match node {
        AnnotationNode::Arrow { start, end, thickness, .. } => {
            let tol = (thickness * 0.5 + 4.0).powi(2);
            dist2_to_segment(p, *start, *end) <= tol
        }
        _ => bounds_of_node(node).map(|b| hit_bbox(p, b)).unwrap_or(false),
    }
}

fn color_to_rgba(c: egui::Color32) -> [u8; 4] { [c.r(), c.g(), c.b(), c.a()] }
fn color_to_rgba_alpha(c: egui::Color32, a: u8) -> [u8; 4] { [c.r(), c.g(), c.b(), a] }
fn egui_color(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn color32_to_rgba(c: egui::Color32) -> [u8; 4] { [c.r(), c.g(), c.b(), c.a()] }
fn rgba_to_color32(c: [u8; 4]) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

/// Map an editor `Tool` to the subset of `StyleToolKind`s we recognise.
/// Tools without meaningful style knobs (Select, CaptureInfo) return
/// `None` and show a "select a styleable tool" prompt instead.
fn tool_to_style_kind(tool: Tool) -> Option<StyleToolKind> {
    Some(match tool {
        Tool::Arrow => StyleToolKind::Arrow,
        Tool::Text => StyleToolKind::Text,
        Tool::Callout => StyleToolKind::Callout,
        Tool::Rect => StyleToolKind::Rect,
        Tool::Ellipse => StyleToolKind::Ellipse,
        Tool::Step => StyleToolKind::Step,
        Tool::Blur => StyleToolKind::Blur,
        Tool::Magnify => StyleToolKind::Magnify,
        Tool::Select | Tool::CaptureInfo => return None,
    })
}

fn style_kind_to_tool(kind: StyleToolKind) -> Option<Tool> {
    Some(match kind {
        StyleToolKind::Arrow => Tool::Arrow,
        StyleToolKind::Text => Tool::Text,
        StyleToolKind::Callout => Tool::Callout,
        StyleToolKind::Rect => Tool::Rect,
        StyleToolKind::Ellipse => Tool::Ellipse,
        StyleToolKind::Step => Tool::Step,
        StyleToolKind::Blur => Tool::Blur,
        StyleToolKind::Magnify => Tool::Magnify,
    })
}

/// Actions buffered during a presets-panel iteration, to avoid mutating
/// `self.preset_store` while we're iterating it.
enum PresetAction {
    Edit(usize),
    Duplicate(usize),
    Delete(usize),
    CaptureNow(String),
}

fn darken(c: [u8; 4]) -> [u8; 4] {
    [
        (c[0] as f32 * 0.5) as u8,
        (c[1] as f32 * 0.5) as u8,
        (c[2] as f32 * 0.5) as u8,
        c[3],
    ]
}

fn dist_sq(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

// ───────────────────────────────────────────────────────────────────────────
// Clipboard copy for annotated PNG on Save.
// Duplicates a bit of `crate::export::copy_to_clipboard` because that path
// expects a `CaptureResult`; here we already have a flattened RgbaImage.
// ───────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn copy_rgba_to_clipboard(img: &RgbaImage) -> Result<()> {
    use anyhow::anyhow;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::Win32::System::Ole::CF_DIB;

    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(anyhow!("empty image"));
    }
    let stride = (w as usize) * 4;
    let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
    let pixel_bytes = stride * (h as usize);
    let total = header_size + pixel_bytes;

    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total).map_err(|e| anyhow!("GlobalAlloc: {e}"))?;
        if hmem.0.is_null() {
            return Err(anyhow!("GlobalAlloc null"));
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            return Err(anyhow!("GlobalLock failed"));
        }

        let hdr = BITMAPINFOHEADER {
            biSize: header_size as u32,
            biWidth: w as i32,
            biHeight: h as i32,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: pixel_bytes as u32,
            biXPelsPerMeter: 2835,
            biYPelsPerMeter: 2835,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        std::ptr::write(ptr as *mut BITMAPINFOHEADER, hdr);

        let src = img.as_raw();
        let dst_pixels = ptr.add(header_size);
        for y in 0..h as usize {
            let src_row = &src[y * stride..(y + 1) * stride];
            let dst_y = (h as usize - 1) - y;
            let dst_row = dst_pixels.add(dst_y * stride);
            for x in 0..w as usize {
                let s = &src_row[x * 4..x * 4 + 4];
                let d = dst_row.add(x * 4);
                *d.add(0) = s[2];
                *d.add(1) = s[1];
                *d.add(2) = s[0];
                *d.add(3) = s[3];
            }
        }
        let _ = GlobalUnlock(hmem);

        if OpenClipboard(None).is_err() {
            return Err(anyhow!("OpenClipboard failed"));
        }
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) { unsafe { let _ = CloseClipboard(); } }
        }
        let _g = Guard;

        if EmptyClipboard().is_err() {
            return Err(anyhow!("EmptyClipboard failed"));
        }
        let handle = HANDLE(hmem.0 as *mut _);
        if SetClipboardData(CF_DIB.0 as u32, handle).is_err() {
            return Err(anyhow!("SetClipboardData failed"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn copy_rgba_to_clipboard(_img: &RgbaImage) -> Result<()> {
    Ok(())
}
