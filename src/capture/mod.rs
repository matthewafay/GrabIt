pub mod cursor;
pub mod delay;
pub mod gdi;
pub mod region;
pub mod wgc;
pub mod window_pick;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use image::RgbaImage;
use serde::{Deserialize, Serialize};

/// What the user asked to capture.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub target: CaptureTarget,
    pub delay_ms: u32,
    pub include_cursor: bool,
}

#[derive(Debug, Clone)]
pub enum CaptureTarget {
    /// Entire virtual desktop across all monitors.
    Fullscreen,
    /// Rect already selected (e.g. via overlay or exact-dims).
    Region(Rect),
    /// Specific window by HWND.
    Window(isize),
    /// Run the interactive selector overlay, then capture whatever the
    /// user chose. `allow_windows = false` restricts the overlay to region
    /// drag-select only — short clicks stay in the overlay instead of
    /// capturing the window under the cursor. Cancelling yields `Ok(None)`.
    Interactive { allow_windows: bool },
    // Object { ui_element: ElementRef } — M6
    // Multi { rects: Vec<Rect> } — M6
}

/// A completed capture. Cursor lives on its own RGBA layer so feature #2
/// (Edit cursor) can move/resize/delete it without touching the base image.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub base: RgbaImage,
    pub cursor: Option<CursorLayer>,
    pub metadata: CaptureMetadata,
}

#[derive(Debug, Clone)]
pub struct CursorLayer {
    pub image: RgbaImage,
    /// Screen-space position of the cursor top-left (already adjusted by
    /// the hotspot). Relative to the capture's top-left, so callers can
    /// composite with a straight blit.
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureMetadata {
    pub captured_at: DateTime<Utc>,
    pub foreground_title: Option<String>,
    pub foreground_process: Option<String>,
    pub os_version: String,
    pub monitors: Vec<MonitorInfo>,
    /// The rect in virtual-screen coordinates that was captured.
    pub capture_rect: Rect,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub rect: Rect,
    pub scale_factor: f32,
    pub is_primary: bool,
}

/// Entry point. Resolves an `Interactive` target to a concrete region/window
/// via the overlay, applies any requested countdown, dispatches to a
/// backend, attaches a cursor layer if requested, and returns the result.
/// Returns `Ok(None)` if the user cancelled the interactive selector.
pub fn perform(req: CaptureRequest) -> Result<Option<CaptureResult>> {
    // Resolve Interactive before the delay/countdown so the overlay's
    // closing does not flash into the output.
    let resolved_target = match req.target.clone() {
        CaptureTarget::Interactive { allow_windows } => match region::select(allow_windows)? {
            region::RegionResult::Region(r) => CaptureTarget::Region(r),
            region::RegionResult::Window(h) => CaptureTarget::Window(h),
            region::RegionResult::Cancelled => return Ok(None),
        },
        t => t,
    };

    if req.delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(req.delay_ms as u64));
    }

    // Capture cursor first so we have an accurate position — capturing the
    // base image can take tens of milliseconds, during which the cursor may
    // move. Because the cursor is drawn as a separate layer, this staleness
    // is inherent and we minimize it by sampling cursor state up-front.
    let cursor = if req.include_cursor {
        cursor::sample().ok().flatten()
    } else {
        None
    };

    let (base, rect) = match resolved_target {
        CaptureTarget::Fullscreen => gdi::capture_virtual_desktop()
            .context("GDI fullscreen capture")?,
        CaptureTarget::Region(r) => gdi::capture_region(r)
            .context("GDI region capture")?,
        CaptureTarget::Window(hwnd) => window_pick::capture_window(hwnd)
            .context("window capture")?,
        CaptureTarget::Interactive { .. } => unreachable!("already resolved above"),
    };

    let metadata = CaptureMetadata {
        captured_at: Utc::now(),
        foreground_title: platform::foreground_window_title(),
        foreground_process: platform::foreground_process_name(),
        os_version: platform::os_version_string(),
        monitors: platform::enumerate_monitors(),
        capture_rect: rect,
    };

    // Cursor position is in screen coords; make it relative to capture rect.
    let cursor = cursor.and_then(|mut c| {
        c.x -= rect.x;
        c.y -= rect.y;
        // Drop cursors that fall outside the captured rect.
        if c.x + c.image.width() as i32 <= 0
            || c.y + c.image.height() as i32 <= 0
            || c.x >= rect.width as i32
            || c.y >= rect.height as i32
        {
            None
        } else {
            Some(c)
        }
    });

    Ok(Some(CaptureResult { base, cursor, metadata }))
}

mod platform {
    use super::MonitorInfo;

    pub fn foreground_window_title() -> Option<String> {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() { return None; }
            let mut buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len <= 0 { return None; }
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        }
        #[cfg(not(windows))]
        { None }
    }

    pub fn foreground_process_name() -> Option<String> {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
            use windows::Win32::System::ProcessStatus::GetProcessImageFileNameW;
            use windows::Win32::System::Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            use windows::Win32::UI::WindowsAndMessaging::{
                GetForegroundWindow, GetWindowThreadProcessId,
            };

            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() { return None; }

            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
            if pid == 0 { return None; }

            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; MAX_PATH as usize];
            let len = GetProcessImageFileNameW(handle, &mut buf);
            let _ = CloseHandle(handle);
            if len == 0 { return None; }
            let full = String::from_utf16_lossy(&buf[..len as usize]);
            Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
        }
        #[cfg(not(windows))]
        { None }
    }

    pub fn os_version_string() -> String {
        #[cfg(windows)]
        {
            // Win32's GetVersionEx is shimmed for app-compat; RtlGetVersion is
            // the reliable path but requires ntdll linkage. A simple product
            // string is enough for the capture-info stamp in M4.
            format!("Windows {}", os_info_line())
        }
        #[cfg(not(windows))]
        { "unknown".to_string() }
    }

    #[cfg(windows)]
    fn os_info_line() -> String {
        use windows::Win32::System::SystemInformation::{
            GetVersionExW, OSVERSIONINFOEXW, OSVERSIONINFOW,
        };
        let mut v = OSVERSIONINFOEXW::default();
        v.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOEXW>() as u32;
        unsafe {
            // Safe: OSVERSIONINFOEXW begins with OSVERSIONINFOW.
            let ptr = &mut v as *mut OSVERSIONINFOEXW as *mut OSVERSIONINFOW;
            if GetVersionExW(ptr).is_ok() {
                return format!(
                    "{}.{}.{}",
                    v.dwMajorVersion, v.dwMinorVersion, v.dwBuildNumber
                );
            }
        }
        "unknown".to_string()
    }

    pub fn enumerate_monitors() -> Vec<MonitorInfo> {
        crate::platform::monitors::enumerate()
    }
}
