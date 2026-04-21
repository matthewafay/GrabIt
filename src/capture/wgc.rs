//! Windows.Graphics.Capture (WGC) primary capture path.
//!
//! **Status: stubbed in M0.** The full WGC pipeline (Direct3D11 device,
//! `IGraphicsCaptureItemInterop`, frame-pool, session lifecycle, CPU readback
//! via `ID3D11DeviceContext::Map`) lands in M1 when per-window and per-region
//! captures need its superpowers (DPI correctness, window-occlusion handling,
//! HDR). M0 uses GDI exclusively because fullscreen virtual-desktop capture
//! is well-served by `BitBlt` and keeps the first milestone testable without
//! a large chunk of COM plumbing.
//!
//! When M1 activates this module:
//!   1. `D3D11CreateDevice(HARDWARE, BGRA_SUPPORT)` — shared with editor if
//!      we later move rendering onto wgpu over the same adapter.
//!   2. `GraphicsCaptureItem::CreateFromMonitor` / `...FromWindow` via the
//!      `IGraphicsCaptureItemInterop` free-threaded COM interface.
//!   3. `Direct3D11CaptureFramePool::CreateFreeThreaded` with `BGRA8` + 2
//!      buffers; attach `FrameArrived` with a channel sender.
//!   4. `session.IsCursorCaptureEnabled = false` — we draw cursor ourselves.
//!   5. On first frame: `CopyResource` to a staging texture, `Map(READ)`,
//!      copy rows with stride fix-up, unmap, close session.
//!
//! The M0 stub returns `Err(Unsupported)` so `capture::perform` can fall
//! back to GDI cleanly.

use super::Rect;
use anyhow::{anyhow, Result};
use image::RgbaImage;

#[allow(dead_code)]
pub fn is_available() -> bool {
    // Runtime check against Windows 10 1903+ will live here in M1. For
    // now we report unavailable so callers go to GDI.
    false
}

#[allow(dead_code)]
pub fn capture_monitor(_monitor_index: usize) -> Result<(RgbaImage, Rect)> {
    Err(anyhow!("WGC capture path activates in M1"))
}

#[allow(dead_code)]
pub fn capture_window(_hwnd: isize) -> Result<(RgbaImage, Rect)> {
    Err(anyhow!("WGC window capture activates in M1"))
}
