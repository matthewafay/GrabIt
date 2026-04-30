//! Frame editor for a recorded GIF sidecar — Dioxus port.
//!
//! Loads `recording.json` plus its spool directory and presents:
//!
//! - **Top bar**: filename, frame counts + duration, copy-on-export
//!   toggle, Export button.
//! - **Center preview**: current frame as an `<img>`. FPS-paced
//!   playback advances the index from a tokio task that loops while
//!   `playing` is true.
//! - **Right inspector**: target FPS, loop count, IN/OUT trim markers,
//!   Trim-to-selection.
//! - **Bottom timeline**: horizontally-scrolling strip of thumbnails;
//!   click to scrub, right-click to toggle delete, IN/OUT highlight.
//! - **Export modal**: progress bar fed by an `mpsc` channel from a
//!   worker thread running `crate::export::gif::encode_to_gif`.
//!
//! On a successful export the spool directory + sidecar are cleaned up;
//! cancelled exports leave both in place so the user can retry.
//!
//! Frames are decoded once at startup on a tokio blocking-thread pool
//! (rayon-style parallelism via `spawn_blocking` inside a join_all).
//! Each decoded frame becomes a `data:image/png;base64,…` URI cached in
//! a per-frame slot; the timeline + preview just point `<img src>` at
//! that URI, which means playback is bottlenecked only by the
//! browser's image-swap cost (not PNG decode).

use crate::app::paths::AppPaths;
use crate::capture::gif_record::{GifSidecar, SidecarFrame};
use crate::settings::Settings;
use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use dioxus::desktop::{tao::window::WindowBuilder, Config, LogicalSize};
use dioxus::prelude::*;
use log::{info, warn};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

const STYLES: &str = include_str!("gif_app.css");

/// Bounding box for the timeline thumbnails. Aspect ratio is preserved;
/// the strip's frame divs are 78×56 so this matches roughly.
const THUMB_W: u32 = 156;
const THUMB_H: u32 = 88;

#[derive(Clone, Debug, PartialEq)]
struct EditableFrame {
    path: PathBuf,
    delay_ms: u32,
    deleted: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum ExportStatus {
    Idle,
    Encoding { done: usize, total: usize },
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone, Debug)]
enum ExportProgress {
    Tick { done: usize, total: usize },
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone)]
struct InitialState {
    sidecar: GifSidecar,
    sidecar_path: PathBuf,
    paths: AppPaths,
    settings: Settings,
}

/// Newtype around the editor's `alive` cancellation flag so it can ride
/// through Dioxus's typed context lookup.
#[derive(Clone)]
struct AliveFlag(Arc<AtomicBool>);

pub fn run_blocking(sidecar_path: PathBuf, paths: AppPaths, settings: Settings) -> Result<()> {
    let body = std::fs::read_to_string(&sidecar_path)
        .with_context(|| format!("read sidecar {}", sidecar_path.display()))?;
    let sidecar: GifSidecar = serde_json::from_str(&body)
        .with_context(|| format!("parse sidecar {}", sidecar_path.display()))?;

    let mut window = WindowBuilder::new()
        .with_title("GrabIt — GIF editor")
        .with_inner_size(LogicalSize::new(1100.0, 720.0))
        .with_min_inner_size(LogicalSize::new(820.0, 520.0));
    if let Some(icon) = crate::platform::icon::load_window_icon() {
        window = window.with_window_icon(Some(icon));
    }
    let cfg = Config::new().with_window(window);

    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(InitialState {
            sidecar,
            sidecar_path,
            paths,
            settings,
        })
        .launch(gif_editor_app);

    Ok(())
}

#[component]
fn gif_editor_app() -> Element {
    let initial = use_context::<InitialState>();

    let frames_init: Vec<EditableFrame> = initial
        .sidecar
        .frames
        .iter()
        .map(|f: &SidecarFrame| EditableFrame {
            path: initial.sidecar.spool_dir.join(&f.file),
            delay_ms: f.delay_ms,
            deleted: false,
        })
        .collect();
    let frames_count = frames_init.len();

    let frames = use_signal(|| frames_init.clone());
    let current = use_signal(|| 0usize);
    let fps = use_signal(|| initial.sidecar.fps_target.clamp(5, 60));
    let loop_count = use_signal(|| initial.sidecar.loop_count);
    let trim_in = use_signal(|| Option::<usize>::None);
    let trim_out = use_signal(|| Option::<usize>::None);
    let playing = use_signal(|| false);
    let copy_on_export = use_signal(|| initial.settings.copy_to_clipboard);
    let export_status = use_signal(|| ExportStatus::Idle);
    // Per-frame decoded data URI. `None` until the background decoder
    // catches up. Length is fixed at frames_count so we can index into
    // it from anywhere without bounds checks.
    let decoded = use_signal(|| vec![Option::<String>::None; frames_count]);

    // Cancellation flag flipped to false when the GifEditorApp is
    // dropped (i.e. the window closed). Spawned tasks check it
    // before touching any signal — without this, the decoder /
    // playback futures continue running after Dioxus has dropped
    // the signal storage and the next .with_mut / .read panics with
    // "value was dropped". Shared with `Timeline` (where the
    // playback loop lives) via context.
    let alive = use_hook(|| Arc::new(AtomicBool::new(true)));
    use_drop({
        let alive = alive.clone();
        move || alive.store(false, Ordering::SeqCst)
    });
    use_context_provider(|| AliveFlag(alive.clone()));

    // Background decoder. `use_hook` runs exactly once on mount.
    use_hook(|| {
        let mut decoded = decoded.to_owned();
        let paths_iter: Vec<(usize, PathBuf)> = frames_init
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.path.clone()))
            .collect();
        let alive = alive.clone();
        spawn(async move {
            for (i, path) in paths_iter {
                if !alive.load(Ordering::SeqCst) {
                    return;
                }
                let res = tokio::task::spawn_blocking(move || build_thumb_uri(&path)).await;
                if !alive.load(Ordering::SeqCst) {
                    return;
                }
                match res {
                    Ok(Some(uri)) => {
                        decoded.with_mut(|d| {
                            if i < d.len() {
                                d[i] = Some(uri);
                            }
                        });
                    }
                    Ok(None) => {
                        warn!("gif editor: decode frame {i} returned None");
                    }
                    Err(e) => {
                        warn!("gif editor: decode task {i} panicked: {e}");
                    }
                }
            }
        });
    });

    rsx! {
        style { "{STYLES}" }
        div { class: "app",
            TopBar {
                sidecar_filename: initial.sidecar_path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "(no name)".into()),
                frames: frames,
                fps: fps,
                copy_on_export: copy_on_export,
                export_status: export_status,
                loop_count: loop_count,
            }
            Preview {
                decoded: decoded,
                current: current,
                frames: frames,
                export_status: export_status,
            }
            Inspector {
                fps: fps,
                loop_count: loop_count,
                trim_in: trim_in,
                trim_out: trim_out,
                current: current,
                frames: frames,
            }
            Timeline {
                decoded: decoded,
                frames: frames,
                current: current,
                trim_in: trim_in,
                trim_out: trim_out,
                playing: playing,
                fps: fps,
            }
        }
    }
}

#[component]
fn TopBar(
    sidecar_filename: String,
    frames: Signal<Vec<EditableFrame>>,
    fps: Signal<u32>,
    copy_on_export: Signal<bool>,
    export_status: Signal<ExportStatus>,
    loop_count: Signal<u16>,
) -> Element {
    // Pull the InitialState here at component-scope so the click
    // handler closures don't have to call `use_context` themselves
    // (hooks must be at top-level of a component, not inside arbitrary
    // closures).
    let initial = use_context::<InitialState>();

    let total = frames.read().len();
    let active = frames.read().iter().filter(|f| !f.deleted).count();
    let total_ms: u64 = frames
        .read()
        .iter()
        .filter(|f| !f.deleted)
        .map(|f| f.delay_ms as u64)
        .sum();
    let duration_s = total_ms as f32 / 1000.0;
    let exporting = matches!(*export_status.read(), ExportStatus::Encoding { .. });
    let copy_state = *copy_on_export.read();

    let on_export_click = move |_| {
        start_export(
            frames,
            fps,
            loop_count,
            copy_on_export,
            export_status,
            initial.paths.default_gif_filename(),
            initial.sidecar.spool_dir.clone(),
            initial.sidecar_path.clone(),
        );
    };

    rsx! {
        div { class: "topbar",
            div { class: "title",
                h1 { "{sidecar_filename}" }
                p { "{total} frames • {active} active • {duration_s:.1}s" }
            }
            div { class: "actions",
                label { class: "toggle-mini",
                    input {
                        r#type: "checkbox",
                        checked: "{copy_state}",
                        onchange: move |evt| copy_on_export.set(evt.checked()),
                    }
                    "Copy on export"
                }
                button {
                    class: "primary",
                    disabled: exporting,
                    onclick: on_export_click,
                    if exporting { "Exporting…" } else { "Export GIF" }
                }
            }
        }
    }
}

#[component]
fn Preview(
    decoded: Signal<Vec<Option<String>>>,
    current: Signal<usize>,
    frames: Signal<Vec<EditableFrame>>,
    export_status: Signal<ExportStatus>,
) -> Element {
    let cur_idx = *current.read();
    let uri = decoded
        .read()
        .get(cur_idx)
        .cloned()
        .flatten();
    let total = frames.read().len();

    rsx! {
        div { class: "preview",
            if let Some(u) = uri {
                img { src: "{u}", alt: "frame preview" }
            } else if total == 0 {
                div { class: "empty", "No frames recorded." }
            } else {
                div { class: "empty", "Loading preview…" }
            }
            ExportOverlay { export_status: export_status }
        }
    }
}

#[component]
fn ExportOverlay(export_status: Signal<ExportStatus>) -> Element {
    let status = export_status.read().clone();
    rsx! {
        match status {
            ExportStatus::Idle => rsx! {},
            ExportStatus::Encoding { done, total } => {
                let pct = if total == 0 { 0 } else { (done * 100 / total).min(100) };
                rsx! {
                    div { class: "progress-overlay",
                        div { class: "card",
                            h3 { "Encoding GIF…" }
                            div { class: "bar",
                                div {
                                    class: "fill",
                                    style: "width: {pct}%;",
                                }
                            }
                            div { class: "meta", "Frame {done} / {total}" }
                        }
                    }
                }
            }
            ExportStatus::Done(path) => {
                let path_str = path.display().to_string();
                let path_for_show = path.clone();
                rsx! {
                    div { class: "progress-overlay",
                        div { class: "card",
                            h3 { "Export complete" }
                            div { class: "meta", "Saved to:" }
                            code { "{path_str}" }
                            div { class: "row-actions",
                                button {
                                    class: "ghost",
                                    onclick: move |_| show_in_explorer(&path_for_show),
                                    "Show in Explorer"
                                }
                                button {
                                    class: "primary",
                                    onclick: move |_| {
                                        dioxus::desktop::window().close();
                                    },
                                    "Close"
                                }
                            }
                        }
                    }
                }
            }
            ExportStatus::Failed(msg) => {
                rsx! {
                    div { class: "progress-overlay",
                        div { class: "card",
                            h3 { "Export failed" }
                            div { class: "meta", "{msg}" }
                            div { class: "row-actions",
                                button {
                                    class: "ghost",
                                    onclick: move |_| export_status.set(ExportStatus::Idle),
                                    "Dismiss"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Inspector(
    fps: Signal<u32>,
    loop_count: Signal<u16>,
    trim_in: Signal<Option<usize>>,
    trim_out: Signal<Option<usize>>,
    current: Signal<usize>,
    frames: Signal<Vec<EditableFrame>>,
) -> Element {
    let fps_val = *fps.read();
    let loop_val = *loop_count.read();
    // `read()` returns a Ref guard; deref to get the Option itself
    // before calling `.map`, otherwise we'd try to map over the guard.
    let in_label = (*trim_in.read())
        .map(|i| i.to_string())
        .unwrap_or_else(|| "—".into());
    let out_label = (*trim_out.read())
        .map(|i| i.to_string())
        .unwrap_or_else(|| "—".into());
    let total_frames = frames.read().len();
    let active = frames.read().iter().filter(|f| !f.deleted).count();

    rsx! {
        div { class: "inspector",
            h2 { "Inspector" }
            div { class: "field",
                label { "Frames per second" }
                input {
                    r#type: "number",
                    min: "5",
                    max: "60",
                    value: "{fps_val}",
                    oninput: move |evt| {
                        if let Ok(v) = evt.value().parse::<u32>() {
                            fps.set(v.clamp(5, 60));
                        }
                    },
                }
            }
            div { class: "field",
                label { "Loop count (0 = infinite)" }
                input {
                    r#type: "number",
                    min: "0",
                    max: "10000",
                    value: "{loop_val}",
                    oninput: move |evt| {
                        if let Ok(v) = evt.value().parse::<u16>() {
                            loop_count.set(v);
                        }
                    },
                }
            }

            h2 { style: "margin-top: 14px;", "Trim" }
            div { class: "trim-row",
                button {
                    class: "ghost",
                    onclick: move |_| trim_in.set(Some(*current.read())),
                    "Set IN"
                }
                button {
                    class: "ghost",
                    onclick: move |_| trim_out.set(Some(*current.read())),
                    "Set OUT"
                }
                button {
                    class: "ghost",
                    onclick: move |_| {
                        trim_in.set(None);
                        trim_out.set(None);
                    },
                    "Clear"
                }
            }
            div { class: "trim-status", "in: {in_label}   out: {out_label}" }
            button {
                class: "primary trim-apply",
                onclick: move |_| {
                    let lo_hi = match (*trim_in.read(), *trim_out.read()) {
                        (Some(a), Some(b)) if a <= b => Some((a, b)),
                        (Some(a), Some(b)) => Some((b, a)),
                        _ => None,
                    };
                    if let Some((lo, hi)) = lo_hi {
                        frames.with_mut(|fs| {
                            for (i, f) in fs.iter_mut().enumerate() {
                                if i < lo || i > hi {
                                    f.deleted = true;
                                }
                            }
                        });
                    }
                },
                "Trim to selection"
            }

            div { class: "stats",
                div { span { "Total frames" } span { "{total_frames}" } }
                div { span { "Active" } span { "{active}" } }
                div { span { "Deleted" } span { "{total_frames - active}" } }
            }
        }
    }
}

#[component]
fn Timeline(
    decoded: Signal<Vec<Option<String>>>,
    frames: Signal<Vec<EditableFrame>>,
    current: Signal<usize>,
    trim_in: Signal<Option<usize>>,
    trim_out: Signal<Option<usize>>,
    playing: Signal<bool>,
    fps: Signal<u32>,
) -> Element {
    let cur_idx = *current.read();
    let total = frames.read().len();
    let in_idx = *trim_in.read();
    let out_idx = *trim_out.read();
    let playing_val = *playing.read();

    // Cancellation flag — same one the editor's decoder uses. Guards
    // every signal read in the playback loop so we don't panic on a
    // `.read()` after the window has closed.
    let alive = use_context::<AliveFlag>().0;

    let on_play_click = move |_| {
        let was_playing = *playing.read();
        playing.set(!was_playing);
        if !was_playing && total > 0 {
            let alive = alive.clone();
            // Self-terminating playback loop. Sleeps a frame, then
            // checks `alive` (window not closed) AND `playing`
            // (user didn't pause) before advancing.
            spawn(async move {
                loop {
                    if !alive.load(Ordering::SeqCst) {
                        return;
                    }
                    if !*playing.read() {
                        break;
                    }
                    let dt_ms =
                        ((1000 / (*fps.read()).max(1)).max(10)) as u64;
                    tokio::time::sleep(std::time::Duration::from_millis(dt_ms)).await;
                    if !alive.load(Ordering::SeqCst) {
                        return;
                    }
                    if !*playing.read() {
                        break;
                    }
                    advance_to_next_active(current, frames);
                }
            });
        }
    };

    rsx! {
        div { class: "timeline",
            div { class: "controls",
                button {
                    class: "ghost",
                    onclick: on_play_click,
                    if playing_val { "⏸ Pause" } else { "▶ Play" }
                }
                button {
                    class: "ghost",
                    onclick: move |_| current.set(0),
                    "|◀"
                }
                button {
                    class: "ghost",
                    onclick: move |_| current.set(total.saturating_sub(1)),
                    "▶|"
                }
                div { class: "spacer" }
                div { class: "position",
                    "Frame {cur_idx + 1} / {total}"
                }
            }
            div { class: "strip",
                {
                    // Build the per-frame classes + URI lookups inside
                    // a Rust closure — the rsx for-loop doesn't allow
                    // arbitrary statements before the element body, so
                    // we hand it an iterator of pre-built elements.
                    (0..total).map(move |i| {
                        let frames_snapshot = frames.read();
                        let deleted = frames_snapshot.get(i).map(|f| f.deleted).unwrap_or(false);
                        let mut classes = String::from("frame");
                        if i == cur_idx { classes.push_str(" current"); }
                        if deleted { classes.push_str(" deleted"); }
                        if Some(i) == in_idx { classes.push_str(" in-mark"); }
                        if Some(i) == out_idx { classes.push_str(" out-mark"); }
                        let uri = decoded.read().get(i).cloned().flatten();
                        rsx! {
                            div {
                                key: "frame-{i}",
                                class: "{classes}",
                                title: "Click to scrub, right-click to toggle delete",
                                onclick: move |_| current.set(i),
                                oncontextmenu: move |evt| {
                                    evt.prevent_default();
                                    frames.with_mut(|fs| {
                                        if let Some(f) = fs.get_mut(i) {
                                            f.deleted = !f.deleted;
                                        }
                                    });
                                },
                                if let Some(u) = uri {
                                    img { src: "{u}", alt: "frame {i}" }
                                }
                                span { class: "idx", "{i}" }
                            }
                        }
                    })
                }
            }
        }
    }
}

/// Move `current` to the next frame whose `deleted` flag is false.
/// Wraps to 0 at the end. No-op if every frame is deleted.
fn advance_to_next_active(
    mut current: Signal<usize>,
    frames: Signal<Vec<EditableFrame>>,
) {
    let fs = frames.read();
    if fs.is_empty() {
        return;
    }
    let mut next = (*current.read() + 1) % fs.len();
    for _ in 0..fs.len() {
        if !fs[next].deleted {
            current.set(next);
            return;
        }
        next = (next + 1) % fs.len();
    }
    // All deleted — leave current alone.
}

/// Kick off a worker thread that runs `encode_to_gif` against the
/// active frames + the current FPS slider, and a polling task that
/// drains progress updates into `export_status`. Runs from an event
/// handler — paths are passed in rather than re-read from context so
/// no hook calls happen below the top of the component.
#[allow(clippy::too_many_arguments)]
fn start_export(
    frames: Signal<Vec<EditableFrame>>,
    fps: Signal<u32>,
    loop_count: Signal<u16>,
    copy_on_export: Signal<bool>,
    mut export_status: Signal<ExportStatus>,
    out_path: PathBuf,
    spool_dir: PathBuf,
    sidecar_path: PathBuf,
) {
    let fps_val = (*fps.read()).clamp(5, 60);
    let delay_ms = (1000 / fps_val).max(10);
    let active: Vec<crate::export::gif::FrameInput> = frames
        .read()
        .iter()
        .filter(|f| !f.deleted)
        .map(|f| crate::export::gif::FrameInput {
            png_path: f.path.clone(),
            delay_ms,
        })
        .collect();
    let total = active.len();
    if total == 0 {
        export_status.set(ExportStatus::Failed(
            "Nothing to export — all frames are deleted.".into(),
        ));
        return;
    }

    let lc = *loop_count.read();
    let copy_clipboard = *copy_on_export.read();

    export_status.set(ExportStatus::Encoding { done: 0, total });

    let (tx, rx) = mpsc::channel::<ExportProgress>();
    let tx_progress = tx.clone();

    std::thread::Builder::new()
        .name("grabit-gif-encode".into())
        .spawn(move || {
            let progress = move |done: usize, total: usize| {
                let _ = tx_progress.send(ExportProgress::Tick { done, total });
            };
            match crate::export::gif::encode_to_gif(&active, lc, &out_path, progress) {
                Ok(()) => {
                    if copy_clipboard {
                        // GIFs go on the clipboard as CF_HDROP (a file
                        // drop) — that's how Slack / Discord / Outlook
                        // preserve animation when pasting. CF_DIB
                        // would only carry the first frame.
                        if let Err(e) = crate::export::copy_file_to_clipboard(&out_path) {
                            warn!("gif: copy on export failed: {e}");
                        } else {
                            info!("gif: copied to clipboard as file drop");
                        }
                    }
                    if let Err(e) = std::fs::remove_dir_all(&spool_dir) {
                        warn!("gif: cleanup spool {}: {e}", spool_dir.display());
                    }
                    if let Err(e) = std::fs::remove_file(&sidecar_path) {
                        log::debug!(
                            "gif: cleanup sidecar {}: {e}",
                            sidecar_path.display()
                        );
                    }
                    let _ = tx.send(ExportProgress::Done(out_path));
                }
                Err(e) => {
                    let _ = tx.send(ExportProgress::Failed(format!("{e:#}")));
                }
            }
        })
        .expect("spawn grabit-gif-encode");

    // Polling task — drains the receiver into export_status and exits
    // when the channel closes. Bounded sleep keeps it cheap; the worker
    // posts progress every frame which is well below tens-of-ms cadence.
    spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            loop {
                match rx.try_recv() {
                    Ok(ExportProgress::Tick { done, total }) => {
                        export_status.set(ExportStatus::Encoding { done, total });
                    }
                    Ok(ExportProgress::Done(p)) => {
                        export_status.set(ExportStatus::Done(p));
                        return;
                    }
                    Ok(ExportProgress::Failed(e)) => {
                        export_status.set(ExportStatus::Failed(e));
                        return;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
        }
    });
}

/// Decode a frame PNG, downscale to THUMB_W × THUMB_H (aspect
/// preserved), re-encode as PNG, base64-encode into a `data:` URI.
/// Used both for the timeline thumbnails and the center preview —
/// preview displays the same downscaled image, which is fine because
/// browsers upscale `<img>` smoothly and the recorder's region rect is
/// usually small enough that no detail is lost at 156×88.
fn build_thumb_uri(path: &std::path::Path) -> Option<String> {
    let img = image::open(path).ok()?;
    let resized = img.thumbnail(THUMB_W, THUMB_H);
    let mut bytes: Vec<u8> = Vec::new();
    resized
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        STANDARD.encode(&bytes)
    ))
}

#[cfg(windows)]
fn show_in_explorer(path: &std::path::Path) {
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
fn show_in_explorer(_path: &std::path::Path) {}
