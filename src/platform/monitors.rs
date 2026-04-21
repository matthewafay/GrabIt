//! Virtual-desktop monitor enumeration.
//!
//! `capture::CaptureMetadata` carries a `Vec<MonitorInfo>` sampled via this
//! module at capture time. M0 had a stubbed version inside `capture/mod.rs`
//! that always reported `scale_factor = 1.0`; this is the replacement with
//! real per-monitor DPI pulled from `GetDpiForMonitor`.

use crate::capture::{MonitorInfo, Rect};

#[cfg(windows)]
pub fn enumerate() -> Vec<MonitorInfo> {
    use parking_lot::Mutex;
    use std::sync::Arc;
    use windows::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
    use windows::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
    };

    const MONITORINFOF_PRIMARY: u32 = 0x00000001;

    let out: Arc<Mutex<Vec<MonitorInfo>>> = Arc::new(Mutex::new(Vec::new()));
    let out_ptr = Arc::into_raw(out.clone());

    unsafe extern "system" fn enum_proc(
        hmon: HMONITOR,
        _dc: HDC,
        _rc: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        let p = &mut info as *mut MONITORINFOEXW as *mut MONITORINFO;
        if !GetMonitorInfoW(hmon, p).as_bool() {
            return TRUE;
        }
        let r = info.monitorInfo.rcMonitor;
        let rect = Rect {
            x: r.left,
            y: r.top,
            width: (r.right - r.left).max(0) as u32,
            height: (r.bottom - r.top).max(0) as u32,
        };
        let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;
        let dpi = crate::platform::dpi::dpi_for_monitor(hmon);
        let scale = crate::platform::dpi::scale_for_dpi(dpi);
        let list = data.0 as *const Mutex<Vec<MonitorInfo>>;
        (*list).lock().push(MonitorInfo { rect, scale_factor: scale, is_primary });
        TRUE
    }

    unsafe {
        let lparam = LPARAM(out_ptr as isize);
        let _ = EnumDisplayMonitors(None, None, Some(enum_proc), lparam);
        // Reclaim the Arc ref count we leaked through the callback.
        let _ = Arc::from_raw(out_ptr);
    }
    Arc::try_unwrap(out).ok().map(|m| m.into_inner()).unwrap_or_default()
}

#[cfg(not(windows))]
pub fn enumerate() -> Vec<MonitorInfo> {
    Vec::new()
}

/// Bounding rect of the entire virtual desktop across all monitors.
#[cfg(windows)]
pub fn virtual_desktop_rect() -> Rect {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(0) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(0) as u32,
        }
    }
}

#[cfg(not(windows))]
pub fn virtual_desktop_rect() -> Rect {
    Rect { x: 0, y: 0, width: 0, height: 0 }
}
