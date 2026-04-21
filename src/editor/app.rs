//! eframe-based editor window. Minimum viable surface for M2 + M3:
//!
//! - Renders the captured base image on a canvas.
//! - Arrow tool with click-and-drag placement (color + thickness adjustable).
//! - Undo (Ctrl+Z) via simple Document snapshots.
//! - Save (Ctrl+S or toolbar button) writes an annotated PNG + `.grabit`
//!   sidecar. Optionally copies the annotated PNG to the clipboard.
//!
//! Pan/zoom, selection, and post-placement editing are deferred — the goal
//! here is "draw arrows on a screenshot and save it" with no ceremony.

use crate::editor::document::{AnnotationNode, Document};
use crate::editor::rasterize;
use anyhow::{Context, Result};
use eframe::egui;
use image::RgbaImage;
use log::{debug, info, warn};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Arrow,
    Text,
}

/// Text annotation in the middle of being typed. Once committed (Enter or
/// click-outside) it becomes an `AnnotationNode::Text`; Escape discards it.
struct PendingText {
    position: [f32; 2],
    buffer: String,
}

pub struct EditorApp {
    document: Document,
    /// Where to write the flattened PNG on Save.
    png_path: PathBuf,
    /// Where to write the `.grabit` sidecar.
    grabit_path: PathBuf,
    /// Whether to copy the flattened PNG to the clipboard on Save.
    copy_to_clipboard: bool,

    tool: Tool,
    /// Arrow in progress: (start, current) in image-pixel coordinates.
    pending: Option<([f32; 2], [f32; 2])>,
    /// Text annotation currently being typed.
    pending_text: Option<PendingText>,

    /// Currently selected draw color (sRGB RGBA).
    color: egui::Color32,
    /// Arrow stroke thickness in image pixels.
    thickness: f32,
    /// Text size in image pixels.
    text_size: f32,

    /// Snapshot stack for undo. M3 uses coarse snapshots; M5 can replace
    /// this with the command-pattern undo described in the plan.
    undo: Vec<Vec<AnnotationNode>>,

    /// Base image texture, loaded lazily on first frame.
    texture: Option<egui::TextureHandle>,
    /// Decoded base image cached so Save doesn't redecode from the PNG blob.
    base_rgba: Option<RgbaImage>,

    dirty: bool,
    saved_once: bool,
    status: String,
}

impl EditorApp {
    pub fn new(
        document: Document,
        png_path: PathBuf,
        grabit_path: PathBuf,
        copy_to_clipboard: bool,
    ) -> Self {
        Self {
            document,
            png_path,
            grabit_path,
            copy_to_clipboard,
            tool: Tool::Arrow,
            pending: None,
            pending_text: None,
            color: egui::Color32::from_rgb(220, 40, 40),
            thickness: 6.0,
            text_size: 28.0,
            undo: Vec::new(),
            texture: None,
            base_rgba: None,
            dirty: false,
            saved_once: false,
            status: String::new(),
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

    fn push_undo(&mut self) {
        self.undo.push(self.document.annotations.clone());
        // Cap history to prevent unbounded growth.
        const CAP: usize = 64;
        if self.undo.len() > CAP {
            let excess = self.undo.len() - CAP;
            self.undo.drain(0..excess);
        }
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.document.annotations = prev;
            self.dirty = true;
        }
    }

    fn clear_all(&mut self) {
        if self.document.annotations.is_empty() {
            return;
        }
        self.push_undo();
        self.document.annotations.clear();
    }

    fn commit_pending_text(&mut self) {
        if let Some(pt) = self.pending_text.take() {
            let trimmed = pt.buffer.trim();
            if trimmed.is_empty() {
                return;
            }
            self.push_undo();
            self.document.annotations.push(AnnotationNode::Text {
                id: Uuid::new_v4(),
                position: pt.position,
                text: trimmed.to_string(),
                color: color_to_rgba(self.color),
                size_px: self.text_size,
            });
        }
    }

    fn cancel_pending_text(&mut self) {
        self.pending_text = None;
    }

    fn save(&mut self) -> Result<()> {
        let base = self
            .base_rgba
            .as_ref()
            .context("base image not decoded")?
            .clone();
        let flat = rasterize::flatten(&base, &self.document.annotations);

        flat.save_with_format(&self.png_path, image::ImageFormat::Png)
            .with_context(|| format!("write {}", self.png_path.display()))?;
        info!("saved {}", self.png_path.display());

        // `.grabit` sidecar preserves annotations for future editing.
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

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .selectable_value(&mut self.tool, Tool::Arrow, "Arrow")
                .clicked()
            {
                self.cancel_pending_text();
            }
            ui.selectable_value(&mut self.tool, Tool::Text, "Text");
            ui.separator();

            ui.label("Color");
            ui.color_edit_button_srgba(&mut self.color);

            match self.tool {
                Tool::Arrow => {
                    ui.label("Thickness");
                    ui.add(egui::Slider::new(&mut self.thickness, 1.0..=40.0));
                }
                Tool::Text => {
                    ui.label("Text size");
                    ui.add(egui::Slider::new(&mut self.text_size, 8.0..=128.0));
                }
            }

            ui.separator();

            let undo_enabled = !self.undo.is_empty();
            if ui
                .add_enabled(undo_enabled, egui::Button::new("Undo (Ctrl+Z)"))
                .clicked()
            {
                self.undo();
            }

            let clear_enabled = !self.document.annotations.is_empty();
            if ui
                .add_enabled(clear_enabled, egui::Button::new("Clear"))
                .clicked()
            {
                self.clear_all();
            }

            ui.separator();

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
        let scale = (available.x / img_w).min(available.y / img_h).min(1.0).max(0.01);
        let display = egui::vec2(img_w * scale, img_h * scale);

        let (response, painter) = ui.allocate_painter(display, egui::Sense::click_and_drag());
        let rect = response.rect;

        // Draw base image.
        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // Helper: canvas screen-pos → image-pixel coord.
        let to_image = |p: egui::Pos2| -> [f32; 2] {
            let local = p - rect.min;
            [
                (local.x / scale).clamp(0.0, img_w),
                (local.y / scale).clamp(0.0, img_h),
            ]
        };
        // Helper: image coord → canvas screen-pos.
        let to_canvas = |p: [f32; 2]| -> egui::Pos2 {
            egui::pos2(rect.min.x + p[0] * scale, rect.min.y + p[1] * scale)
        };

        // Handle input.
        match self.tool {
            Tool::Arrow => {
                if response.drag_started() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let p = to_image(pos);
                        self.pending = Some((p, p));
                    }
                }
                if response.dragged() {
                    if let (Some(current_screen), Some(pending)) =
                        (response.interact_pointer_pos(), self.pending.as_mut())
                    {
                        pending.1 = to_image(current_screen);
                    }
                }
                if response.drag_stopped() {
                    if let Some((s, e)) = self.pending.take() {
                        let dx = e[0] - s[0];
                        let dy = e[1] - s[1];
                        if dx * dx + dy * dy >= 4.0 {
                            self.push_undo();
                            self.document.annotations.push(AnnotationNode::Arrow {
                                id: Uuid::new_v4(),
                                start: s,
                                end: e,
                                color: color_to_rgba(self.color),
                                thickness: self.thickness,
                            });
                        }
                    }
                }
            }
            Tool::Text => {
                if response.clicked() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        // Commit anything the user was already typing before
                        // starting a new text at the clicked point.
                        self.commit_pending_text();
                        self.pending_text = Some(PendingText {
                            position: to_image(pos),
                            buffer: String::new(),
                        });
                    }
                }
            }
        }

        // Draw existing annotations.
        for node in &self.document.annotations {
            match node {
                AnnotationNode::Arrow { start, end, color, thickness, .. } => {
                    draw_arrow_preview(
                        &painter,
                        to_canvas(*start),
                        to_canvas(*end),
                        egui::Color32::from_rgba_unmultiplied(
                            color[0], color[1], color[2], color[3],
                        ),
                        thickness * scale,
                    );
                }
                AnnotationNode::Text { position, text, color, size_px, .. } => {
                    let canvas_size = size_px * scale;
                    painter.text(
                        to_canvas(*position),
                        egui::Align2::LEFT_TOP,
                        text,
                        egui::FontId::monospace(canvas_size),
                        egui::Color32::from_rgba_unmultiplied(
                            color[0], color[1], color[2], color[3],
                        ),
                    );
                }
            }
        }

        // Draw in-progress arrow.
        if let Some((s, e)) = self.pending {
            draw_arrow_preview(
                &painter,
                to_canvas(s),
                to_canvas(e),
                self.color,
                self.thickness * scale,
            );
        }

        // Floating editor for in-progress text. We show this AFTER painting
        // the canvas so it overlays the base image.
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

            // Commit on Enter (TextEdit::singleline loses focus on Enter),
            // cancel on Escape.
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
}

impl eframe::App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_image_loaded(ctx);

        // Keyboard shortcuts.
        ctx.input_mut(|i| {
            if i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::Z,
            )) {
                self.undo();
            }
            if i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::CTRL,
                egui::Key::S,
            )) {
                if let Err(e) = self.save() {
                    self.status = format!("Save failed: {e}");
                }
            }
        });

        egui::TopBottomPanel::top("grabit-toolbar").show(ctx, |ui| self.toolbar(ui));

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::both().show(ui, |ui| self.canvas(ui));
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty {
            debug!("editor closing with unsaved changes; persisting now");
            let _ = self.save();
        } else if !self.saved_once {
            // Always write at least one PNG — the capture produced value the
            // user would expect to see on disk. If they never annotate, this
            // mirrors the non-editor capture flow.
            let _ = self.save();
        }
    }
}

fn color_to_rgba(c: egui::Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), c.a()]
}

fn draw_arrow_preview(
    painter: &egui::Painter,
    start: egui::Pos2,
    end: egui::Pos2,
    color: egui::Color32,
    thickness_px: f32,
) {
    use egui::Shape;
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let ux = dx / len;
    let uy = dy / len;
    let px = -uy;
    let py = ux;

    let head_len = (thickness_px * 4.0).max(10.0).min(len * 0.45);
    let head_half = head_len * 0.55;
    let shaft_end = egui::pos2(end.x - ux * head_len, end.y - uy * head_len);
    let ht = (thickness_px * 0.5).max(0.5);

    // Shaft polygon
    let shaft = vec![
        egui::pos2(start.x + px * ht, start.y + py * ht),
        egui::pos2(start.x - px * ht, start.y - py * ht),
        egui::pos2(shaft_end.x - px * ht, shaft_end.y - py * ht),
        egui::pos2(shaft_end.x + px * ht, shaft_end.y + py * ht),
    ];
    painter.add(Shape::convex_polygon(shaft, color, egui::Stroke::NONE));

    // Head triangle
    let head = vec![
        end,
        egui::pos2(shaft_end.x + px * head_half, shaft_end.y + py * head_half),
        egui::pos2(shaft_end.x - px * head_half, shaft_end.y - py * head_half),
    ];
    painter.add(Shape::convex_polygon(head, color, egui::Stroke::NONE));
}

// ─────────────────────────────────────────────────────────────────────────
// Clipboard copy for annotated PNG on Save.
// Duplicates a bit of `crate::export::copy_to_clipboard` because that path
// expects a `CaptureResult`; here we already have a flattened RgbaImage.
// ─────────────────────────────────────────────────────────────────────────

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
