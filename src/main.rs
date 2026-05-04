#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod capture;
mod cli;
mod editor;
mod export;
mod history;
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

    let args: Vec<String> = std::env::args().collect();

    // Top-level help. Caught before the subprocess dispatch so `--help`,
    // `-h`, or bare `help` always print usage instead of falling through to
    // the tray app (which silently exits on a duplicate-instance launch and
    // looked like a no-op when scripts probed it). Defer to the verb-level
    // dispatcher when `--capture` is also present so `--capture help` still
    // prints the full flag matrix.
    let has_capture = args.iter().any(|a| a == "--capture");
    if !has_capture
        && args
            .iter()
            .skip(1)
            .any(|a| matches!(a.as_str(), "--help" | "-h" | "help"))
    {
        print_top_level_help();
        return Ok(());
    }

    // Editor subprocess mode: `grabit.exe --editor <sidecar.grabit> [--png-out
    // <path.png>] [--clipboard]`. Each capture spawns a fresh editor process
    // because winit 0.30 refuses to recreate its event loop within a single
    // process, so per-capture threads don't work.
    if let Some(idx) = args.iter().position(|a| a == "--editor") {
        let grabit = args.get(idx + 1)
            .ok_or_else(|| anyhow::anyhow!("--editor requires a path"))?
            .clone();
        let png_out = arg_value(&args, "--png-out");
        let clipboard = args.iter().any(|a| a == "--clipboard");
        return run_editor_subprocess(&grabit, png_out.as_deref(), clipboard);
    }
    if args.iter().any(|a| a == "--settings") {
        return run_settings_subprocess();
    }
    if let Some(idx) = args.iter().position(|a| a == "--gif-editor") {
        let sidecar = args.get(idx + 1)
            .ok_or_else(|| anyhow::anyhow!("--gif-editor requires a path"))?
            .clone();
        return run_gif_editor_subprocess(&sidecar);
    }
    if args.iter().any(|a| a == "--history") {
        return run_history_subprocess();
    }
    // Headless capture for Claude Code / scripts. Runs as a fresh
    // subprocess that bypasses the single-instance guard so it can
    // coexist with the resident tray app. See `cli::run` and
    // CLAUDE.md for the surface.
    if args.iter().any(|a| a == "--capture") {
        return run_capture_subprocess(&args);
    }

    let _instance_guard = match app::single_instance::acquire() {
        Ok(g) => g,
        Err(app::single_instance::Error::AlreadyRunning) => return Ok(()),
        Err(e) => return Err(anyhow::anyhow!("single-instance check failed: {e}")),
    };

    let mut paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
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
    apply_output_dir_override(&mut paths, &settings);

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

/// Apply the user's output-directory override from settings onto the
/// bootstrapped `AppPaths`. No-op when the override is unset, blank, or
/// the path can't be created.
fn apply_output_dir_override(paths: &mut app::paths::AppPaths, settings: &settings::Settings) {
    let Some(raw) = settings.output_dir.as_deref().map(str::trim) else { return };
    if raw.is_empty() { return; }
    let candidate = std::path::PathBuf::from(raw);
    if let Err(e) = std::fs::create_dir_all(&candidate) {
        warn!("output_dir override {} unusable: {e}; keeping default", candidate.display());
        return;
    }
    info!("output_dir override: {}", candidate.display());
    paths.output_dir = candidate;
}

fn print_top_level_help() {
    let msg = concat!(
        "GrabIt v", env!("CARGO_PKG_VERSION"), " — Windows screenshot + GIF tool\n",
        "\n",
        "USAGE:\n",
        "  grabit.exe                        Launch the system-tray app (default).\n",
        "  grabit.exe --capture <verb> ...   Headless capture for scripts / Claude Code.\n",
        "  grabit.exe --help                 This message.\n",
        "\n",
        "HEADLESS CAPTURE:\n",
        "  grabit.exe --capture help         Full verb + flag reference.\n",
        "  grabit.exe --capture screenshot   Single PNG.\n",
        "  grabit.exe --capture gif          Fire-and-wait GIF recording.\n",
        "  grabit.exe --capture list-windows Enumerate top-level windows as JSON.\n",
        "\n",
        "DOCS:\n",
        "  https://github.com/matthewafay/GrabIt/blob/main/CLAUDE.md\n",
        "  https://github.com/matthewafay/GrabIt/blob/main/docs/CAPTURE-CLI.md\n",
    );
    print!("{msg}");
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn run_capture_subprocess(args: &[String]) -> Result<()> {
    let paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    init_logging(&paths.log_file());
    // DPI awareness has to be set before any capture happens, otherwise
    // GetWindowRect / BitBlt see virtualized coordinates on HiDPI displays
    // and the output PNG is the wrong size or shifted.
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();
    info!("capture subprocess: {:?}", &args[1..]);
    cli::run(args, &paths)
}

fn run_settings_subprocess() -> Result<()> {
    let paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    init_logging(&paths.log_file());
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();
    info!("settings subprocess start");
    let initial = settings::Settings::load_or_default(&paths);
    settings::ui::run_blocking(paths, initial)
}

fn run_history_subprocess() -> Result<()> {
    let mut paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    init_logging(&paths.log_file());
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();
    let settings = settings::Settings::load_or_default(&paths);
    apply_output_dir_override(&mut paths, &settings);
    info!("history subprocess start");
    history::run_blocking(paths, settings)
}

fn run_gif_editor_subprocess(sidecar_path: &str) -> Result<()> {
    let paths = app::paths::AppPaths::bootstrap().context("create app data directories")?;
    init_logging(&paths.log_file());
    platform::dpi::init_process_awareness();
    platform::fonts::register_with_gdi();
    let settings = settings::Settings::load_or_default(&paths);
    info!("gif editor subprocess \u{2192} {sidecar_path}");
    editor::gif_app::run_blocking(
        std::path::PathBuf::from(sidecar_path),
        paths,
        settings,
    )
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
    let settings = settings::Settings::load_or_default(&paths);

    info!("editor subprocess → {}", grabit_path.display());
    editor::run_blocking(document, png_path, grabit_path, clipboard, paths, settings)
}

/// Execute a hotkey-originated command from the worker thread. Capture
/// commands run inline so they aren't gated on the main thread; everything
/// else is forwarded to the main loop via `cmd_tx` (those commands touch
/// `AppState` which lives on main).
///
/// Settings are re-read from `settings.json` per capture so we always use
/// the user's latest `include_cursor` / `copy_to_clipboard` without
/// needing a cross-thread sync channel.
fn handle_hotkey_command(
    cmd: app::Command,
    paths: &app::paths::AppPaths,
    cmd_tx: &crossbeam_channel::Sender<app::Command>,
) {
    use capture::{CaptureRequest, CaptureTarget};
    match cmd {
        app::Command::CaptureFullscreen => {
            let settings = settings::Settings::load_or_default(paths);
            let req = CaptureRequest {
                target: CaptureTarget::Fullscreen,
                delay_ms: 0,
                include_cursor: settings.include_cursor,
            };
            match capture::perform(req) {
                Ok(Some(result)) => {
                    let saved = match export::save_png(&result, paths) {
                        Ok(p) => Some(p),
                        Err(e) => {
                            warn!("fullscreen capture save failed: {e}");
                            None
                        }
                    };
                    if settings.copy_to_clipboard {
                        if let Err(e) = export::copy_to_clipboard(&result, saved.as_deref()) {
                            warn!("fullscreen clipboard copy failed: {e}");
                        }
                    }
                    info!("fullscreen capture (hotkey) complete");
                }
                Ok(None) => info!("fullscreen capture cancelled"),
                Err(e) => warn!("fullscreen capture failed: {e}"),
            }
        }
        app::Command::CaptureAndAnnotate => {
            let settings = settings::Settings::load_or_default(paths);
            let req = CaptureRequest {
                target: CaptureTarget::Interactive { allow_windows: false },
                delay_ms: 0,
                include_cursor: settings.include_cursor,
            };
            match capture::perform(req) {
                Ok(Some(result)) => {
                    if let Err(e) = editor::open_from_capture(
                        result,
                        paths,
                        settings.copy_to_clipboard,
                    ) {
                        warn!("editor spawn failed: {e}");
                    }
                }
                Ok(None) => info!("annotate flow cancelled"),
                Err(e) => warn!("annotate capture failed: {e}"),
            }
        }
        app::Command::CaptureGif => {
            // Same toggle semantics as the tray-driven path. Re-press of
            // the GIF chord while a recording is in flight stops it; an
            // idle press starts a new one.
            let settings = settings::Settings::load_or_default(paths);
            app::run_gif_capture(paths, &settings);
        }
        other => {
            // Non-capture commands still need AppState access — send to
            // main's cmd channel.
            if let Err(e) = cmd_tx.send(other) {
                warn!("worker \u{2192} main send failed: {e}");
            }
        }
    }
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
        (state.settings.gif_hotkey.clone(), app::Command::CaptureGif),
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

    // Drain hotkey events on a dedicated worker thread. For capture
    // commands (CaptureFullscreen / CaptureAndAnnotate) the worker runs
    // the capture *itself* — this bypasses the main thread's event loop
    // entirely, which means the capture fires even while the main thread
    // is parked inside a modal UI (e.g. the GrabIt tray popup menu). The
    // menu stays open and shows up in the screenshot.
    //
    // Non-capture commands (Quit, RefreshHotkeys, SetLaunchAtStartup, …)
    // still need to touch the main-thread `AppState` so they're forwarded
    // via `cmd_tx` as before.
    let paths_for_worker = state.paths.clone();
    let worker_tx = cmd_tx.clone();
    std::thread::Builder::new()
        .name("grabit-hotkey-drain".into())
        .spawn(move || {
            while let Ok(ev) = hotkey_rx.recv() {
                if let Some(cmd) = hotkeys::command_for_event(&ev) {
                    handle_hotkey_command(cmd, &paths_for_worker, &worker_tx);
                }
            }
        })
        .expect("spawn grabit-hotkey-drain");

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
        // Hotkey events are drained by the dedicated worker thread above;
        // nothing to do for them here.

        // Cross-process marker files from the editor + settings subprocesses.
        // Subprocess IPC is file-based because crossbeam channels don't span
        // process boundaries.
        check_marker_files(&state.paths, &cmd_tx);

        // Settings subprocess has saved — reload settings + rebuild
        // the hotkey registrar with the new global bindings.
        let settings_marker = state.paths.data_dir.join(".settings_refresh");
        if settings_marker.exists() {
            let _ = std::fs::remove_file(&settings_marker);
            state.settings = settings::Settings::load_or_default(&state.paths);
            apply_output_dir_override(&mut state.paths, &state.settings);
            let bindings = [
                (state.settings.hotkey.clone(), app::Command::CaptureFullscreen),
                (state.settings.annotate_hotkey.clone(), app::Command::CaptureAndAnnotate),
                (state.settings.gif_hotkey.clone(), app::Command::CaptureGif),
            ];
            drop(hotkeys);
            hotkeys = match hotkeys::Registrar::install(cmd_tx.clone(), &bindings) {
                Ok(r) => r,
                Err(e) => {
                    error!("re-register hotkeys after settings reload failed: {e:?}");
                    return Err(e);
                }
            };
            let preset_hk: Vec<hotkeys::PresetHotkey> = state
                .presets
                .bound_hotkeys()
                .into_iter()
                .map(|(chord, name)| hotkeys::PresetHotkey { chord, preset_name: name })
                .collect();
            let report = hotkeys.refresh_hotkeys(&preset_hk);
            for (name, chord, reason) in &report.failed {
                warn!("preset {name:?} hotkey {chord:?} not bound: {reason}");
            }
            // Update the accelerator labels on the two tray capture
            // entries so they reflect the just-saved chords. In-place
            // update via muda's set_accelerator + set_text — no icon
            // flicker, works symmetrically for both fullscreen and
            // annotate.
            tray.refresh_hotkey_labels(&state.settings);
            info!("settings reloaded");
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
