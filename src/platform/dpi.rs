//! Per-monitor DPI awareness.
//!
//! We set `PER_MONITOR_AWARE_V2` via the embedded application manifest
//! (`assets/manifest.xml`), which is the most reliable way to opt in — it
//! takes effect before any GDI calls. We additionally call
//! `SetProcessDpiAwarenessContext` at startup as a belt-and-suspenders so
//! DPI awareness is correct even when the exe is launched via a parent that
//! stripped the manifest (rare but possible with some launchers).

use log::{debug, warn};

/// Called once from `main` before any window creation or capture.
pub fn init_process_awareness() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::HiDpi::{
            SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        match SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            Ok(_) => debug!("process DPI awareness set to PerMonitorV2"),
            Err(e) => {
                // Non-fatal — the manifest usually already set this.
                warn!("SetProcessDpiAwarenessContext failed (manifest likely applied): {e}");
            }
        }
    }
}

/// DPI (dots per inch) for a given monitor handle. 96 = 100% scale.
#[cfg(windows)]
pub fn dpi_for_monitor(hmon: windows::Win32::Graphics::Gdi::HMONITOR) -> u32 {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    unsafe {
        let mut x: u32 = 96;
        let mut y: u32 = 96;
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        x
    }
}

#[allow(dead_code)] // used by the editor (M2+) to match canvas DPI to window DPI.
#[cfg(windows)]
pub fn dpi_for_window(hwnd: windows::Win32::Foundation::HWND) -> u32 {
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    unsafe { GetDpiForWindow(hwnd) }
}

/// DPI → scale factor (1.0 at 96 DPI).
pub fn scale_for_dpi(dpi: u32) -> f32 {
    dpi as f32 / 96.0
}
