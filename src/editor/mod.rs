//! Editor subsystem.
//!
//! M2 boots a minimal eframe/egui window with canvas + Arrow tool; M3 adds
//! more annotation tools on top of the same `AnnotationNode` scene graph.

pub mod app;
pub mod document;
pub mod rasterize;

/// Decode the embedded logo PNG for use as an eframe window icon.
/// Returns `None` if decoding fails — eframe then falls back to its default.
fn load_app_icon_data() -> Option<egui::IconData> {
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
use anyhow::{Context, Result};
use log::{info, warn};
use std::path::PathBuf;
use std::thread;

/// Spawn an editor window on a dedicated worker thread, pre-loaded with the
/// given capture. Returns immediately; the thread stays alive until the user
/// closes the editor window.
pub fn open_from_capture(
    result: CaptureResult,
    paths: &AppPaths,
    copy_to_clipboard: bool,
) -> Result<()> {
    let document =
        document::from_capture(&result).context("build document from capture")?;

    // One timestamped filename reused for PNG + `.grabit` sidecar.
    let png_path: PathBuf = paths.default_capture_filename("png");
    let grabit_path = png_path.with_extension("grabit");

    thread::Builder::new()
        .name("grabit-editor".to_string())
        .spawn(move || {
            let png_display = png_path.display().to_string();
            info!("editor thread start → {png_display}");
            if let Err(e) = run_editor(document, png_path, grabit_path, copy_to_clipboard) {
                warn!("editor exited with error: {e:?}");
            }
            info!("editor thread end");
        })
        .context("spawn editor thread")?;

    Ok(())
}

fn run_editor(
    document: document::Document,
    png_path: PathBuf,
    grabit_path: PathBuf,
    copy_to_clipboard: bool,
) -> Result<()> {
    let w = document.base_width.max(320);
    let h = document.base_height.max(240);
    // Cap initial window to something sane for giant captures.
    let init_w = (w as f32).min(1600.0);
    let init_h = (h as f32).min(1000.0);

    let viewport = {
        let mut vb = egui::ViewportBuilder::default()
            .with_title("GrabIt editor")
            .with_inner_size([init_w + 16.0, init_h + 80.0]);
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
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

/// Register JetBrains Mono as the first-choice face for both of egui's
/// built-in font families. egui keeps its default fallbacks (Ubuntu-Light,
/// Noto Emoji, etc.) behind ours so missing glyphs still render.
fn install_jetbrains_mono(ctx: &egui::Context) {
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
