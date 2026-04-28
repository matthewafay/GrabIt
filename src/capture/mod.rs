pub mod cursor;
pub mod delay;
pub mod exact_dims;
pub mod gdi;
pub mod gif_record;
pub mod object_pick;
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
    /// Capture a region of exactly `width` x `height` physical pixels;
    /// the user picks where to place it via the `exact_dims` overlay.
    /// Cancelling yields `Ok(None)`.
    ExactDims { width: u32, height: u32 },
    /// Hover over a UI element (button, menu item, list row) via the
    /// UIA picker and capture its bounding rect. Cancelling yields
    /// `Ok(None)`. Resolved to a `Region` internally before capture;
    /// the picker also installs a `SetWinEventHook` so menus stay
    /// pinned while hovered.
    Object,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    // Annotate flow (allow_windows = false): capture-first, then show a
    // frozen overlay painted with that capture so any transient UI (tray
    // popups, context menus, hover tooltips) is preserved in the final
    // image even after focus moves to the overlay and dismisses it. The
    // selected region is cropped out of the already-captured bitmap — we
    // never do a second BitBlt.
    if let CaptureTarget::Interactive { allow_windows: false } = req.target {
        let cursor = if req.include_cursor {
            cursor::sample().ok().flatten()
        } else {
            None
        };
        let (frozen, frozen_rect) = gdi::capture_virtual_desktop()
            .context("GDI fullscreen capture for frozen overlay")?;
        let region = match region::select_on_frozen(&frozen)? {
            region::RegionResult::Region(r) => r,
            region::RegionResult::Window(_) | region::RegionResult::Cancelled => {
                // Window pick is disabled in frozen mode; any other variant
                // counts as a cancel.
                return Ok(None);
            }
        };
        return Ok(Some(assemble_from_frozen(frozen, frozen_rect, region, cursor)));
    }

    // Resolve Interactive / ExactDims / Object before the delay/countdown
    // so the overlay's closing does not flash into the output.
    let resolved_target = match req.target.clone() {
        CaptureTarget::Interactive { allow_windows } => match region::select(allow_windows)? {
            region::RegionResult::Region(r) => CaptureTarget::Region(r),
            region::RegionResult::Window(h) => CaptureTarget::Window(h),
            region::RegionResult::Cancelled => return Ok(None),
        },
        CaptureTarget::ExactDims { width, height } => match exact_dims::pick(width, height)? {
            exact_dims::ExactDimsResult::Region(r) => CaptureTarget::Region(r),
            exact_dims::ExactDimsResult::Cancelled => return Ok(None),
        },
        CaptureTarget::Object => match object_pick::pick()? {
            object_pick::ObjectPickResult::Region(r) => CaptureTarget::Region(r),
            object_pick::ObjectPickResult::Cancelled => return Ok(None),
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
        CaptureTarget::Interactive { .. }
        | CaptureTarget::ExactDims { .. }
        | CaptureTarget::Object => {
            unreachable!("already resolved above")
        }
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

/// Crop a pre-captured virtual-desktop bitmap down to the user's selected
/// region and build a `CaptureResult`. Used by the capture-first annotate
/// path so the overlay isn't allowed to race the bitmap (transient UI
/// stays inside the captured pixels).
fn assemble_from_frozen(
    frozen: RgbaImage,
    frozen_rect: Rect,
    region: Rect,
    cursor: Option<CursorLayer>,
) -> CaptureResult {
    // Express the region in the bitmap's coordinate space (the bitmap was
    // captured starting at frozen_rect.x/y). Clamp to stay inside the
    // bitmap bounds even if the user drags off-screen.
    let src_x = (region.x - frozen_rect.x).max(0);
    let src_y = (region.y - frozen_rect.y).max(0);
    let max_w = frozen.width() as i32 - src_x;
    let max_h = frozen.height() as i32 - src_y;
    let w = (region.width as i32).min(max_w).max(1) as u32;
    let h = (region.height as i32).min(max_h).max(1) as u32;

    let base = image::imageops::crop_imm(&frozen, src_x as u32, src_y as u32, w, h)
        .to_image();

    let adjusted_rect = Rect { x: region.x, y: region.y, width: w, height: h };

    let metadata = CaptureMetadata {
        captured_at: Utc::now(),
        foreground_title: platform::foreground_window_title(),
        foreground_process: platform::foreground_process_name(),
        os_version: platform::os_version_string(),
        monitors: platform::enumerate_monitors(),
        capture_rect: adjusted_rect,
    };

    let cursor = cursor.and_then(|mut c| {
        c.x -= adjusted_rect.x;
        c.y -= adjusted_rect.y;
        if c.x + c.image.width() as i32 <= 0
            || c.y + c.image.height() as i32 <= 0
            || c.x >= adjusted_rect.width as i32
            || c.y >= adjusted_rect.height as i32
        {
            None
        } else {
            Some(c)
        }
    });

    CaptureResult { base, cursor, metadata }
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
