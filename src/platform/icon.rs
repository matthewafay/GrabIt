//! Shared window-icon loader for Dioxus desktop windows.
//!
//! All four Dioxus windows (history, settings, gif editor, annotation
//! editor) need the GrabIt logo on their title bar / Alt-Tab tile.
//! Dioxus's underlying window backend is tao, whose `Icon` is a
//! distinct type from egui's `IconData` and the tray-icon crate's
//! `Icon`. This module owns the decode-and-wrap glue so each window's
//! `run_blocking` can call one function instead of duplicating the
//! image::load_from_memory + Icon::from_rgba dance.
//!
//! The PNG itself is embedded at compile time via `include_bytes!`,
//! same source as the tray icon — so the binary always carries one
//! copy regardless of how many windows the user opens.

use dioxus::desktop::tao::window::Icon;

/// Decode the embedded `assets/icons/grabit.png` and wrap it as a
/// `tao::window::Icon`. Returns `None` if decoding fails — callers
/// should treat that as "use the OS default" rather than aborting.
pub fn load_window_icon() -> Option<Icon> {
    const PNG: &[u8] = include_bytes!("../../assets/icons/grabit.png");
    let img = image::load_from_memory(PNG).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}
