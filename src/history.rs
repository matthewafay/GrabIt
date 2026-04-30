//! Mini capture history viewer.
//!
//! Tray → "History…" spawns `grabit.exe --history`, which scans the
//! configured `output_dir` for the most recent PNGs and GIFs and shows
//! them in a thumbnail grid. Each entry has two actions:
//!
//! - **Copy**: PNGs go on the clipboard as `CF_DIB` (paste-as-image
//!   anywhere); GIFs go as `CF_HDROP` (a file drop, so chat clients
//!   paste them as the actual animated file).
//! - **Copy path**: drops the absolute path on the clipboard as
//!   `CF_UNICODETEXT`.
//!
//! No persistent history file is maintained — we just walk the output
//! directory on each open. Files the user deletes from disk drop out of
//! the list naturally.

use crate::app::paths::AppPaths;
use crate::settings::Settings;
use anyhow::Result;
use eframe::egui;
use log::{info, warn};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// Maximum number of entries shown in the history grid. Loading
/// thumbnails is lazy, but capping the entry count keeps the directory
/// scan + initial sort cheap on output folders that have accumulated
/// thousands of captures.
const MAX_ENTRIES: usize = 60;

/// Subprocess entry. Mirrors `editor::run_blocking` and
/// `editor::gif_app::run_blocking`.
pub fn run_blocking(paths: AppPaths, _settings: Settings) -> Result<()> {
    let viewport = {
        let mut vb = egui::ViewportBuilder::default()
            .with_title("GrabIt — History")
            .with_inner_size([720.0, 560.0])
            .with_min_inner_size([520.0, 360.0]);
        if let Some(icon) = crate::editor::load_app_icon_data() {
            vb = vb.with_icon(Arc::new(icon));
        }
        vb
    };

    let options = eframe::NativeOptions {
        viewport,
        // We're a fresh `--history` subprocess so we own the main
        // thread; `with_any_thread(true)` mirrors the gif/editor
        // subprocesses for consistency.
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
        "GrabIt — History",
        options,
        Box::new(move |cc| {
            crate::editor::install_jetbrains_mono(&cc.egui_ctx);
            Ok(Box::new(HistoryApp::new(paths)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Png,
    Gif,
}

#[derive(Debug, Clone)]
struct Entry {
    path: PathBuf,
    kind: Kind,
    /// Cached file size for the label. None until first stat.
    size_bytes: u64,
    modified: Option<SystemTime>,
    /// Set once a `Copy` / `Copy path` button has been clicked, so the
    /// row can flash a brief confirmation label.
    flash: Option<(String, std::time::Instant)>,
}

struct HistoryApp {
    paths: AppPaths,
    entries: Vec<Entry>,
    /// Lazily-populated thumbnail textures, keyed by entry index.
    thumbs: std::collections::HashMap<usize, egui::TextureHandle>,
    status: String,
}

impl HistoryApp {
    fn new(paths: AppPaths) -> Self {
        let entries = scan(&paths.output_dir);
        let status = if entries.is_empty() {
            "No captures yet — your saved screenshots will appear here.".into()
        } else {
            String::new()
        };
        Self {
            paths,
            entries,
            thumbs: std::collections::HashMap::new(),
            status,
        }
    }

    fn refresh(&mut self) {
        self.entries = scan(&self.paths.output_dir);
        self.thumbs.clear();
        self.status = if self.entries.is_empty() {
            "No captures yet.".into()
        } else {
            format!("Found {} item(s).", self.entries.len())
        };
    }

    fn ensure_thumb(&mut self, ctx: &egui::Context, idx: usize) {
        if self.thumbs.contains_key(&idx) {
            return;
        }
        let Some(entry) = self.entries.get(idx) else {
            return;
        };
        // For both PNG and GIF, `image::open` gives us the first frame.
        // That's what we want — a thumbnail is just a still preview.
        match image::open(&entry.path) {
            Ok(img) => {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let tex = ctx.load_texture(
                    format!("history-thumb-{idx}"),
                    egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        rgba.as_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                );
                self.thumbs.insert(idx, tex);
            }
            Err(e) => warn!("history: load {} failed: {e}", entry.path.display()),
        }
    }
}

impl eframe::App for HistoryApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Top bar.
        egui::TopBottomPanel::top("history-top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("Capture history");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        self.refresh();
                    }
                    if ui.button("Open folder").clicked() {
                        show_in_explorer(&self.paths.output_dir);
                    }
                });
            });
            ui.label(
                egui::RichText::new(self.paths.output_dir.display().to_string())
                    .small()
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(4.0);
        });

        // Status bar.
        egui::TopBottomPanel::bottom("history-status").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(if self.status.is_empty() {
                    format!("{} item(s)", self.entries.len())
                } else {
                    self.status.clone()
                })
                .small()
                .color(egui::Color32::GRAY),
            );
            ui.add_space(2.0);
        });

        // Grid of cards.
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.entries.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new(&self.status)
                            .color(egui::Color32::GRAY),
                    );
                });
                return;
            }

            const CARD_W: f32 = 220.0;
            const CARD_H: f32 = 200.0;
            const THUMB_W: f32 = 200.0;
            const THUMB_H: f32 = 112.0; // 16:9-ish

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    let avail_w = ui.available_width();
                    let cols = ((avail_w / CARD_W).floor() as usize).max(1);
                    let total = self.entries.len();
                    let mut i = 0;
                    while i < total {
                        ui.horizontal(|ui| {
                            for _ in 0..cols {
                                if i >= total {
                                    break;
                                }
                                self.ensure_thumb(ctx, i);
                                self.draw_card(ui, i, CARD_W, CARD_H, THUMB_W, THUMB_H);
                                i += 1;
                            }
                        });
                        ui.add_space(6.0);
                    }
                });
        });

        // Repaint while any flash badge is still on screen so it can
        // expire on its own clock.
        if self.entries.iter().any(|e| e.flash.is_some()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
}

impl HistoryApp {
    fn draw_card(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        card_w: f32,
        card_h: f32,
        thumb_w: f32,
        thumb_h: f32,
    ) {
        let frame = egui::Frame::group(ui.style())
            .inner_margin(egui::Margin::same(6.0))
            .rounding(egui::Rounding::same(6.0));
        frame.show(ui, |ui| {
            ui.set_width(card_w - 12.0);
            ui.set_height(card_h - 12.0);

            // Thumbnail area — fixed-size box centered horizontally, with
            // the underlying image drawn via paint_at into the centered
            // sub-rect. Same trick as the GIF preview; avoids any image
            // size feeding back into ui layout.
            let (thumb_rect, _) = ui.allocate_exact_size(
                egui::vec2(thumb_w, thumb_h),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(
                thumb_rect,
                egui::Rounding::same(4.0),
                egui::Color32::from_gray(28),
            );
            if let Some(tex) = self.thumbs.get(&idx) {
                let img_size = tex.size_vec2();
                let scale = (thumb_rect.width() / img_size.x.max(1.0))
                    .min(thumb_rect.height() / img_size.y.max(1.0))
                    .min(1.0);
                let target = img_size * scale;
                let centered =
                    egui::Rect::from_center_size(thumb_rect.center(), target);
                egui::Image::new(tex).paint_at(ui, centered);
            } else {
                ui.painter().text(
                    thumb_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "loading…",
                    egui::TextStyle::Body.resolve(ui.style()),
                    egui::Color32::from_gray(140),
                );
            }

            // Type badge (PNG / GIF) painted over the top-left.
            let (badge_text, badge_color) = match self.entries[idx].kind {
                Kind::Png => ("PNG", egui::Color32::from_rgb(0, 120, 200)),
                Kind::Gif => ("GIF", egui::Color32::from_rgb(180, 100, 0)),
            };
            let badge_rect = egui::Rect::from_min_size(
                thumb_rect.min + egui::vec2(4.0, 4.0),
                egui::vec2(36.0, 16.0),
            );
            ui.painter().rect_filled(
                badge_rect,
                egui::Rounding::same(3.0),
                badge_color.linear_multiply(0.85),
            );
            ui.painter().text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                badge_text,
                egui::TextStyle::Small.resolve(ui.style()),
                egui::Color32::WHITE,
            );

            // Filename (single line, truncated).
            let fname = self.entries[idx]
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "(unnamed)".into());
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(fname).strong())
                    .truncate(),
            );

            // Size + relative-mtime label.
            let size_label = human_size(self.entries[idx].size_bytes);
            let rel = self.entries[idx]
                .modified
                .map(|t| relative_time(t))
                .unwrap_or_else(|| String::new());
            ui.label(
                egui::RichText::new(format!("{size_label}  •  {rel}"))
                    .small()
                    .color(egui::Color32::GRAY),
            );

            ui.add_space(2.0);

            // Action row.
            ui.horizontal(|ui| {
                let path = self.entries[idx].path.clone();
                if ui.button("Copy").clicked() {
                    self.do_copy_image(idx, &path);
                }
                if ui.button("Copy path").clicked() {
                    self.do_copy_path(idx, &path);
                }
            });

            // Flash (brief "Copied!" badge after a click).
            if let Some((msg, when)) = &self.entries[idx].flash {
                let elapsed = when.elapsed();
                if elapsed < std::time::Duration::from_millis(1600) {
                    ui.label(
                        egui::RichText::new(msg)
                            .small()
                            .color(egui::Color32::from_rgb(120, 200, 120)),
                    );
                } else {
                    self.entries[idx].flash = None;
                }
            }
        });
    }

    fn do_copy_image(&mut self, idx: usize, path: &std::path::Path) {
        match copy_image(path) {
            Ok(()) => {
                let kind = self.entries[idx].kind;
                let msg = match kind {
                    Kind::Png => "Copied image to clipboard",
                    Kind::Gif => "Copied GIF (file drop) to clipboard",
                };
                self.entries[idx].flash =
                    Some((msg.into(), std::time::Instant::now()));
                info!("history: copy image {} ({:?})", path.display(), kind);
            }
            Err(e) => {
                self.entries[idx].flash =
                    Some((format!("Copy failed: {e}"), std::time::Instant::now()));
                warn!("history: copy image {}: {e}", path.display());
            }
        }
    }

    fn do_copy_path(&mut self, idx: usize, path: &std::path::Path) {
        let abs = std::fs::canonicalize(path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        // Strip the kernel's `\\?\` UNC prefix so the pasted path looks
        // like a normal Windows path. Same sanitization as the HDROP
        // helper does internally.
        let clean = abs.strip_prefix(r"\\?\").unwrap_or(&abs).to_string();
        match crate::export::copy_text_to_clipboard(&clean) {
            Ok(()) => {
                self.entries[idx].flash = Some((
                    "Copied path to clipboard".into(),
                    std::time::Instant::now(),
                ));
                info!("history: copy path {}", clean);
            }
            Err(e) => {
                self.entries[idx].flash =
                    Some((format!("Copy failed: {e}"), std::time::Instant::now()));
                warn!("history: copy path {}: {e}", clean);
            }
        }
    }
}

#[cfg(windows)]
fn copy_image(path: &std::path::Path) -> Result<()> {
    crate::export::copy_file_to_clipboard(path)
}

#[cfg(not(windows))]
fn copy_image(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// Walk `dir` for `*.png` / `*.gif`, sorted by modification time
/// (newest first), capped at `MAX_ENTRIES`. Errors are logged and
/// produce an empty list — the UI handles the empty case gracefully.
fn scan(dir: &std::path::Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            warn!("history: read_dir {}: {e}", dir.display());
            return out;
        }
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let kind = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
        {
            Some(ref s) if s == "png" => Kind::Png,
            Some(ref s) if s == "gif" => Kind::Gif,
            _ => continue,
        };
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(Entry {
            path,
            kind,
            size_bytes: meta.len(),
            modified: meta.modified().ok(),
            flash: None,
        });
    }
    // Newest first.
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out.truncate(MAX_ENTRIES);
    out
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn relative_time(t: SystemTime) -> String {
    match SystemTime::now().duration_since(t) {
        Ok(d) => {
            let secs = d.as_secs();
            if secs < 60 {
                "just now".to_string()
            } else if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86_400)
            }
        }
        // If `modified` is in the future (clock skew), don't error —
        // just fall back to a neutral label.
        Err(_) => "recently".to_string(),
    }
}

#[cfg(windows)]
fn show_in_explorer(path: &std::path::Path) {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let op = HSTRING::from("open");
    let file = HSTRING::from(path.to_string_lossy().to_string());
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
fn show_in_explorer(_path: &std::path::Path) {}
