pub mod paths;
pub mod single_instance;

use crate::capture::{CaptureRequest, CaptureTarget};
use crate::presets::{Preset, PresetStore, PresetTargetKind};
use crate::settings::Settings;
use crate::styles::StyleStore;
use anyhow::{Context, Result};
use log::{info, warn};
use paths::AppPaths;

/// Top-level app state carried through the event loop.
pub struct AppState {
    pub paths: AppPaths,
    pub settings: Settings,
    /// Loaded preset collection. Re-reads from disk on `RefreshHotkeys`.
    pub presets: PresetStore,
    /// Quick annotation styles. The editor reloads its own copy on open,
    /// so this is mainly here to keep first-run initialisation symmetric
    /// with presets (and to validate the styles file at startup).
    #[allow(dead_code)]
    pub styles: StyleStore,
}

impl AppState {
    pub fn new(
        paths: AppPaths,
        settings: Settings,
        presets: PresetStore,
        styles: StyleStore,
    ) -> Self {
        Self { paths, settings, presets, styles }
    }
}

/// High-level commands produced by the tray/hotkeys and consumed by the
/// main loop. Keeping this enum small and stable is what lets the
/// input-surface modules (tray, hotkeys) stay decoupled from capture/editor.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Several variants are reachable only via preset hotkeys,
                    // not from the slim tray menu.
pub enum Command {
    /// Capture the entire virtual desktop.
    CaptureFullscreen,
    /// Run the interactive region/window overlay.
    CaptureInteractive,
    /// Sleep for `delay_ms` then capture the virtual desktop. Countdown
    /// visuals are owned by `capture::delay`.
    CaptureWithDelay { delay_ms: u32 },
    /// Interactive capture → opens the editor with the result ready to
    /// annotate.
    CaptureAndAnnotate,
    /// Capture a region of exactly `width` x `height` physical pixels; the
    /// user positions it via the exact-dims overlay.
    CaptureExactDims { width: u32, height: u32 },
    /// Run the UI-Automation object / menu picker and capture whatever
    /// element the user highlights.
    CaptureObject,
    /// Begin (or stop) a GIF screen recording. Re-firing the chord while a
    /// recording is in progress acts as a stop toggle; the actual
    /// in-progress detection lives in `capture::gif_record`.
    CaptureGif,
    /// Fire the preset with the given display name. Resolved against the
    /// live `PresetStore`; missing names are logged and ignored so stale
    /// tray entries can't crash the app.
    CapturePreset(String),
    /// Reload presets from disk and re-register their hotkeys. Triggered
    /// from the editor's presets panel after an edit.
    RefreshHotkeys,
    /// Open the output folder in Explorer.
    OpenOutputFolder,
    /// Open the capture history viewer (mini gallery of recent PNGs/GIFs).
    OpenHistory,
    /// Open settings window (stub in M0).
    OpenSettings,
    /// Quit the application.
    Quit,
}

pub fn dispatch(state: &mut AppState, cmd: Command) -> Result<()> {
    match cmd {
        Command::CaptureFullscreen => {
            info!("dispatch: CaptureFullscreen");
            run_capture(
                state,
                CaptureRequest {
                    target: CaptureTarget::Fullscreen,
                    delay_ms: 0,
                    include_cursor: state.settings.include_cursor,
                },
            )?;
        }
        Command::CaptureInteractive => {
            info!("dispatch: CaptureInteractive");
            run_capture(
                state,
                CaptureRequest {
                    target: CaptureTarget::Interactive { allow_windows: true },
                    delay_ms: 0,
                    include_cursor: state.settings.include_cursor,
                },
            )?;
        }
        Command::CaptureAndAnnotate => {
            info!("dispatch: CaptureAndAnnotate");
            let req = CaptureRequest {
                // Annotate flow: drag-release is the only gesture — no
                // window-hover-click-to-capture ambiguity.
                target: CaptureTarget::Interactive { allow_windows: false },
                delay_ms: 0,
                include_cursor: state.settings.include_cursor,
            };
            let maybe = crate::capture::perform(req)?;
            if let Some(result) = maybe {
                crate::editor::open_from_capture(
                    result,
                    &state.paths,
                    state.settings.copy_to_clipboard,
                )?;
            } else {
                info!("annotate flow cancelled at capture stage");
            }
        }
        Command::CaptureExactDims { width, height } => {
            info!("dispatch: CaptureExactDims {width}x{height}");
            run_capture(
                state,
                CaptureRequest {
                    target: CaptureTarget::ExactDims { width, height },
                    delay_ms: 0,
                    include_cursor: state.settings.include_cursor,
                },
            )?;
        }
        Command::CaptureObject => {
            info!("dispatch: CaptureObject");
            run_capture(
                state,
                CaptureRequest {
                    target: CaptureTarget::Object,
                    delay_ms: 0,
                    include_cursor: state.settings.include_cursor,
                },
            )?;
        }
        Command::CaptureGif => {
            info!("dispatch: CaptureGif");
            // Tray-driven entry mirrors the hotkey path: same start/stop
            // toggle, same recorder, same `--gif-editor` subprocess.
            run_gif_capture(&state.paths, &state.settings);
        }
        Command::CapturePreset(name) => {
            info!("dispatch: CapturePreset {name:?}");
            // Resolve against the loaded store; if the preset was deleted
            // while the hotkey was still registered (unlikely — refresh
            // unregisters first — but possible during a hot edit) we log
            // and drop the command instead of erroring.
            let Some(preset) = state.presets.find(&name).cloned() else {
                warn!("preset {name:?} not found; command dropped");
                return Ok(());
            };
            run_preset_capture(state, &preset)?;
        }
        Command::RefreshHotkeys => {
            info!("dispatch: RefreshHotkeys (from settings UI)");
            state.presets = PresetStore::load(&state.paths);
            // Actual rebinding is handled by the event loop — it owns the
            // Registrar. The loop checks for this command and calls
            // `refresh_hotkeys` with the new preset list.
        }
        Command::CaptureWithDelay { delay_ms } => {
            info!("dispatch: CaptureWithDelay {delay_ms}ms");
            // Show a countdown overlay during the delay so the user knows
            // when the capture will fire. The overlay closes before the
            // capture so it cannot appear in the output.
            if let Err(e) = crate::capture::delay::countdown(delay_ms) {
                warn!("countdown overlay failed: {e}");
            }
            run_capture(
                state,
                CaptureRequest {
                    target: CaptureTarget::Fullscreen,
                    delay_ms: 0, // already waited via countdown
                    include_cursor: state.settings.include_cursor,
                },
            )?;
        }
        Command::OpenOutputFolder => {
            open_in_explorer(&state.paths.output_dir);
        }
        Command::OpenHistory => {
            // History runs as its own subprocess so it can host an
            // eframe window without contending for the tray's main
            // thread event loop. Same pattern as Settings + GIF editor.
            let exe = std::env::current_exe().context("resolve current exe")?;
            if let Err(e) = std::process::Command::new(exe).arg("--history").spawn() {
                warn!("spawn history subprocess failed: {e}");
            }
        }
        Command::OpenSettings => {
            // Settings GUI runs as a `grabit.exe --settings` subprocess so it
            // gets its own winit event loop (the tray process can't host a
            // second one).
            let exe = std::env::current_exe().context("resolve current exe")?;
            if let Err(e) = std::process::Command::new(exe).arg("--settings").spawn() {
                warn!("spawn settings subprocess failed: {e}");
            }
        }
        Command::Quit => { /* handled at loop root */ }
    }
    Ok(())
}

fn run_capture(state: &AppState, req: CaptureRequest) -> Result<()> {
    let maybe_result = crate::capture::perform(req)?;
    let Some(result) = maybe_result else {
        info!("capture cancelled");
        return Ok(());
    };
    let out_path = crate::export::save_png(&result, &state.paths)?;
    if state.settings.copy_to_clipboard {
        if let Err(e) = crate::export::copy_to_clipboard(&result, Some(&out_path)) {
            warn!("clipboard copy failed: {e}");
        }
    }
    info!("capture saved to {}", out_path.display());
    Ok(())
}

/// Preset-driven capture path. Builds a `CaptureRequest` from a `Preset`,
/// respects the preset's filename template / subfolder, and branches on
/// the preset's post-capture action.
fn run_preset_capture(state: &AppState, preset: &Preset) -> Result<()> {
    // Countdown + delay before resolving Interactive, matching the regular
    // delay path. The overlay closes before the shot fires.
    if preset.delay_ms > 0 {
        if let Err(e) = crate::capture::delay::countdown(preset.delay_ms) {
            warn!("preset countdown failed: {e}");
        }
    }

    let target = match preset.target {
        PresetTargetKind::Fullscreen => CaptureTarget::Fullscreen,
        PresetTargetKind::Region => CaptureTarget::Interactive { allow_windows: false },
        PresetTargetKind::Window => CaptureTarget::Interactive { allow_windows: true },
        PresetTargetKind::ExactDims => {
            // Presets with exact-dims must carry a non-zero size. If the
            // user saved one with 0x0, fall back to a reasonable default
            // and warn — preferable to silently running a zero capture.
            let w = if preset.width == 0 { 1920 } else { preset.width };
            let h = if preset.height == 0 { 1080 } else { preset.height };
            if preset.width == 0 || preset.height == 0 {
                warn!(
                    "preset {:?} exact-dims W/H is zero; using {w}x{h}",
                    preset.name
                );
            }
            CaptureTarget::ExactDims { width: w, height: h }
        }
        PresetTargetKind::Object => CaptureTarget::Object,
    };

    let req = CaptureRequest {
        target,
        delay_ms: 0, // already ticked via countdown above
        include_cursor: preset.include_cursor,
    };

    let Some(result) = crate::capture::perform(req)? else {
        info!("preset capture cancelled");
        return Ok(());
    };

    use crate::presets::PostAction;
    match preset.post_action {
        PostAction::CopyOnly => {
            // CopyOnly skips disk — no path to attach as CF_UNICODETEXT.
            if let Err(e) = crate::export::copy_to_clipboard(&result, None) {
                warn!("preset clipboard copy failed: {e}");
            } else {
                info!("preset {:?}: copied to clipboard (no disk save)", preset.name);
            }
        }
        PostAction::SaveOnly => {
            let out_path = save_with_preset(preset, &result, &state.paths)?;
            if state.settings.copy_to_clipboard {
                if let Err(e) = crate::export::copy_to_clipboard(&result, Some(&out_path)) {
                    warn!("clipboard copy failed: {e}");
                }
            }
            info!("preset {:?}: saved to {}", preset.name, out_path.display());
        }
        PostAction::Editor => {
            // Save alongside opening the editor so the PNG on disk exists
            // immediately — same as `CaptureAndAnnotate` today.
            if let Err(e) = save_with_preset(preset, &result, &state.paths) {
                warn!("preset PNG save (before editor) failed: {e}");
            }
            crate::editor::open_from_capture(
                result,
                &state.paths,
                state.settings.copy_to_clipboard,
            )?;
        }
    }
    Ok(())
}

/// Save a preset capture honouring its filename template + subfolder. The
/// `.grabit` sidecar is written next to the PNG so the capture can be
/// reopened in the editor even for `SaveOnly` presets.
fn save_with_preset(
    preset: &Preset,
    result: &crate::capture::CaptureResult,
    paths: &AppPaths,
) -> Result<std::path::PathBuf> {
    let window = result.metadata.foreground_title.as_deref();
    let png_path = preset.resolve_png_path(paths, window, chrono::Local::now());
    if let Some(parent) = png_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("create preset output dir {}: {e}", parent.display());
        }
    }
    crate::export::save_png_to(result, &png_path)?;
    Ok(png_path)
}

/// Shared GIF-capture entry. Used by both the tray menu route (via
/// `dispatch`) and the hotkey worker thread. If a recording is already in
/// flight, signals it to stop. Otherwise runs the recorder inline (region
/// pick + floating bar) and, on Stop, spawns a fresh
/// `grabit.exe --gif-editor <sidecar>` subprocess.
pub(crate) fn run_gif_capture(paths: &paths::AppPaths, settings: &Settings) {
    if crate::capture::gif_record::is_recording() {
        crate::capture::gif_record::request_stop();
        info!("gif: stop requested via toggle");
        return;
    }
    match crate::capture::gif_record::run(paths, settings) {
        Ok(Some(sidecar)) => {
            let exe = match std::env::current_exe() {
                Ok(p) => p,
                Err(e) => {
                    warn!("gif: resolve current exe: {e}");
                    return;
                }
            };
            if let Err(e) = std::process::Command::new(exe)
                .arg("--gif-editor")
                .arg(&sidecar)
                .spawn()
            {
                warn!("gif: spawn editor subprocess failed: {e}");
            } else {
                info!("gif: editor subprocess spawned \u{2192} {}", sidecar.display());
            }
        }
        Ok(None) => info!("gif recording cancelled"),
        Err(e) => warn!("gif recording failed: {e}"),
    }
}

fn open_in_explorer(path: &std::path::Path) {
    #[cfg(windows)]
    {
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
    {
        let _ = path;
    }
}
