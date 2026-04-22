#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod autostart;
mod capture;
mod editor;
mod export;
mod hotkeys;
mod platform;
mod presets;
mod settings;
mod styles;
mod tray;

use anyhow::{Context, Result};
use log::{error, info, warn};

fn main() -> Result<()> {
    install_panic_hook();

    // Editor subprocess mode: `grabit.exe --editor <sidecar.grabit> [--png-out
    // <path.png>] [--clipboard]`. Each capture spawns a fresh editor process
    // because winit 0.30 refuses to recreate its event loop within a single
    // process, so per-capture threads don't work.
    let args: Vec<String> = std::env::args().collect();
    if let Some(idx) = args.iter().position(|a| a == "--editor") {
        let grabit = args.get(idx + 1)
            .ok_or_else(|| anyhow::anyhow!("--editor requires a path"))?
            .clone();
        let png_out = arg_value(&args, "--png-out");
        let clipboard = args.iter().any(|a| a == "--clipboard");
        return run_editor_subprocess(&grabit, png_out.as_deref(), clipboard);
    }

    let _instance_guard = match app::single_instance::acquire() {
        Ok(g) => g,
        Err(app::single_instance::Error::AlreadyRunning) => return Ok(()),
        Err(e) => return Err(anyhow::anyhow!("single-instance check failed: {e}")),
    };

    let paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    init_logging(&paths.log_file());

    info!("GrabIt v{} starting", env!("CARGO_PKG_VERSION"));
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();

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

    // Presets + quick styles (M5). Seed a default preset on first run so
    // the tray "Presets" submenu isn't empty.
    let (preset_store, seeded) = presets::PresetStore::load_or_seed_default(&paths);
    if seeded {
        info!("seeded default preset file under {}", paths.presets_dir.display());
    }
    let style_store = styles::StyleStore::load(&paths);

    let app_state = app::AppState::new(paths, settings, preset_store, style_store);

    if let Err(e) = run_event_loop(app_state) {
        error!("event loop exited with error: {e:?}");
        return Err(e);
    }

    info!("GrabIt shutting down cleanly");
    Ok(())
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn run_editor_subprocess(
    grabit_path: &str,
    png_out: Option<&str>,
    clipboard: bool,
) -> Result<()> {
    let paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    init_logging(&paths.log_file());
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();

    let grabit_path = std::path::PathBuf::from(grabit_path);
    let png_path = png_out
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| grabit_path.with_extension("png"));
    let document = editor::document::load(&grabit_path)
        .with_context(|| format!("load sidecar {}", grabit_path.display()))?;

    info!("editor subprocess → {}", grabit_path.display());
    editor::run_blocking(document, png_path, grabit_path, clipboard, paths)
}

fn init_logging(log_file: &std::path::Path) {
    let default = if cfg!(debug_assertions) { "debug" } else { "info" };
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(default),
    );
    builder.format_timestamp_secs();
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(log_file) {
        builder.target(env_logger::Target::Pipe(Box::new(f)));
    }
    let _ = builder.try_init();
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

    let mut tray = tray::Tray::install(cmd_tx.clone(), &state.settings, &state.presets)
        .context("install system tray")?;

    let hotkey_bindings = [
        (state.settings.hotkey.clone(), app::Command::CaptureFullscreen),
        (state.settings.annotate_hotkey.clone(), app::Command::CaptureAndAnnotate),
    ];
    let mut hotkeys = hotkeys::Registrar::install(cmd_tx.clone(), &hotkey_bindings)
        .context("register global hotkeys")?;

    // Install preset-bound hotkeys on top. Collisions are logged and the
    // offending presets are left unbound — the user can fix them in the
    // editor's presets panel.
    let preset_hotkeys: Vec<hotkeys::PresetHotkey> = state
        .presets
        .bound_hotkeys()
        .into_iter()
        .map(|(chord, name)| hotkeys::PresetHotkey { chord, preset_name: name })
        .collect();
    let report = hotkeys.refresh_hotkeys(&preset_hotkeys);
    for (name, chord, reason) in &report.failed {
        warn!("preset {name:?} hotkey {chord:?} not bound: {reason}");
    }

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
            // RefreshHotkeys needs access to the Registrar (which lives here
            // in the event loop, not in AppState) and to the Tray (so the
            // Presets submenu can be rebuilt). Intercept it before
            // forwarding to the normal dispatcher, which handles the
            // PresetStore reload.
            let needs_rebind = matches!(cmd, app::Command::RefreshHotkeys);
            if let Err(e) = app::dispatch(&mut state, cmd) {
                error!("command failed: {e:?}");
            }
            if needs_rebind {
                let desired: Vec<hotkeys::PresetHotkey> = state
                    .presets
                    .bound_hotkeys()
                    .into_iter()
                    .map(|(chord, name)| hotkeys::PresetHotkey { chord, preset_name: name })
                    .collect();
                let report = hotkeys.refresh_hotkeys(&desired);
                for (name, chord, reason) in &report.failed {
                    warn!("preset {name:?} hotkey {chord:?} not bound: {reason}");
                }
                if let Err(e) = tray.rebuild_presets(&state.presets) {
                    warn!("tray presets rebuild failed: {e}");
                }
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

        // Cross-thread marker files from the editor's presets/styles panels.
        // The editor runs on a worker thread and can't reach this loop
        // directly; dropping small marker files is a lightweight bridge.
        check_marker_files(&state.paths, &cmd_tx);

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

/// Poll for editor-dropped marker files. Cheap per-tick; even with
/// `std::fs::metadata` on a missing path this costs tens of microseconds on
/// a modern Windows NVMe. Markers are one-shot: we consume and delete them.
fn check_marker_files(paths: &app::paths::AppPaths, cmd_tx: &crossbeam_channel::Sender<app::Command>) {
    let refresh_marker = paths.data_dir.join(".presets_refresh");
    if refresh_marker.exists() {
        let _ = std::fs::remove_file(&refresh_marker);
        if let Err(e) = cmd_tx.send(app::Command::RefreshHotkeys) {
            warn!("send RefreshHotkeys: {e}");
        }
    }
    let capture_marker = paths.data_dir.join(".capture_preset");
    if capture_marker.exists() {
        match std::fs::read_to_string(&capture_marker) {
            Ok(name) => {
                let _ = std::fs::remove_file(&capture_marker);
                let trimmed = name.trim().to_string();
                if !trimmed.is_empty() {
                    if let Err(e) = cmd_tx.send(app::Command::CapturePreset(trimmed)) {
                        warn!("send CapturePreset: {e}");
                    }
                }
            }
            Err(e) => {
                warn!("read capture_preset marker: {e}");
                let _ = std::fs::remove_file(&capture_marker);
            }
        }
    }
}
