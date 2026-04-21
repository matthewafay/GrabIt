#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autostart;
mod capture;
mod editor;
mod export;
mod hotkeys;
mod platform;
mod settings;
mod tray;

use anyhow::{Context, Result};
use log::{error, info, warn};

fn main() -> Result<()> {
    init_logging();
    install_panic_hook();

    info!("GrabIt v{} starting", env!("CARGO_PKG_VERSION"));
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();

    let _instance_guard = match app::single_instance::acquire() {
        Ok(g) => g,
        Err(app::single_instance::Error::AlreadyRunning) => {
            info!("Another GrabIt instance is already running; exiting.");
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("single-instance check failed: {e}")),
    };

    let paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    info!("app data: {}", paths.data_dir.display());
    info!("output dir: {}", paths.output_dir.display());

    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // WGC requires an initialized COM apartment on the thread that creates
        // the capture session. APARTMENTTHREADED is the documented choice for
        // UI-adjacent threads.
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if hr.is_err() {
            warn!("CoInitializeEx returned {hr:?}; continuing (likely already initialized)");
        }
    }

    let settings = settings::Settings::load_or_default(&paths);
    settings.save(&paths).ok();

    // On first run, honor the persisted autostart preference.
    if let Err(e) = autostart::sync(&settings.launch_at_startup) {
        warn!("autostart sync failed: {e}");
    }

    let app_state = app::AppState::new(paths, settings);

    if let Err(e) = run_event_loop(app_state) {
        error!("event loop exited with error: {e:?}");
        return Err(e);
    }

    info!("GrabIt shutting down cleanly");
    Ok(())
}

fn init_logging() {
    // Default level: INFO in release, DEBUG in debug.
    let default = if cfg!(debug_assertions) { "debug" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default))
        .format_timestamp_secs()
        .init();
}

/// Route panics to `%APPDATA%\GrabIt\logs\panic.log`. Release builds have no
/// console, so without this a crash would be silent. Falls back to the
/// temp directory if APPDATA resolution fails (should never happen, but
/// we don't want the hook itself to panic).
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let line = format!(
            "[{}] {}\n{}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            info,
            std::backtrace::Backtrace::force_capture()
        );
        let log_path = dirs::config_dir()
            .map(|d| d.join("GrabIt").join("logs").join("panic.log"))
            .unwrap_or_else(|| std::env::temp_dir().join("grabit-panic.log"));
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true).append(true).open(&log_path)
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
        prev(info);
    }));
}

fn run_event_loop(mut state: app::AppState) -> Result<()> {
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<app::Command>();

    let _tray = tray::Tray::install(cmd_tx.clone(), &state.settings)
        .context("install system tray")?;

    let _hotkeys = hotkeys::Registrar::install(cmd_tx.clone(), &state.settings.hotkey)
        .context("register global hotkey")?;

    let tray_rx = tray_icon::menu::MenuEvent::receiver().clone();
    let tray_icon_rx = tray_icon::TrayIconEvent::receiver().clone();
    let hotkey_rx = global_hotkey::GlobalHotKeyEvent::receiver().clone();

    // Pump Windows messages on the main thread while also consuming events
    // from the crossbeam channels. We do this by blocking-poll on the native
    // message loop and draining non-Win32 channels on each iteration.
    #[cfg(windows)]
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
    };

    loop {
        // Drain app-level command channel.
        while let Ok(cmd) = cmd_rx.try_recv() {
            if matches!(cmd, app::Command::Quit) {
                return Ok(());
            }
            if let Err(e) = app::dispatch(&mut state, cmd) {
                error!("command failed: {e:?}");
            }
        }

        // Drain tray/hotkey event channels (they forward into cmd_tx via
        // callbacks installed in install(), so in practice this is a safety
        // net for any stray events we didn't translate).
        while let Ok(ev) = tray_rx.try_recv() {
            tray::on_menu_event(ev, &cmd_tx);
        }
        while let Ok(ev) = tray_icon_rx.try_recv() {
            tray::on_tray_event(ev, &cmd_tx);
        }
        while let Ok(ev) = hotkey_rx.try_recv() {
            hotkeys::on_event(ev, &cmd_tx);
        }

        // Pump one Win32 message (non-blocking). Sleep briefly if idle to
        // keep CPU flat.
        #[cfg(windows)]
        unsafe {
            let mut msg = MSG::default();
            let msg_ptr = &mut msg as *mut MSG;
            if PeekMessageW(msg_ptr, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return Ok(());
                }
                let _ = TranslateMessage(msg_ptr as *const MSG);
                DispatchMessageW(msg_ptr as *const MSG);
            } else {
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        }
        #[cfg(not(windows))]
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}
