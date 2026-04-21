//! Countdown overlay for time-delayed captures.
//!
//! Shows a compact window in the center of the primary monitor with a
//! countdown number that ticks down each second. The window is destroyed
//! before `countdown()` returns, so the capture that fires afterward
//! cannot include the countdown UI.

use anyhow::{anyhow, Result};

#[cfg(windows)]
pub fn countdown(total_ms: u32) -> Result<()> {
    imp::run(total_ms)
}

#[cfg(not(windows))]
pub fn countdown(total_ms: u32) -> Result<()> {
    std::thread::sleep(std::time::Duration::from_millis(total_ms as u64));
    Ok(())
}

#[cfg(windows)]
mod imp {
    use super::*;
    use log::debug;
    use std::cell::Cell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontIndirectW, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
        InvalidateRect, SelectObject, SetBkMode, SetTextColor, TextOutW, FW_BOLD, HBRUSH, HGDIOBJ,
        LOGFONTW, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics,
        KillTimer, PeekMessageW, PostQuitMessage, RegisterClassExW, SetLayeredWindowAttributes,
        SetTimer, ShowWindow, TranslateMessage, UnregisterClassW, HCURSOR, HICON, LWA_ALPHA,
        LWA_COLORKEY, MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SW_SHOWNOACTIVATE, WM_DESTROY,
        WM_PAINT, WM_TIMER, WNDCLASSEXW, WNDCLASS_STYLES, WS_EX_LAYERED, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    const CLASS_NAME: PCWSTR = w!("GrabItCountdown");
    const TIMER_ID: usize = 1;
    const SIZE: i32 = 180;

    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    thread_local! {
        static REMAINING: Cell<u32> = Cell::new(0);
    }

    pub fn run(total_ms: u32) -> Result<()> {
        if total_ms == 0 { return Ok(()); }
        // Countdown in whole seconds, rounded up (e.g. 500ms still shows "1").
        let seconds = total_ms.div_ceil(1000);
        REMAINING.with(|c| c.set(seconds));

        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(PCWSTR::null())
                .map_err(|e| anyhow!("GetModuleHandle: {e}"))?
                .into();

            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: WNDCLASS_STYLES(0),
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: HICON::default(),
                hCursor: HCURSOR::default(),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: CLASS_NAME,
                hIconSm: HICON::default(),
            };
            let _ = RegisterClassExW(&class);

            let scr_w = GetSystemMetrics(SM_CXSCREEN);
            let scr_h = GetSystemMetrics(SM_CYSCREEN);
            let x = (scr_w - SIZE) / 2;
            let y = (scr_h - SIZE) / 2;

            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("GrabIt"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                SIZE,
                SIZE,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(|e| anyhow!("CreateWindowEx: {e}"))?;

            // Magenta = fully transparent → we paint a circle with anti-key
            // color for the body, so the surrounding square is invisible.
            let _ = SetLayeredWindowAttributes(
                hwnd,
                rgb(255, 0, 255),
                230,
                LWA_COLORKEY | LWA_ALPHA,
            );

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetTimer(hwnd, TIMER_ID, 1000, None);

            // Spin a local message loop that exits once REMAINING hits 0.
            let mut msg = MSG::default();
            let start = std::time::Instant::now();
            loop {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if REMAINING.with(|c| c.get()) == 0 { break; }
                // Safety net: never stay longer than total_ms + 500 ms.
                if start.elapsed().as_millis() as u32 > total_ms + 500 { break; }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }

            let _ = KillTimer(hwnd, TIMER_ID);
            let _ = DestroyWindow(hwnd);
            let _ = UnregisterClassW(CLASS_NAME, hinstance);
            debug!("countdown finished");
            Ok(())
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TIMER => {
                let remaining = REMAINING.with(|c| {
                    let n = c.get().saturating_sub(1);
                    c.set(n);
                    n
                });
                let _ = InvalidateRect(hwnd, None, true);
                if remaining == 0 {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                // Magenta fill (becomes transparent via LWA_COLORKEY).
                let bg = CreateSolidBrush(rgb(255, 0, 255));
                FillRect(hdc, &RECT { left: 0, top: 0, right: SIZE, bottom: SIZE }, bg);
                let _ = DeleteObject(HGDIOBJ(bg.0));

                // A dark square in the middle as the countdown body.
                let pad = 20;
                let body_rect = RECT {
                    left: pad,
                    top: pad,
                    right: SIZE - pad,
                    bottom: SIZE - pad,
                };
                let body = CreateSolidBrush(rgb(24, 24, 28));
                FillRect(hdc, &body_rect, body);
                let _ = DeleteObject(HGDIOBJ(body.0));

                let mut lf = LOGFONTW::default();
                lf.lfHeight = -88;
                lf.lfWeight = FW_BOLD.0 as i32;
                let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
                for (i, c) in face.iter().enumerate() {
                    if i < lf.lfFaceName.len() { lf.lfFaceName[i] = *c; }
                }
                let font = CreateFontIndirectW(&lf);
                let old = SelectObject(hdc, HGDIOBJ(font.0));
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, rgb(240, 240, 240));

                let n = REMAINING.with(|c| c.get().max(1));
                let text = format!("{}", n);
                let wide: Vec<u16> = text.encode_utf16().collect();
                // Rough horizontal centering; with monospace numerals this is close enough.
                let text_x = SIZE / 2 - 24 * (wide.len() as i32);
                let _ = TextOutW(hdc, text_x, 30, &wide);

                SelectObject(hdc, old);
                let _ = DeleteObject(HGDIOBJ(font.0));
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}
