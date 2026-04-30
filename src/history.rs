//! Capture-history viewer rendered with Dioxus desktop.
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
//! the list naturally. The whole window is one process with the rest of
//! grabit, so clipboard helpers in `crate::export` are called directly
//! from event handlers — no IPC.

use crate::app::paths::AppPaths;
use crate::settings::Settings;
use anyhow::Result;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use dioxus::desktop::{tao::window::WindowBuilder, Config, LogicalSize};
use dioxus::prelude::*;
use log::{info, warn};
use rayon::prelude::*;
use std::path::PathBuf;
use std::time::SystemTime;

/// Cap on how many history items the window loads. Each thumbnail is
/// downscaled + base64-encoded into the DOM, so the cost grows linearly.
/// 60 keeps the initial paint well under a second on a fast machine.
const MAX_ENTRIES: usize = 60;

/// Thumbnail target size in physical pixels. Aspect ratio is preserved
/// by `image::imageops::thumbnail`, so this is just a bounding box.
const THUMB_W: u32 = 320;
const THUMB_H: u32 = 180;

/// CSS embedded at compile time — see `history.css` for the actual
/// styling. Inlined into the document via a `<style>` tag.
const STYLES: &str = include_str!("history.css");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Png,
    Gif,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Png => "PNG",
            Kind::Gif => "GIF",
        }
    }
    fn class(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Gif => "gif",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    path: PathBuf,
    kind: Kind,
    size_bytes: u64,
    modified: Option<SystemTime>,
    /// Pre-built `data:image/png;base64,…` URI ready to drop into
    /// `<img src=…>`. Built once during the directory scan to avoid
    /// re-encoding on every repaint.
    thumb_data_uri: String,
}

/// Held in Dioxus's context registry so the root component can pull
/// the initial scan + the output_dir without prop-drilling.
#[derive(Clone)]
struct InitialState {
    entries: Vec<Entry>,
    output_dir: PathBuf,
}

/// Subprocess entry. Mirrors `history::run_blocking` in shape so the
/// two are interchangeable from `main.rs`'s perspective.
pub fn run_blocking(paths: AppPaths, _settings: Settings) -> Result<()> {
    info!(
        "history: scanning {} (cap {} entries)",
        paths.output_dir.display(),
        MAX_ENTRIES
    );
    let entries = scan_and_load(&paths.output_dir);
    info!("history: loaded {} entries", entries.len());

    let initial = InitialState {
        entries,
        output_dir: paths.output_dir.clone(),
    };

    let cfg = Config::new().with_window(
        WindowBuilder::new()
            .with_title("GrabIt — History")
            .with_inner_size(LogicalSize::new(900.0, 660.0))
            .with_min_inner_size(LogicalSize::new(620.0, 420.0)),
    );

    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(initial)
        .launch(history_app);

    Ok(())
}

#[component]
fn history_app() -> Element {
    let initial = use_context::<InitialState>();
    let mut entries = use_signal(|| initial.entries.clone());
    let output_dir = initial.output_dir.clone();
    let output_dir_label = output_dir.display().to_string();

    let dir_for_open = output_dir.clone();
    let dir_for_refresh = output_dir.clone();

    rsx! {
        style { "{STYLES}" }
        div { class: "app",
            header { class: "toolbar",
                div { class: "title",
                    h1 { "Capture history" }
                    p { class: "path", "{output_dir_label}" }
                }
                div { class: "actions",
                    button {
                        class: "ghost",
                        onclick: move |_| open_in_explorer(&dir_for_open),
                        "Open folder"
                    }
                    button {
                        class: "ghost",
                        onclick: move |_| {
                            entries.set(scan_and_load(&dir_for_refresh));
                        },
                        "Refresh"
                    }
                }
            }

            main { class: "main",
                if entries.read().is_empty() {
                    div { class: "empty",
                        p { "No captures yet — your saved screenshots will appear here." }
                    }
                } else {
                    div { class: "grid",
                        for entry in entries.read().iter().cloned() {
                            EntryCard {
                                key: "{entry.path.display()}",
                                entry: entry.clone(),
                            }
                        }
                    }
                }
            }

            footer { class: "footer",
                span { "{entries.read().len()} item(s)" }
            }
        }
    }
}

#[component]
fn EntryCard(entry: Entry) -> Element {
    // Per-card flash: shows briefly under the action row after a click.
    // Carries an `Instant` so the message expires on its own clock; we
    // schedule a delayed re-render via `spawn` to clear it.
    let mut flash = use_signal(|| Option::<String>::None);

    // Each closure captures its own clone of the path so the
    // borrow-checker is happy with two onclicks each consuming a
    // separate move. PathBuf is cheap to clone.
    let path_for_image = entry.path.clone();
    let path_for_text = entry.path.clone();
    let kind = entry.kind;

    let do_copy_image = move |_| {
        let path = path_for_image.clone();
        let result = crate::export::copy_file_to_clipboard(&path);
        let msg = match result {
            Ok(()) => match kind {
                Kind::Png => "Copied image to clipboard".to_string(),
                Kind::Gif => "Copied GIF (file drop) to clipboard".to_string(),
            },
            Err(e) => {
                warn!("history: copy image {}: {e}", path.display());
                format!("Copy failed: {e}")
            }
        };
        flash.set(Some(msg));
        // Auto-clear after ~1.6s. `spawn` schedules a Dioxus task on
        // the runtime; setting the signal triggers a re-render that
        // hides the flash.
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
            flash.set(None);
        });
    };

    let do_copy_path = move |_| {
        let path = path_for_text.clone();
        let abs = std::fs::canonicalize(&path)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        let clean = abs.strip_prefix(r"\\?\").unwrap_or(&abs).to_string();
        let msg = match crate::export::copy_text_to_clipboard(&clean) {
            Ok(()) => "Copied path to clipboard".to_string(),
            Err(e) => {
                warn!("history: copy path {}: {e}", clean);
                format!("Copy failed: {e}")
            }
        };
        flash.set(Some(msg));
        spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
            flash.set(None);
        });
    };

    let badge_class = format!("badge {}", entry.kind.class());
    let filename = entry
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unnamed)".into());
    let sub = format!(
        "{} • {}",
        human_size(entry.size_bytes),
        entry
            .modified
            .map(relative_time)
            .unwrap_or_else(|| "—".into())
    );

    rsx! {
        div { class: "card",
            div { class: "thumb",
                img { src: "{entry.thumb_data_uri}", alt: "thumbnail" }
                span { class: "{badge_class}", "{entry.kind.label()}" }
            }
            div { class: "meta",
                div { class: "name", "{filename}" }
                div { class: "sub", "{sub}" }
            }
            div { class: "row",
                button { class: "primary", onclick: do_copy_image, "Copy" }
                button { class: "secondary", onclick: do_copy_path, "Copy path" }
            }
            if let Some(msg) = flash.read().clone() {
                div { class: "flash", "{msg}" }
            }
        }
    }
}

/// Walk `dir` for *.png/*.gif, sort newest first, cap at MAX_ENTRIES,
/// then build base64 thumbnails for each in parallel. The parallel
/// thumbnail step is rayon-driven so a large output folder still
/// populates quickly on a multi-core box.
fn scan_and_load(dir: &std::path::Path) -> Vec<Entry> {
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            warn!("history: read_dir {}: {e}", dir.display());
            return Vec::new();
        }
    };
    let mut metas: Vec<(PathBuf, Kind, u64, Option<SystemTime>)> = Vec::new();
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
        metas.push((path, kind, meta.len(), meta.modified().ok()));
    }
    metas.sort_by(|a, b| b.3.cmp(&a.3));
    metas.truncate(MAX_ENTRIES);

    metas
        .into_par_iter()
        .filter_map(|(path, kind, size, modified)| {
            let thumb = build_thumb(&path)?;
            Some(Entry {
                path,
                kind,
                size_bytes: size,
                modified,
                thumb_data_uri: thumb,
            })
        })
        .collect()
}

/// Decode `path`, downscale to a THUMB_W × THUMB_H bounding box (aspect
/// preserved), re-encode as PNG, then base64-encode into a data URI
/// suitable for `<img src=…>`. Returns `None` on any decode failure;
/// the caller drops the entry rather than rendering a broken card.
fn build_thumb(path: &std::path::Path) -> Option<String> {
    let img = image::open(path).ok()?;
    let resized = img.thumbnail(THUMB_W, THUMB_H);
    let mut bytes: Vec<u8> = Vec::new();
    resized
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    let b64 = STANDARD.encode(&bytes);
    Some(format!("data:image/png;base64,{}", b64))
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
        Err(_) => "recently".to_string(),
    }
}

#[cfg(windows)]
fn open_in_explorer(path: &std::path::Path) {
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
fn open_in_explorer(_path: &std::path::Path) {}
