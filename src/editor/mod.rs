//! Editor subsystem.
//!
//! As of 1.6 the editor renders through Dioxus desktop (Wry +
//! WebView2) — see `dx_app.rs`. The previous eframe implementation
//! (`app.rs`) and its egui-specific helpers have been retired.

pub mod commands;
pub mod document;
pub mod dx_app;
pub mod gif_app;
pub mod rasterize;
pub mod tools;

use crate::app::paths::AppPaths;
use crate::capture::CaptureResult;
use crate::settings::Settings;
use anyhow::{Context, Result};
use log::info;
use std::path::PathBuf;

/// Persist the capture to disk and spawn a fresh `grabit.exe --editor …`
/// subprocess pre-loaded with it. Subprocess isolation kept from the
/// eframe era: Wry/Dioxus also doesn't appreciate hosting two top-level
/// windows in one process, and the marker-file IPC the tray uses is
/// already stable around per-window subprocesses.
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

/// Blocking editor entry used by the `--editor` subprocess. Wires
/// straight through to `dx_app::run_blocking` — this thin wrapper
/// keeps the call-site signature in `main.rs` stable across the
/// eframe-to-Dioxus port.
pub fn run_blocking(
    document: document::Document,
    png_path: PathBuf,
    grabit_path: PathBuf,
    copy_to_clipboard: bool,
    paths: AppPaths,
    settings: Settings,
) -> Result<()> {
    dx_app::run_blocking(
        document,
        png_path,
        grabit_path,
        copy_to_clipboard,
        paths,
        settings,
    )
}
