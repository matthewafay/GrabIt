//! Editor subsystem.
//!
//! M2 boots a minimal eframe/egui window with canvas + Arrow tool; M3 adds
//! more annotation tools on top of the same `AnnotationNode` scene graph.

pub mod app;
pub mod commands;
pub mod document;
pub mod gif_app;
pub mod rasterize;
pub mod tools;

/// Decode the embedded logo PNG for use as an eframe window icon.
/// Returns `None` if decoding fails — eframe then falls back to its default.
pub(crate) fn load_app_icon_data() -> Option<egui::IconData> {
    const PNG: &[u8] = include_bytes!("../../assets/icons/grabit.png");
    let img = image::load_from_memory(PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

use crate::app::paths::AppPaths;
use crate::capture::CaptureResult;
use crate::settings::Settings;
use anyhow::{Context, Result};
use log::info;
use std::path::PathBuf;

/// Persist the capture to disk and spawn a fresh `grabit.exe --editor …`
/// subprocess pre-loaded with it. Subprocess isolation is required because
/// winit 0.30 rejects recreating its event loop inside a single process, so
/// we can't simply spawn a new editor thread per capture.
pub fn open_from_capture(
    result: CaptureResult,
    paths: &AppPaths,
    copy_to_clipboard: bool,
) -> Result<()> {
    let document =
        document::from_capture(&result).context("build document from capture")?;

    let png_path: PathBuf = paths.default_capture_filename("png");
    let grabit_path = png_path.with_extension("grabit");
    document::save(&document, &grabit_path)
        .with_context(|| format!("write sidecar {}", grabit_path.display()))?;

    let exe = std::env::current_exe().context("resolve current exe")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--editor").arg(&grabit_path);
    cmd.arg("--png-out").arg(&png_path);
    if copy_to_clipboard {
        cmd.arg("--clipboard");
    }
    cmd.spawn().context("spawn editor subprocess")?;

    info!("editor subprocess spawned → {}", grabit_path.display());
    Ok(())
}

/// Blocking editor entry used by the `--editor` subprocess. Runs eframe on
/// the current (main) thread and returns when the window closes.
pub fn run_blocking(
    document: document::Document,
    png_path: PathBuf,
    grabit_path: PathBuf,
    copy_to_clipboard: bool,
    paths: AppPaths,
    settings: Settings,
) -> Result<()> {
    // Editor window sizing:
    //  - Chrome adds ~16 px horizontal (margins / scrollbar) and ~80 px
    //    vertical (toolbar + status row) on top of the canvas area.
    //  - Floor: a 1×1 pixel capture would otherwise produce an unusable
    //    336×320 window. Clamp the initial size to `MIN_*` so the
    //    toolbar + inspector + status row have room to breathe.
    //  - Ceiling: cap at MAX_* so a giant 4K capture doesn't spawn a window
    //    larger than the screen. The canvas scrolls / fits-to-view inside.
    //  - `min_inner_size`: hard floor users can't shrink below without
    //    making the UI illegible.
    const MIN_W: f32 = 1000.0;
    const MIN_H: f32 = 700.0;
    const MAX_W: f32 = 1616.0;
    const MAX_H: f32 = 1080.0;
    const SHRINK_FLOOR_W: f32 = 800.0;
    const SHRINK_FLOOR_H: f32 = 550.0;
    let init_w = ((document.base_width as f32 + 16.0).max(MIN_W)).min(MAX_W);
    let init_h = ((document.base_height as f32 + 80.0).max(MIN_H)).min(MAX_H);

    let viewport = {
        let mut vb = egui::ViewportBuilder::default()
            .with_title("GrabIt editor")
            .with_inner_size([init_w, init_h])
            .with_min_inner_size([SHRINK_FLOOR_W, SHRINK_FLOOR_H]);
        if let Some(icon) = load_app_icon_data() {
            vb = vb.with_icon(std::sync::Arc::new(icon));
        }
        vb
    };

    let options = eframe::NativeOptions {
        viewport,
        // winit rejects non-main-thread event loops by default; we spawn the
        // editor on a worker thread so the main-thread tray loop stays live.
        // `with_any_thread(true)` is the supported escape hatch on Windows.
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
        "GrabIt editor",
        options,
        Box::new(move |cc| {
            install_jetbrains_mono(&cc.egui_ctx);
            Ok(Box::new(app::EditorApp::new(
                document,
                png_path,
                grabit_path,
                copy_to_clipboard,
                paths,
                settings,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

/// Register JetBrains Mono as the first-choice face for both of egui's
/// built-in font families. egui keeps its default fallbacks (Ubuntu-Light,
/// Noto Emoji, etc.) behind ours so missing glyphs still render.
pub(crate) fn install_jetbrains_mono(ctx: &egui::Context) {
    use crate::platform::fonts::{JETBRAINS_MONO_BOLD, JETBRAINS_MONO_REGULAR};

    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "jetbrains-mono".to_owned(),
        egui::FontData::from_static(JETBRAINS_MONO_REGULAR),
    );
    fonts.font_data.insert(
        "jetbrains-mono-bold".to_owned(),
        egui::FontData::from_static(JETBRAINS_MONO_BOLD),
    );

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let chain = fonts.families.entry(family).or_default();
        chain.insert(0, "jetbrains-mono".to_owned());
    }

    ctx.set_fonts(fonts);
}
