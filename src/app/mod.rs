pub mod paths;
pub mod single_instance;

use crate::capture::{CaptureRequest, CaptureTarget};
use crate::settings::Settings;
use anyhow::Result;
use log::{info, warn};
use paths::AppPaths;

/// Top-level app state carried through the event loop.
pub struct AppState {
    pub paths: AppPaths,
    pub settings: Settings,
}

impl AppState {
    pub fn new(paths: AppPaths, settings: Settings) -> Self {
        Self { paths, settings }
    }
}

/// High-level commands produced by the tray/hotkeys and consumed by the
/// main loop. Keeping this enum small and stable is what lets the
/// input-surface modules (tray, hotkeys) stay decoupled from capture/editor.
#[derive(Debug, Clone)]
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
    /// Open the output folder in Explorer.
    OpenOutputFolder,
    /// Toggle "Launch at startup" — flips the HKCU Run entry and persists
    /// the choice to settings.
    ToggleAutostart,
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
                    target: CaptureTarget::Interactive,
                    delay_ms: 0,
                    include_cursor: state.settings.include_cursor,
                },
            )?;
        }
        Command::CaptureAndAnnotate => {
            info!("dispatch: CaptureAndAnnotate");
            let req = CaptureRequest {
                target: CaptureTarget::Interactive,
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
        Command::ToggleAutostart => {
            state.settings.launch_at_startup = !state.settings.launch_at_startup;
            if let Err(e) = crate::autostart::sync(&state.settings.launch_at_startup) {
                warn!("autostart toggle failed: {e}");
            }
            state.settings.save(&state.paths).ok();
        }
        Command::OpenSettings => {
            // Editor/settings UI lands in M2. For M0 we just open the TOML
            // file in the user's default editor so they can hand-edit it.
            let path = state.paths.settings_file();
            open_in_explorer(&path);
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
        if let Err(e) = crate::export::copy_to_clipboard(&result) {
            warn!("clipboard copy failed: {e}");
        }
    }
    info!("capture saved to {}", out_path.display());
    Ok(())
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
