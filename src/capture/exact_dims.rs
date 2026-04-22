//! Exact-dimensions positioner overlay (feature #6).
//!
//! Presents a layered, topmost window covering the entire virtual desktop.
//! A fixed-size rectangle (`width` x `height`, in physical pixels) follows
//! the cursor; the user positions it and commits:
//!
//! - **Mouse move** → the rectangle's top-left tracks the cursor (the
//!   rectangle is clamped so it stays inside the virtual desktop).
//! - **Click-and-drag** → pick up the rectangle by a specific grab point
//!   and drag the whole box with that offset preserved.
//! - **Left click / Enter** → commit.
//! - **Right click / Escape** → cancel.
//! - **Arrow keys** → nudge by 1px (Shift: 10px) for pixel-perfect placement.
//!
//! If the requested size equals or exceeds the virtual desktop on either
//! axis, the rectangle is centred on that axis and cannot be moved along it.
//! Dimensions are always in physical pixels — the process is per-monitor v2
//! DPI-aware, so raw-pixel coordinates are exactly what WGC/GDI consume.

use crate::capture::Rect;
use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub enum ExactDimsResult {
    /// User picked a placement; contains the virtual-screen rect that the
    /// capture backend should snapshot.
    Region(Rect),
    Cancelled,
}

#[cfg(windows)]
pub fn pick(width: u32, height: u32) -> Result<ExactDimsResult> {
    if width == 0 || height == 0 {
        return Err(anyhow!("exact dims width/height must be non-zero"));
    }
    imp::run(width, height)
}

#[cfg(not(windows))]
pub fn pick(_width: u32, _height: u32) -> Result<ExactDimsResult> {
    Err(anyhow!("exact-dims picker is Windows-only"))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::platform::monitors::virtual_desktop_rect;
    use log::debug;
    use std::cell::RefCell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontIndirectW, CreatePen, CreateSolidBrush, DeleteObject, EndPaint,
        FillRect, InvalidateRect, Rectangle, SelectObject, SetBkMode, SetTextColor, TextOutW,
        FONT_CHARSET, FW_SEMIBOLD, HBRUSH, HGDIOBJ, LOGFONTW, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, ReleaseCapture, SetCapture, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN,
        VK_RIGHT, VK_SHIFT, VK_UP,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        LoadCursorW, PostQuitMessage, RegisterClassExW, SetLayeredWindowAttributes, ShowWindow,
        TranslateMessage, UnregisterClassW, HCURSOR, HICON, IDC_SIZEALL, LWA_ALPHA, LWA_COLORKEY,
        MSG, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WM_RBUTTONDOWN, WNDCLASSEXW, WNDCLASS_STYLES,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    const CLASS_NAME: PCWSTR = w!("GrabItExactDimsOverlay");

    // Color key: magenta pixels become fully transparent via LWA_COLORKEY.
    const KEY_R: u8 = 255;
    const KEY_G: u8 = 0;
    const KEY_B: u8 = 255;
    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    struct State {
        /// Virtual desktop bounding rect (overlay-client coords start at 0,0
        /// and span this rect's width/height).
        virtual_rect: Rect,
        /// Target capture size in physical pixels.
        target_w: i32,
        target_h: i32,
        /// Top-left of the floating rectangle, in overlay-client coords.
        rect_x: i32,
        rect_y: i32,
        /// When dragging, the grab offset (cursor - rect_x/y) we maintain
        /// so the rectangle does not jump to the cursor's hotspot.
        dragging: bool,
        grab_dx: i32,
        grab_dy: i32,
        result: Option<ExactDimsResult>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub fn run(width: u32, height: u32) -> Result<ExactDimsResult> {
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
                hCursor: LoadCursorW(None, IDC_SIZEALL).unwrap_or(HCURSOR::default()),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: CLASS_NAME,
                hIconSm: HICON::default(),
            };
            let _ = RegisterClassExW(&class); // ignore ERROR_CLASS_ALREADY_EXISTS

            let vrect = virtual_desktop_rect();
            if vrect.width == 0 || vrect.height == 0 {
                return Err(anyhow!("virtual desktop has zero size"));
            }

            // Clamp requested size to the virtual desktop — if the user
            // asks for something larger than every monitor combined, we
            // still produce a sensible capture by clamping. (Callers may
            // prefer to error out instead; current UX is clamp + proceed.)
            let target_w = (width as i32).min(vrect.width as i32).max(1);
            let target_h = (height as i32).min(vrect.height as i32).max(1);

            // Start centred on the virtual desktop.
            let initial_x = (vrect.width as i32 - target_w) / 2;
            let initial_y = (vrect.height as i32 - target_h) / 2;

            let style_ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let style = WS_POPUP | WS_VISIBLE;

            let hwnd = CreateWindowExW(
                style_ex,
                CLASS_NAME,
                w!("GrabIt"),
                style,
                vrect.x,
                vrect.y,
                vrect.width as i32,
                vrect.height as i32,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(|e| anyhow!("CreateWindowEx: {e}"))?;

            if hwnd.0.is_null() {
                return Err(anyhow!(
                    "CreateWindowEx returned null (GetLastError: {:?})",
                    GetLastError()
                ));
            }

            // Magenta pixels fully transparent; everything else drawn at 75% alpha.
            let _ = SetLayeredWindowAttributes(
                hwnd,
                rgb(KEY_R, KEY_G, KEY_B),
                192,
                LWA_COLORKEY | LWA_ALPHA,
            );

            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    virtual_rect: vrect,
                    target_w,
                    target_h,
                    rect_x: initial_x,
                    rect_y: initial_y,
                    dragging: false,
                    grab_dx: 0,
                    grab_dy: 0,
                    result: None,
                });
            });

            let _ = ShowWindow(hwnd, SW_SHOW);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg as *mut MSG, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg as *const MSG);
                DispatchMessageW(&msg as *const MSG);
            }

            let result = STATE
                .with(|cell| cell.borrow_mut().take().and_then(|s| s.result))
                .unwrap_or(ExactDimsResult::Cancelled);

            let _ = UnregisterClassW(CLASS_NAME, hinstance);
            debug!("exact-dims picker exited: {result:?}");
            Ok(result)
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_NCCREATE => DefWindowProcW(hwnd, msg, wparam, lparam),
            WM_MOUSEMOVE => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                with_state(|s| {
                    // Track the cursor: if dragging, preserve the grab offset
                    // so the rectangle doesn't snap its top-left to the
                    // cursor. Otherwise, centre the rect on the cursor.
                    let (new_x, new_y) = if s.dragging {
                        (x - s.grab_dx, y - s.grab_dy)
                    } else {
                        (x - s.target_w / 2, y - s.target_h / 2)
                    };
                    s.rect_x = clamp_axis(new_x, s.target_w, s.virtual_rect.width as i32);
                    s.rect_y = clamp_axis(new_y, s.target_h, s.virtual_rect.height as i32);
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                with_state(|s| {
                    // If the click landed inside the rect, begin a drag with
                    // the grab offset. Otherwise treat the press as the start
                    // of a drag from the centre so the rect snaps there.
                    if x >= s.rect_x
                        && x < s.rect_x + s.target_w
                        && y >= s.rect_y
                        && y < s.rect_y + s.target_h
                    {
                        s.grab_dx = x - s.rect_x;
                        s.grab_dy = y - s.rect_y;
                    } else {
                        // Centre on the click point.
                        s.rect_x = clamp_axis(x - s.target_w / 2, s.target_w, s.virtual_rect.width as i32);
                        s.rect_y = clamp_axis(y - s.target_h / 2, s.target_h, s.virtual_rect.height as i32);
                        s.grab_dx = s.target_w / 2;
                        s.grab_dy = s.target_h / 2;
                    }
                    s.dragging = true;
                });
                let _ = SetCapture(hwnd);
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let _ = ReleaseCapture();
                // Commit on mouse-up: this is the single gesture that
                // produces a capture. Short clicks (no movement) also commit.
                let commit = with_state(|s| {
                    s.dragging = false;
                    Rect {
                        x: s.virtual_rect.x + s.rect_x,
                        y: s.virtual_rect.y + s.rect_y,
                        width: s.target_w as u32,
                        height: s.target_h as u32,
                    }
                });
                with_state(|s| s.result = Some(ExactDimsResult::Region(commit)));
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u32;
                if key == VK_ESCAPE.0 as u32 {
                    with_state(|s| s.result = Some(ExactDimsResult::Cancelled));
                    let _ = DestroyWindow(hwnd);
                } else if key == VK_RETURN.0 as u32 {
                    let commit = with_state(|s| Rect {
                        x: s.virtual_rect.x + s.rect_x,
                        y: s.virtual_rect.y + s.rect_y,
                        width: s.target_w as u32,
                        height: s.target_h as u32,
                    });
                    with_state(|s| s.result = Some(ExactDimsResult::Region(commit)));
                    let _ = DestroyWindow(hwnd);
                } else if matches!(
                    key,
                    k if k == VK_LEFT.0 as u32
                        || k == VK_RIGHT.0 as u32
                        || k == VK_UP.0 as u32
                        || k == VK_DOWN.0 as u32
                ) {
                    // Shift held → 10px steps; otherwise 1px.
                    let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                    let step = if shift { 10 } else { 1 };
                    let (dx, dy) = if key == VK_LEFT.0 as u32 {
                        (-step, 0)
                    } else if key == VK_RIGHT.0 as u32 {
                        (step, 0)
                    } else if key == VK_UP.0 as u32 {
                        (0, -step)
                    } else {
                        (0, step)
                    };
                    with_state(|s| {
                        s.rect_x = clamp_axis(
                            s.rect_x + dx,
                            s.target_w,
                            s.virtual_rect.width as i32,
                        );
                        s.rect_y = clamp_axis(
                            s.rect_y + dy,
                            s.target_h,
                            s.virtual_rect.height as i32,
                        );
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                with_state(|s| s.result = Some(ExactDimsResult::Cancelled));
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_CLOSE => {
                with_state(|s| {
                    if s.result.is_none() {
                        s.result = Some(ExactDimsResult::Cancelled);
                    }
                });
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Clamp `pos` so a rectangle of `size` along one axis of length
    /// `extent` stays fully inside `[0, extent]`. If `size >= extent`, the
    /// rectangle is centred on that axis.
    fn clamp_axis(pos: i32, size: i32, extent: i32) -> i32 {
        if size >= extent {
            (extent - size) / 2
        } else {
            pos.clamp(0, extent - size)
        }
    }

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
        STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let s = borrow.as_mut().expect("exact-dims state must be initialized");
            f(s)
        })
    }

    unsafe fn paint(hdc: windows::Win32::Graphics::Gdi::HDC) {
        let (virt_w, virt_h, rect_x, rect_y, target_w, target_h) = with_state(|s| {
            (
                s.virtual_rect.width as i32,
                s.virtual_rect.height as i32,
                s.rect_x,
                s.rect_y,
                s.target_w,
                s.target_h,
            )
        });

        // 1. Dim background — dark gray fills everything.
        let dim = CreateSolidBrush(rgb(30, 30, 30));
        let full = RECT { left: 0, top: 0, right: virt_w, bottom: virt_h };
        FillRect(hdc, &full, dim);
        let _ = DeleteObject(HGDIOBJ(dim.0));

        // 2. Rectangle cut-out — magenta = transparent per color key.
        let target = RECT {
            left: rect_x,
            top: rect_y,
            right: rect_x + target_w,
            bottom: rect_y + target_h,
        };
        let cut = CreateSolidBrush(rgb(KEY_R, KEY_G, KEY_B));
        FillRect(hdc, &target, cut);
        let _ = DeleteObject(HGDIOBJ(cut.0));

        // Outline.
        let pen = CreatePen(PS_SOLID, 2, rgb(0, 180, 255));
        let old = SelectObject(hdc, HGDIOBJ(pen.0));
        let _ = Rectangle(hdc, target.left, target.top, target.right, target.bottom);
        SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(pen.0));

        // Size label.
        let text = format!(
            "{} x {}  \u{2014} click or Enter to capture, Esc to cancel",
            target_w, target_h
        );
        let label_y = if target.top > 28 { target.top - 22 } else { target.bottom + 6 };
        draw_label(hdc, target.left + 4, label_y, &text);
    }

    unsafe fn draw_label(hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, text: &str) {
        let mut lf = LOGFONTW::default();
        lf.lfHeight = -16;
        lf.lfWeight = FW_SEMIBOLD.0 as i32;
        lf.lfCharSet = FONT_CHARSET(0);
        let face_str = format!("{}\0", crate::platform::fonts::FACE_NAME);
        let face: Vec<u16> = face_str.encode_utf16().collect();
        for (i, c) in face.iter().enumerate() {
            if i < lf.lfFaceName.len() {
                lf.lfFaceName[i] = *c;
            }
        }
        let font = CreateFontIndirectW(&lf);
        let old_font = SelectObject(hdc, HGDIOBJ(font.0));
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, rgb(240, 240, 240));
        let wide: Vec<u16> = text.encode_utf16().collect();
        let _ = TextOutW(hdc, x, y, &wide);
        SelectObject(hdc, old_font);
        let _ = DeleteObject(HGDIOBJ(font.0));
    }
}

#[cfg(test)]
mod tests {
    // clamp_axis logic is small but load-bearing — test it directly via a
    // local re-declaration (the original is private to `imp` to keep
    // implementation detail out of the crate's public surface).
    #[test]
    fn clamp_axis_centres_when_size_exceeds_extent() {
        fn clamp(pos: i32, size: i32, extent: i32) -> i32 {
            if size >= extent { (extent - size) / 2 } else { pos.clamp(0, extent - size) }
        }
        assert_eq!(clamp(0, 2000, 1920), (1920 - 2000) / 2);
        assert_eq!(clamp(500, 1920, 1920), 0);
        assert_eq!(clamp(-50, 100, 1920), 0);
        assert_eq!(clamp(5000, 100, 1920), 1820);
        assert_eq!(clamp(100, 100, 1920), 100);
    }
}
