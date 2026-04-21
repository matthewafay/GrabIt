//! Editor subsystem.
//!
//! M2 boots a minimal eframe/egui window with canvas + Arrow tool; M3 adds
//! more annotation tools on top of the same `AnnotationNode` scene graph.

pub mod app;
pub mod document;
pub mod rasterize;

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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GrabIt editor")
            .with_inner_size([init_w + 16.0, init_h + 80.0]),
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
        Box::new(move |_cc| {
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
