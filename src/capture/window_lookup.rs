//! Process-name → window enumeration for the headless `--capture` CLI.
//!
//! `EnumWindows` walks every top-level HWND once. For each one we filter
//! out invisible / cloaked / zero-area / privileged-process windows, then
//! resolve the owning process's image basename (`code.exe`, `notepad.exe`)
//! using the same `OpenProcess` + `GetProcessImageFileNameW` idiom as
//! `super::platform::foreground_process_name`. Matching is case-insensitive
//! and tolerant of a missing `.exe` suffix.

use super::Rect;
use anyhow::{anyhow, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct WindowMatch {
    pub hwnd: isize,
    pub pid: u32,
    pub process: String,
    pub title: String,
    pub rect: Rect,
}

#[cfg(windows)]
pub fn enumerate_top_level() -> Vec<WindowMatch> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let acc = unsafe { &mut *(lparam.0 as *mut Vec<WindowMatch>) };
        if let Some(m) = inspect(hwnd) {
            acc.push(m);
        }
        BOOL(1)
    }

    let mut acc: Vec<WindowMatch> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut acc as *mut _ as isize));
    }
    acc
}

#[cfg(not(windows))]
pub fn enumerate_top_level() -> Vec<WindowMatch> {
    Vec::new()
}

/// Find every visible top-level window owned by a process whose exe basename
/// matches `name` (case-insensitive). `name` may be given with or without
/// the `.exe` suffix — `"code"` and `"code.exe"` both match `code.exe`.
pub fn find_by_process(name: &str) -> Vec<WindowMatch> {
    let needle = normalize_process_name(name);
    enumerate_top_level()
        .into_iter()
        .filter(|m| m.process.eq_ignore_ascii_case(&needle))
        .collect()
}

/// Pick the largest window from a candidate set. Stable: ties broken by
/// HWND value so repeat invocations on an unchanged desktop pick the same
/// window. Returns `None` for an empty input.
pub fn pick_largest(matches: &[WindowMatch]) -> Option<&WindowMatch> {
    matches.iter().max_by_key(|m| {
        let area = (m.rect.width as u64) * (m.rect.height as u64);
        (area, m.hwnd)
    })
}

/// Get a window's screen rectangle via `GetWindowRect`. Matches the bounds
/// `window_pick::capture_window` uses for screenshots so PNG and GIF output
/// of the same target produce identical extents.
#[cfg(windows)]
pub fn window_rect(hwnd_isize: isize) -> Result<Rect> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let hwnd = HWND(hwnd_isize as *mut _);
    if hwnd.0.is_null() {
        return Err(anyhow!("null HWND"));
    }
    unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return Err(anyhow!("GetWindowRect failed for HWND 0x{:x}", hwnd_isize));
        }
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        Ok(Rect { x: r.left, y: r.top, width: w as u32, height: h as u32 })
    }
}

#[cfg(not(windows))]
pub fn window_rect(_hwnd_isize: isize) -> Result<Rect> {
    Err(anyhow!("window_rect is Windows-only"))
}

fn normalize_process_name(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.to_ascii_lowercase().ends_with(".exe") {
        trimmed.to_string()
    } else {
        format!("{trimmed}.exe")
    }
}

#[cfg(windows)]
unsafe fn inspect(hwnd: windows::Win32::Foundation::HWND) -> Option<WindowMatch> {
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::System::ProcessStatus::GetProcessImageFileNameW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    if !IsWindowVisible(hwnd).as_bool() {
        return None;
    }

    // DWMWA_CLOAKED rejects UWP-suspended apps, virtual-desktop-hidden
    // windows, and Edge/VS Code helper surfaces that pass IsWindowVisible
    // but never paint anything visible to the user.
    let mut cloaked: u32 = 0;
    let _ = DwmGetWindowAttribute(
        hwnd,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut _,
        std::mem::size_of::<u32>() as u32,
    );
    if cloaked != 0 {
        return None;
    }

    // Skip windows with neither a title nor a sane size — that's the cheap
    // proxy for "user-facing surface" without enumerating window classes.
    let title_len = GetWindowTextLengthW(hwnd);
    let mut wr = RECT::default();
    if GetWindowRect(hwnd, &mut wr).is_err() {
        return None;
    }
    let w = wr.right - wr.left;
    let h = wr.bottom - wr.top;
    if w <= 1 || h <= 1 {
        return None;
    }
    // Minimized windows park at (-32000, -32000) per Windows convention.
    // Capturing them yields a 237x39 sliver of nothing useful, so skip.
    if wr.left <= -30000 || wr.top <= -30000 {
        return None;
    }
    if title_len <= 0 && (w * h) < 64 * 64 {
        return None;
    }

    let title = if title_len > 0 {
        let mut buf = vec![0u16; (title_len + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied > 0 {
            String::from_utf16_lossy(&buf[..copied as usize])
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid as *mut u32));
    if pid == 0 {
        return None;
    }

    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut name_buf = [0u16; MAX_PATH as usize];
    let len = GetProcessImageFileNameW(handle, &mut name_buf);
    let _ = CloseHandle(handle);
    if len == 0 {
        return None;
    }
    let full = String::from_utf16_lossy(&name_buf[..len as usize]);
    let process = full.rsplit('\\').next().unwrap_or(&full).to_string();

    Some(WindowMatch {
        hwnd: hwnd.0 as isize,
        pid,
        process,
        title,
        rect: Rect { x: wr.left, y: wr.top, width: w as u32, height: h as u32 },
    })
}
