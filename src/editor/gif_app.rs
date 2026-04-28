//! Frame editor for a recorded GIF sidecar.
//!
//! Loads `recording.json` plus its spool directory, presents a timeline UI
//! (preview, scrub, trim in/out, delete frames, FPS / loop tweaks), and on
//! Export streams the kept frames through `crate::export::gif::encode_to_gif`.
//! Cleans up the spool dir + sidecar on a successful export; leaves both on
//! disk if the user closes without exporting (so they can retry).

use crate::app::paths::AppPaths;
use crate::capture::gif_record::{GifSidecar, SidecarFrame};
use crate::settings::Settings;
use anyhow::{Context, Result};
use eframe::egui;
use log::{info, warn};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

/// Subprocess entry. Mirrors `editor::run_blocking` — initializes eframe on
/// the current (main) thread because winit 0.30 won't recreate its event
/// loop within a single process. We're already running as a fresh
/// `--gif-editor` subprocess so the main thread is free.
pub fn run_blocking(sidecar_path: PathBuf, paths: AppPaths, settings: Settings) -> Result<()> {
    let body = std::fs::read_to_string(&sidecar_path)
        .with_context(|| format!("read sidecar {}", sidecar_path.display()))?;
    let sidecar: GifSidecar = serde_json::from_str(&body)
        .with_context(|| format!("parse sidecar {}", sidecar_path.display()))?;

    let viewport = {
        let mut vb = egui::ViewportBuilder::default()
            .with_title("GrabIt — GIF editor")
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0]);
        if let Some(icon) = super::load_app_icon_data() {
            vb = vb.with_icon(Arc::new(icon));
        }
        vb
    };

    let options = eframe::NativeOptions {
        viewport,
        event_loop_builder: Some(Box::new(|builder| {
            #[cfg(windows)]
            {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                builder.with_any_thread(true);
            }
            #[cfg(not(windows))]
            {
                let _ = builder;
            }
        })),
        ..Default::default()
    };

    eframe::run_native(
        "GrabIt — GIF editor",
        options,
        Box::new(move |cc| {
            crate::editor::install_jetbrains_mono(&cc.egui_ctx);
            Ok(Box::new(GifEditorApp::new(
                sidecar,
                sidecar_path,
                paths,
                settings,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

#[derive(Debug, Clone)]
struct EditableFrame {
    /// Absolute on-disk path inside the spool dir.
    path: PathBuf,
    /// Inter-frame delay in ms. Edited by the FPS slider on Export, but
    /// stored verbatim here so a user-set custom FPS can be re-derived
    /// from the recorded count.
    delay_ms: u32,
    /// User toggled this frame off in the timeline. Excluded on Export.
    deleted: bool,
}

enum ExportStatus {
    Idle,
    Encoding { done: usize, total: usize },
    Done(PathBuf),
    Failed(String),
}

struct GifEditorApp {
    sidecar: GifSidecar,
    sidecar_path: PathBuf,
    paths: AppPaths,
    settings: Settings,
    frames: Vec<EditableFrame>,
    current: usize,
    fps: u32,
    loop_count: u16,
    /// Trim markers — bounds (inclusive). When set, "Trim to selection"
    /// drops any frame outside [in, out].
    trim_in: Option<usize>,
    trim_out: Option<usize>,
    playing: bool,
    last_advance: std::time::Instant,
    /// LRU of GPU textures keyed by frame index. The decode is owned by
    /// a background thread (see `decoded` / `decoder_rx`); this cache is
    /// strictly the GPU-resident half so memory stays bounded for long
    /// recordings without thrashing the PNG decoder during playback.
    preview_cache: PreviewCache,
    /// Pre-decoded RGBA per frame. `None` while the background decoder
    /// thread hasn't reached that index yet. Once populated, building
    /// the GPU texture is a synchronous-but-cheap upload.
    decoded: Vec<Option<Arc<egui::ColorImage>>>,
    decoder_rx: Option<mpsc::Receiver<(usize, Arc<egui::ColorImage>)>>,
    /// Cancels the decoder thread when the editor is dropped (or the
    /// thread otherwise needs to wind down) so it doesn't keep churning
    /// on a closed window's spool dir.
    decoder_cancel: Arc<AtomicBool>,
    decoded_count: usize,
    export: ExportStatus,
    export_rx: Option<mpsc::Receiver<ExportProgress>>,
    copy_to_clipboard_on_export: bool,
    status: String,
}

impl Drop for GifEditorApp {
    fn drop(&mut self) {
        self.decoder_cancel.store(true, Ordering::SeqCst);
    }
}

enum ExportProgress {
    Tick { done: usize, total: usize },
    Done(PathBuf),
    Failed(String),
}

struct PreviewCache {
    capacity: usize,
    order: VecDeque<usize>,
    map: std::collections::HashMap<usize, egui::TextureHandle>,
}

impl PreviewCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity + 1),
            map: std::collections::HashMap::with_capacity(capacity + 1),
        }
    }

    fn get(&mut self, idx: usize) -> Option<&egui::TextureHandle> {
        if !self.map.contains_key(&idx) {
            return None;
        }
        if let Some(pos) = self.order.iter().position(|&i| i == idx) {
            self.order.remove(pos);
        }
        self.order.push_back(idx);
        self.map.get(&idx)
    }

    fn insert(&mut self, idx: usize, tex: egui::TextureHandle) {
        if self.map.contains_key(&idx) {
            return;
        }
        if self.map.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.map.remove(&oldest);
            }
        }
        self.order.push_back(idx);
        self.map.insert(idx, tex);
    }
}

impl GifEditorApp {
    fn new(
        sidecar: GifSidecar,
        sidecar_path: PathBuf,
        paths: AppPaths,
        settings: Settings,
    ) -> Self {
        let frames: Vec<EditableFrame> = sidecar
            .frames
            .iter()
            .map(|f: &SidecarFrame| EditableFrame {
                path: sidecar.spool_dir.join(&f.file),
                delay_ms: f.delay_ms,
                deleted: false,
            })
            .collect();
        let fps = sidecar.fps_target.clamp(5, 60);
        let loop_count = sidecar.loop_count;
        let copy_to_clipboard_on_export = settings.copy_to_clipboard;

        // Kick off a background decoder. The UI thread drains its results
        // each frame in `drain_decoder`. Decoding all PNGs up front means
        // playback never has to do disk + decode work synchronously, which
        // is what makes preview look smooth.
        let cancel = Arc::new(AtomicBool::new(false));
        let decoder_rx = if frames.is_empty() {
            None
        } else {
            Some(spawn_decoder(
                frames.iter().map(|f| f.path.clone()).collect(),
                cancel.clone(),
            ))
        };

        let decoded = vec![None; frames.len()];

        Self {
            sidecar,
            sidecar_path,
            paths,
            settings,
            frames,
            current: 0,
            fps,
            loop_count,
            trim_in: None,
            trim_out: None,
            playing: false,
            last_advance: std::time::Instant::now(),
            // GPU texture cache. Bigger than before (decode is no longer
            // the bottleneck so the only cost of cache misses is a fast
            // upload), so a typical playback loop touches mostly hits.
            preview_cache: PreviewCache::new(64),
            decoded,
            decoder_rx,
            decoder_cancel: cancel,
            decoded_count: 0,
            export: ExportStatus::Idle,
            export_rx: None,
            copy_to_clipboard_on_export,
            status: String::new(),
        }
    }

    fn active_count(&self) -> usize {
        self.frames.iter().filter(|f| !f.deleted).count()
    }

    fn total_duration_ms(&self) -> u64 {
        self.frames
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| f.delay_ms as u64)
            .sum()
    }

    /// Pull whatever the background decoder has produced into the per-
    /// frame `decoded` slots. Bounded per-update so a fast decoder can't
    /// stall the UI thread on a bulk drain.
    fn drain_decoder(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.decoder_rx.as_ref() else { return };
        let mut received_any = false;
        for _ in 0..32 {
            match rx.try_recv() {
                Ok((idx, ci)) => {
                    if idx < self.decoded.len() && self.decoded[idx].is_none() {
                        self.decoded[idx] = Some(ci);
                        self.decoded_count += 1;
                        received_any = true;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.decoder_rx = None;
                    break;
                }
            }
        }
        if self.decoded_count >= self.decoded.len() && !self.decoded.is_empty() {
            // All frames decoded — drop the receiver so we stop polling.
            self.decoder_rx = None;
        }
        if received_any {
            // Newly decoded frames may need to repopulate the texture
            // cache for the current playhead — nudge a redraw so the
            // central preview swaps in immediately.
            ctx.request_repaint();
        }
    }

    fn ensure_preview(&mut self, ctx: &egui::Context, idx: usize) {
        if idx >= self.frames.len() {
            return;
        }
        if self.preview_cache.get(idx).is_some() {
            return;
        }
        // Prefer the pre-decoded buffer.
        if let Some(Some(ci)) = self.decoded.get(idx) {
            let tex = ctx.load_texture(
                format!("gif-frame-{idx}"),
                (**ci).clone(),
                egui::TextureOptions::LINEAR,
            );
            self.preview_cache.insert(idx, tex);
            return;
        }
        // Fallback: synchronous decode for the very first paint while the
        // background decoder hasn't reached this index yet (typically
        // only the first frame).
        let path = self.frames[idx].path.clone();
        match image::open(&path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let ci = egui::ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    rgba.as_raw(),
                );
                let tex = ctx.load_texture(
                    format!("gif-frame-{idx}"),
                    ci.clone(),
                    egui::TextureOptions::LINEAR,
                );
                if let Some(slot) = self.decoded.get_mut(idx) {
                    if slot.is_none() {
                        *slot = Some(Arc::new(ci));
                        self.decoded_count += 1;
                    }
                }
                self.preview_cache.insert(idx, tex);
            }
            Err(e) => warn!("gif editor: load {} failed: {e}", path.display()),
        }
    }

    fn step_playback(&mut self, ctx: &egui::Context) {
        if !self.playing || self.active_count() == 0 {
            return;
        }
        // Pace playback off the inspector's FPS slider, not the recorded
        // delays. Two reasons: (1) the export uses 1000/fps for every
        // frame, so the preview now matches the artefact byte-for-byte;
        // (2) recorded delays have OS-jitter baked in and replaying them
        // verbatim looks twitchy compared to a steady cadence.
        let frame_dt = std::time::Duration::from_millis(
            (1000 / self.fps.max(1)).max(10) as u64,
        );
        let now = std::time::Instant::now();
        let elapsed = now.saturating_duration_since(self.last_advance);

        if elapsed >= frame_dt {
            // Advance as many whole frame intervals as fit in `elapsed`.
            // Carry over the sub-frame remainder by *not* setting
            // last_advance to `now` — instead nudge it forward by the
            // exact time we consumed. This keeps long-term drift to zero.
            let dt_ms = frame_dt.as_millis().max(1) as u64;
            let elapsed_ms = elapsed.as_millis() as u64;
            let advance_count = (elapsed_ms / dt_ms).min(self.frames.len() as u64) as u32;
            for _ in 0..advance_count {
                self.advance_one();
            }
            self.last_advance += frame_dt * advance_count;
        }

        // Schedule the next paint precisely at the upcoming frame
        // boundary so egui's redraw cadence matches our playback rate.
        let next = self.last_advance + frame_dt;
        let until_next = next
            .saturating_duration_since(std::time::Instant::now())
            .max(std::time::Duration::from_millis(1));
        ctx.request_repaint_after(until_next);
    }

    fn advance_one(&mut self) {
        let len = self.frames.len();
        if len == 0 {
            return;
        }
        let mut next = self.current;
        for _ in 0..len {
            next = (next + 1) % len;
            if !self.frames[next].deleted {
                break;
            }
        }
        self.current = next;
    }

    fn trim_to_selection(&mut self) {
        let (lo, hi) = match (self.trim_in, self.trim_out) {
            (Some(a), Some(b)) if a <= b => (a, b),
            (Some(a), Some(b)) => (b, a),
            _ => {
                self.status = "Set both an in and an out marker first.".into();
                return;
            }
        };
        let mut removed = 0;
        for (i, f) in self.frames.iter_mut().enumerate() {
            if i < lo || i > hi {
                if !f.deleted {
                    removed += 1;
                }
                f.deleted = true;
            }
        }
        self.status = format!("Trimmed {removed} frame(s) outside [{lo}..={hi}].");
    }

    fn start_export(&mut self) {
        if matches!(self.export, ExportStatus::Encoding { .. }) {
            return;
        }
        // Translate FPS slider back into per-frame delay so the exported
        // GIF plays at the user's chosen rate regardless of the actual
        // capture cadence.
        let fps = self.fps.clamp(5, 60);
        let delay_ms = (1000 / fps).max(10);
        let active: Vec<crate::export::gif::FrameInput> = self
            .frames
            .iter()
            .filter(|f| !f.deleted)
            .map(|f| crate::export::gif::FrameInput {
                png_path: f.path.clone(),
                delay_ms,
            })
            .collect();
        if active.is_empty() {
            self.status = "Nothing to export — all frames are deleted.".into();
            return;
        }
        let out_path = self.paths.default_gif_filename();
        let loop_count = self.loop_count;
        let copy_clipboard = self.copy_to_clipboard_on_export;
        let spool_dir = self.sidecar.spool_dir.clone();
        let sidecar_path = self.sidecar_path.clone();
        let (tx, rx) = mpsc::channel::<ExportProgress>();

        self.export = ExportStatus::Encoding { done: 0, total: active.len() };
        self.export_rx = Some(rx);
        self.status.clear();

        std::thread::Builder::new()
            .name("grabit-gif-encode".into())
            .spawn(move || {
                let total = active.len();
                let tx_progress = tx.clone();
                let progress = move |done: usize, total: usize| {
                    let _ = tx_progress.send(ExportProgress::Tick { done, total });
                };
                match crate::export::gif::encode_to_gif(&active, loop_count, &out_path, progress) {
                    Ok(()) => {
                        if copy_clipboard {
                            // GIFs on the Windows clipboard are an oddity —
                            // most apps only accept CF_DIB / PNG. Skip
                            // clipboard copy for the GIF artefact itself
                            // and drop a note instead.
                            info!("gif: copy_to_clipboard set, but GIFs aren't a standard clipboard format; skipping");
                        }
                        // Best-effort cleanup: drop the spool PNGs and the
                        // sidecar so the next recording starts clean.
                        if let Err(e) = std::fs::remove_dir_all(&spool_dir) {
                            warn!("gif: cleanup spool {}: {e}", spool_dir.display());
                        }
                        if let Err(e) = std::fs::remove_file(&sidecar_path) {
                            // Sidecar lives inside the spool, so this is
                            // usually a no-op already.
                            log::debug!(
                                "gif: cleanup sidecar {}: {e}",
                                sidecar_path.display()
                            );
                        }
                        let _ = tx.send(ExportProgress::Done(out_path));
                        let _ = total; // suppress unused warning if we drop the assertion
                    }
                    Err(e) => {
                        let _ = tx.send(ExportProgress::Failed(format!("{e:#}")));
                    }
                }
            })
            .expect("spawn grabit-gif-encode");
    }

    fn drain_export_progress(&mut self) {
        let Some(rx) = self.export_rx.as_ref() else { return };
        loop {
            match rx.try_recv() {
                Ok(ExportProgress::Tick { done, total }) => {
                    self.export = ExportStatus::Encoding { done, total };
                }
                Ok(ExportProgress::Done(p)) => {
                    self.export = ExportStatus::Done(p);
                    self.export_rx = None;
                    break;
                }
                Ok(ExportProgress::Failed(e)) => {
                    self.export = ExportStatus::Failed(e);
                    self.export_rx = None;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.export_rx = None;
                    break;
                }
            }
        }
    }
}

impl eframe::App for GifEditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_decoder(ctx);
        self.drain_export_progress();
        self.step_playback(ctx);

        // Top bar.
        egui::TopBottomPanel::top("gif-editor-top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = match self.sidecar_path.file_name() {
                    Some(n) => n.to_string_lossy().to_string(),
                    None => "(no name)".to_string(),
                };
                ui.label(egui::RichText::new(label).strong());
                ui.separator();
                ui.label(format!(
                    "{} frames \u{2022} {} active \u{2022} {:.1}s",
                    self.frames.len(),
                    self.active_count(),
                    self.total_duration_ms() as f32 / 1000.0,
                ));
                if self.decoder_rx.is_some() {
                    ui.label(
                        egui::RichText::new(format!(
                            "buffering {}/{}",
                            self.decoded_count,
                            self.frames.len()
                        ))
                        .small()
                        .color(egui::Color32::from_rgb(180, 180, 180)),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let exporting = matches!(self.export, ExportStatus::Encoding { .. });
                    if ui
                        .add_enabled(!exporting, egui::Button::new("Export GIF"))
                        .clicked()
                    {
                        self.start_export();
                    }
                    ui.checkbox(&mut self.copy_to_clipboard_on_export, "Copy on export");
                });
            });
            ui.add_space(4.0);
        });

        // Right inspector.
        egui::SidePanel::right("gif-editor-inspector")
            .resizable(true)
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.heading("Inspector");
                ui.add_space(6.0);
                ui.label("Frames per second");
                ui.add(
                    egui::DragValue::new(&mut self.fps)
                        .range(5..=60)
                        .suffix(" fps"),
                );
                ui.add_space(6.0);
                ui.label("Loop count (0 = infinite)");
                let mut loop_val = self.loop_count as i32;
                ui.add(egui::DragValue::new(&mut loop_val).range(0..=10_000));
                self.loop_count = loop_val.clamp(0, u16::MAX as i32) as u16;
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                ui.label("Trim markers");
                ui.horizontal(|ui| {
                    if ui.button("Set IN").clicked() {
                        self.trim_in = Some(self.current);
                    }
                    if ui.button("Set OUT").clicked() {
                        self.trim_out = Some(self.current);
                    }
                    if ui.button("Clear").clicked() {
                        self.trim_in = None;
                        self.trim_out = None;
                    }
                });
                ui.label(format!(
                    "in: {}   out: {}",
                    self.trim_in.map(|i| i.to_string()).unwrap_or_else(|| "—".into()),
                    self.trim_out.map(|i| i.to_string()).unwrap_or_else(|| "—".into()),
                ));
                if ui.button("Trim to selection").clicked() {
                    self.trim_to_selection();
                }
                ui.add_space(8.0);
                if !self.status.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.status)
                            .small()
                            .color(egui::Color32::from_rgb(180, 180, 180)),
                    );
                }
            });

        // Bottom timeline.
        egui::TopBottomPanel::bottom("gif-editor-timeline")
            .resizable(false)
            .min_height(96.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let play_label = if self.playing { "Pause" } else { "Play" };
                    if ui.button(play_label).clicked() {
                        self.playing = !self.playing;
                        self.last_advance = std::time::Instant::now();
                    }
                    if ui.button("|<").clicked() {
                        self.current = 0;
                    }
                    if ui.button(">|").clicked() {
                        self.current = self.frames.len().saturating_sub(1);
                    }
                    ui.label(format!(
                        "Frame {}/{}",
                        if self.frames.is_empty() { 0 } else { self.current + 1 },
                        self.frames.len()
                    ));
                });
                egui::ScrollArea::horizontal()
                    // false on x = take full width; true on y = shrink to
                    // the thumbnail row's natural height. Without `true`
                    // here the scrollarea fills available vertical space
                    // and pushes the central preview to nothing.
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // Snapshot the indices we want to draw before
                            // we start mutating self via `ensure_preview`.
                            let count = self.frames.len();
                            for i in 0..count {
                                self.ensure_preview(ctx, i);
                                let frame_deleted = self.frames[i].deleted;
                                let is_current = i == self.current;
                                let is_in = self.trim_in == Some(i);
                                let is_out = self.trim_out == Some(i);
                                let tex = self.preview_cache.get(i).cloned();
                                let resp = ui
                                    .vertical(|ui| {
                                        if let Some(t) = tex {
                                            let img = egui::Image::new(&t).fit_to_exact_size(
                                                egui::vec2(64.0, 48.0),
                                            );
                                            let r = ui.add(img);
                                            // Highlight the strip with
                                            // colored borders to convey state.
                                            let rect = r.rect;
                                            let painter = ui.painter();
                                            if is_current {
                                                painter.rect_stroke(
                                                    rect.expand(1.5),
                                                    0.0,
                                                    egui::Stroke::new(
                                                        2.0,
                                                        egui::Color32::from_rgb(0, 180, 255),
                                                    ),
                                                );
                                            }
                                            if is_in || is_out {
                                                painter.rect_stroke(
                                                    rect.expand(2.5),
                                                    0.0,
                                                    egui::Stroke::new(
                                                        2.0,
                                                        egui::Color32::from_rgb(120, 200, 80),
                                                    ),
                                                );
                                            }
                                            if frame_deleted {
                                                painter.line_segment(
                                                    [rect.left_top(), rect.right_bottom()],
                                                    egui::Stroke::new(
                                                        2.0,
                                                        egui::Color32::from_rgb(220, 80, 80),
                                                    ),
                                                );
                                            }
                                            r
                                        } else {
                                            ui.add_sized(
                                                [64.0, 48.0],
                                                egui::Label::new("…"),
                                            )
                                        }
                                    })
                                    .inner;
                                if resp.clicked() {
                                    self.current = i;
                                }
                                if resp.secondary_clicked() {
                                    if let Some(f) = self.frames.get_mut(i) {
                                        f.deleted = !f.deleted;
                                    }
                                }
                                resp.context_menu(|ui| {
                                    if ui.button("Toggle delete").clicked() {
                                        if let Some(f) = self.frames.get_mut(i) {
                                            f.deleted = !f.deleted;
                                        }
                                        ui.close_menu();
                                    }
                                    if ui.button("Set IN here").clicked() {
                                        self.trim_in = Some(i);
                                        ui.close_menu();
                                    }
                                    if ui.button("Set OUT here").clicked() {
                                        self.trim_out = Some(i);
                                        ui.close_menu();
                                    }
                                });
                            }
                        });
                    });
                ui.add_space(4.0);
            });

        // Center preview.
        //
        // Use `paint_at` rather than `Image::fit_to_exact_size` inside
        // `centered_and_justified`: the latter feeds the image's allocated
        // size back into the panel's layout, and on a viewport that's
        // mid-resize (or while playback is forcing repaints) the avail
        // size shrinks by a couple of pixels per frame, which compounds
        // visibly into a "preview keeps shrinking" loop. Allocating the
        // full panel rect once and painting the image into a centered
        // sub-rect avoids any layout feedback.
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ensure_preview(ctx, self.current);
            let panel_rect = ui.available_rect_before_wrap();
            let (rect, _) = ui.allocate_exact_size(panel_rect.size(), egui::Sense::hover());
            if let Some(tex) = self.preview_cache.get(self.current) {
                let img_size = tex.size_vec2();
                let scale = (rect.width() / img_size.x.max(1.0))
                    .min(rect.height() / img_size.y.max(1.0))
                    .min(1.0);
                let target = img_size * scale;
                let centered = egui::Rect::from_center_size(rect.center(), target);
                egui::Image::new(tex).paint_at(ui, centered);
            } else {
                let msg = if self.frames.is_empty() {
                    "No frames recorded."
                } else {
                    "Loading preview\u{2026}"
                };
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    msg,
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.style().visuals.weak_text_color(),
                );
            }
        });

        // Export modal.
        match &self.export {
            ExportStatus::Encoding { done, total } => {
                let prog = if *total == 0 {
                    0.0
                } else {
                    *done as f32 / *total as f32
                };
                egui::Window::new("Encoding GIF\u{2026}")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.add(egui::ProgressBar::new(prog).animate(true));
                        ui.label(format!("Frame {done} / {total}"));
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(60));
            }
            ExportStatus::Done(p) => {
                let path_str = p.display().to_string();
                egui::Window::new("Export complete")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("Saved to:");
                        ui.monospace(&path_str);
                        ui.horizontal(|ui| {
                            if ui.button("Show in Explorer").clicked() {
                                show_in_explorer(p);
                            }
                            if ui.button("Close").clicked() {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        });
                    });
            }
            ExportStatus::Failed(msg) => {
                let msg = msg.clone();
                egui::Window::new("Export failed")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.colored_label(egui::Color32::from_rgb(220, 80, 80), &msg);
                        if ui.button("Dismiss").clicked() {
                            self.export = ExportStatus::Idle;
                        }
                    });
            }
            ExportStatus::Idle => {}
        }
    }
}

/// Spawn the background PNG decoder. Streams `(idx, ColorImage)` results
/// over an mpsc channel as fast as `image::open` will produce them. The
/// `cancel` flag lets `GifEditorApp::drop` short-circuit a bulk decode if
/// the user closes the editor mid-load.
fn spawn_decoder(
    paths: Vec<PathBuf>,
    cancel: Arc<AtomicBool>,
) -> mpsc::Receiver<(usize, Arc<egui::ColorImage>)> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("grabit-gif-decoder".into())
        .spawn(move || {
            for (i, path) in paths.into_iter().enumerate() {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                match image::open(&path) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = rgba.dimensions();
                        let ci = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            rgba.as_raw(),
                        );
                        if tx.send((i, Arc::new(ci))).is_err() {
                            // Receiver dropped — editor closed.
                            return;
                        }
                    }
                    Err(e) => warn!("gif decoder: read {}: {e}", path.display()),
                }
            }
        })
        .expect("spawn grabit-gif-decoder");
    rx
}

fn show_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        use windows::core::{HSTRING, PCWSTR};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let parent = path.parent().unwrap_or(path);
        let op = HSTRING::from("open");
        let file = HSTRING::from(parent.to_string_lossy().to_string());
        unsafe {
            ShellExecuteW(
                HWND::default(),
                &op,
                &file,
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}
