//! Region-selector overlay.
//!
//! Presents a layered, topmost window covering the entire virtual desktop.
//! User gestures:
//!
//! - **Click-and-drag** → select a rectangular region.
//! - **Click without drag on a window** → capture that window.
//! - **Escape** → cancel.
//! - **Enter** → confirm the current selection (equivalent to releasing the
//!   mouse button on a drag).
//!
//! Transparency is done with `LWA_COLORKEY + LWA_ALPHA`: magenta pixels
//! (inside the selection rect) become fully transparent, everything else
//! shows at ~75% alpha. Good enough for M1 — a per-pixel alpha upgrade via
//! `UpdateLayeredWindow` can come later.

use crate::capture::Rect;
use anyhow::{anyhow, Result};
use image::RgbaImage;

#[derive(Debug, Clone)]
pub enum RegionResult {
    Region(Rect),
    Window(isize), // HWND stored as isize so the enum can cross thread boundaries
    Cancelled,
}

/// Run the region-selector overlay. When `allow_windows` is `false`, window
/// hover highlight is disabled and short clicks do nothing — only a
/// click-drag-release produces a result. This is the mode used by the
/// "Capture & annotate" flow so the interaction is a single consistent
/// gesture.
#[cfg(windows)]
pub fn select(allow_windows: bool) -> Result<RegionResult> {
    imp::run(allow_windows, None)
}

/// Frozen-background variant: paints `background` as the overlay's opaque
/// backdrop, dimmed where the user isn't selecting and full-brightness
/// inside the drag rect. Call this after capturing a fullscreen bitmap so
/// transient UI (tray menus, etc.) is preserved in the final image even
/// after focus moves to the overlay. Always returns `Region(...)` or
/// `Cancelled` — window-pick is disabled in this mode.
#[cfg(windows)]
pub fn select_on_frozen(background: &RgbaImage) -> Result<RegionResult> {
    imp::run(false, Some(background))
}

#[cfg(not(windows))]
pub fn select(_allow_windows: bool) -> Result<RegionResult> {
    Err(anyhow!("region selector is Windows-only"))
}

#[cfg(not(windows))]
pub fn select_on_frozen(_background: &RgbaImage) -> Result<RegionResult> {
    Err(anyhow!("region selector is Windows-only"))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::platform::monitors::virtual_desktop_rect;
    use log::debug;
    use std::cell::RefCell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        COLORREF, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW,
        CreatePen, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, GetDC,
        InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, FONT_CHARSET, FW_SEMIBOLD,
        HBITMAP, HBRUSH, HDC, HGDIOBJ, LOGFONTW, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
        GetMessageW, GetWindowRect, IsWindowVisible, LoadCursorW, PostQuitMessage,
        RegisterClassExW, SetLayeredWindowAttributes, ShowWindow, TranslateMessage,
        UnregisterClassW, HCURSOR, HICON, IDC_CROSS, LWA_ALPHA, LWA_COLORKEY, MSG, SW_SHOW,
        WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
        WM_NCCREATE, WM_PAINT, WM_RBUTTONDOWN, WNDCLASSEXW, WNDCLASS_STYLES, WS_EX_LAYERED,
        SetForegroundWindow, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    const CLASS_NAME: PCWSTR = w!("GrabItRegionOverlay");

    // Color key: magenta pixels become fully transparent via LWA_COLORKEY.
    const KEY_R: u8 = 255;
    const KEY_G: u8 = 0;
    const KEY_B: u8 = 255;
    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    struct State {
        virtual_rect: Rect,
        dragging: bool,
        drag_start: POINT,
        current: POINT,
        hover_window: Option<isize>,
        result: Option<RegionResult>,
        overlay_hwnd: HWND,
        allow_windows: bool,
        /// Pre-captured fullscreen bitmaps for frozen-background mode.
        /// `full` is the unmodified capture; `dimmed` is the same image with
        /// RGB multiplied by ~0.45 so the non-selected area reads as darker.
        /// `None` means live transparent mode (legacy behaviour).
        frozen: Option<FrozenBitmaps>,
    }

    struct FrozenBitmaps {
        full: HBITMAP,
        dimmed: HBITMAP,
    }

    impl Drop for FrozenBitmaps {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.full.0));
                let _ = DeleteObject(HGDIOBJ(self.dimmed.0));
            }
        }
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = RefCell::new(None);
    }

    pub fn run(
        allow_windows: bool,
        frozen_image: Option<&image::RgbaImage>,
    ) -> Result<RegionResult> {
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
                hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or(HCURSOR::default()),
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

            // Note: no WS_EX_NOACTIVATE — we want the overlay to become the
            // foreground window so WM_KEYDOWN reaches us (Esc cancels, Enter
            // commits). Otherwise keystrokes still land on whatever app had
            // focus when the hotkey fired.
            let style_ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
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
                return Err(anyhow!("CreateWindowEx returned null (GetLastError: {:?})", GetLastError()));
            }

            // In live mode we keep the colour-key / alpha trick so the user
            // sees the live desktop through the selection rect. In frozen
            // mode the whole window is opaque (painted from the pre-
            // captured bitmaps) and we set a normal per-window alpha of 255.
            let frozen_bitmaps = if let Some(img) = frozen_image {
                let fb = make_frozen_bitmaps(img)?;
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
                Some(fb)
            } else {
                let _ = SetLayeredWindowAttributes(
                    hwnd,
                    rgb(KEY_R, KEY_G, KEY_B),
                    192,
                    LWA_COLORKEY | LWA_ALPHA,
                );
                None
            };

            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    virtual_rect: vrect,
                    dragging: false,
                    drag_start: POINT { x: 0, y: 0 },
                    current: POINT { x: 0, y: 0 },
                    hover_window: None,
                    result: None,
                    overlay_hwnd: hwnd,
                    allow_windows,
                    frozen: frozen_bitmaps,
                });
            });

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);

            // Modal message loop — blocks the caller until WM_QUIT.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg as *mut MSG, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg as *const MSG);
                DispatchMessageW(&msg as *const MSG);
            }

            let result = STATE.with(|cell| {
                cell.borrow_mut().take().and_then(|s| s.result)
            }).unwrap_or(RegionResult::Cancelled);

            let _ = UnregisterClassW(CLASS_NAME, hinstance);
            debug!("region selector exited: {result:?}");
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
            WM_NCCREATE => {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            WM_MOUSEMOVE => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                with_state(|s| {
                    s.current = POINT { x, y };
                    if !s.dragging {
                        s.hover_window = if s.allow_windows {
                            window_under_virtual_point(
                                s.virtual_rect.x + x,
                                s.virtual_rect.y + y,
                                s.overlay_hwnd,
                            )
                        } else {
                            None
                        };
                    }
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                with_state(|s| {
                    s.dragging = true;
                    s.drag_start = POINT { x, y };
                    s.current = POINT { x, y };
                });
                let _ = SetCapture(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let _ = ReleaseCapture();
                let (result_to_set, should_close) = with_state(|s| {
                    s.current = POINT { x, y };
                    let dx = (s.current.x - s.drag_start.x).abs();
                    let dy = (s.current.y - s.drag_start.y).abs();
                    let res = if s.dragging && dx > 2 && dy > 2 {
                        let left = s.drag_start.x.min(s.current.x);
                        let top = s.drag_start.y.min(s.current.y);
                        let width = dx as u32;
                        let height = dy as u32;
                        Some(RegionResult::Region(Rect {
                            x: s.virtual_rect.x + left,
                            y: s.virtual_rect.y + top,
                            width,
                            height,
                        }))
                    } else if s.allow_windows {
                        s.hover_window.map(RegionResult::Window)
                    } else {
                        None
                    };
                    s.dragging = false;
                    // Region-only mode keeps the overlay up after a bare
                    // click so the user can retry their drag. Full mode
                    // always closes on mouse up (legacy behaviour).
                    let close = res.is_some() || s.allow_windows;
                    (res, close)
                });
                if let Some(res) = result_to_set {
                    with_state(|s| s.result = Some(res));
                }
                if should_close {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u32;
                if key == VK_ESCAPE.0 as u32 {
                    with_state(|s| s.result = Some(RegionResult::Cancelled));
                    let _ = DestroyWindow(hwnd);
                } else if key == VK_RETURN.0 as u32 {
                    let result = with_state(|s| {
                        if s.dragging {
                            let dx = (s.current.x - s.drag_start.x).abs();
                            let dy = (s.current.y - s.drag_start.y).abs();
                            if dx > 2 && dy > 2 {
                                let left = s.drag_start.x.min(s.current.x);
                                let top = s.drag_start.y.min(s.current.y);
                                return Some(RegionResult::Region(Rect {
                                    x: s.virtual_rect.x + left,
                                    y: s.virtual_rect.y + top,
                                    width: dx as u32,
                                    height: dy as u32,
                                }));
                            }
                        }
                        s.hover_window.map(RegionResult::Window)
                    });
                    if let Some(r) = result {
                        with_state(|s| s.result = Some(r));
                        let _ = DestroyWindow(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                with_state(|s| s.result = Some(RegionResult::Cancelled));
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc, hwnd);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_CLOSE => {
                with_state(|s| {
                    if s.result.is_none() {
                        s.result = Some(RegionResult::Cancelled);
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

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
        STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let s = borrow.as_mut().expect("region state must be initialized");
            f(s)
        })
    }

    unsafe fn paint(hdc: windows::Win32::Graphics::Gdi::HDC, hwnd: HWND) {
        use windows::Win32::Graphics::Gdi::Rectangle;

        let (virt_w, virt_h, selection, hover_rect, frozen_handles) = with_state(|s| {
            let selection = if s.dragging {
                let left = s.drag_start.x.min(s.current.x);
                let top = s.drag_start.y.min(s.current.y);
                let right = s.drag_start.x.max(s.current.x);
                let bottom = s.drag_start.y.max(s.current.y);
                Some(RECT { left, top, right, bottom })
            } else {
                None
            };
            let hover = if !s.dragging {
                s.hover_window.and_then(|h| window_client_rect(h, s.virtual_rect))
            } else {
                None
            };
            let fh = s.frozen.as_ref().map(|f| (f.full, f.dimmed));
            (
                s.virtual_rect.width as i32,
                s.virtual_rect.height as i32,
                selection,
                hover,
                fh,
            )
        });
        let _ = hwnd; // not needed past BeginPaint
        let full = RECT { left: 0, top: 0, right: virt_w, bottom: virt_h };

        if let Some((full_bmp, dim_bmp)) = frozen_handles {
            // Frozen mode: draw pre-captured bitmaps. Dimmed covers the whole
            // window; full-brightness draws over the selection rect only.
            // No color-key transparency — the overlay is opaque.
            let mem = CreateCompatibleDC(hdc);
            let old = SelectObject(mem, HGDIOBJ(dim_bmp.0));
            let _ = BitBlt(hdc, 0, 0, virt_w, virt_h, mem, 0, 0, SRCCOPY);
            if let Some(r) = selection {
                let sw = (r.right - r.left).max(0);
                let sh = (r.bottom - r.top).max(0);
                if sw > 0 && sh > 0 {
                    SelectObject(mem, HGDIOBJ(full_bmp.0));
                    let _ = BitBlt(hdc, r.left, r.top, sw, sh, mem, r.left, r.top, SRCCOPY);
                }
            }
            SelectObject(mem, old);
            let _ = DeleteDC(mem);
        } else {
            // Live mode: dark fill everywhere; color-keyed magenta inside
            // the selection becomes transparent so the live desktop shows.
            let dim = CreateSolidBrush(rgb(30, 30, 30));
            FillRect(hdc, &full, dim);
            let _ = DeleteObject(HGDIOBJ(dim.0));
            if let Some(r) = selection {
                let cut = CreateSolidBrush(rgb(KEY_R, KEY_G, KEY_B));
                FillRect(hdc, &r, cut);
                let _ = DeleteObject(HGDIOBJ(cut.0));
            }
        }

        // Selection outline + size label, shared between both modes.
        if let Some(r) = selection {
            let pen = CreatePen(PS_SOLID, 2, rgb(0, 180, 255));
            let old = SelectObject(hdc, HGDIOBJ(pen.0));
            let _ = Rectangle(hdc, r.left, r.top, r.right, r.bottom);
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));

            let w = (r.right - r.left).max(0);
            let h = (r.bottom - r.top).max(0);
            let text = format!("{} x {}", w, h);
            draw_label(hdc, r.right + 6, r.bottom + 6, &text);
        } else if let Some(r) = hover_rect {
            // Window hover highlight is only meaningful in live mode (we
            // don't enable allow_windows when painting a frozen overlay).
            let pen = CreatePen(PS_SOLID, 3, rgb(0, 200, 120));
            let old = SelectObject(hdc, HGDIOBJ(pen.0));
            let _ = Rectangle(hdc, r.left, r.top, r.right, r.bottom);
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));
            draw_label(hdc, r.left + 6, r.top + 6, "Click to capture window");
        }
    }

    /// Build two HBITMAPs from an RgbaImage: `full` is a faithful BGRA copy;
    /// `dimmed` is the same image with RGB channels multiplied by ~0.45 so
    /// the non-selected portion of the overlay reads as visibly darker than
    /// the currently-selected region. Both are returned wrapped in a
    /// `FrozenBitmaps` that takes care of `DeleteObject` on drop.
    unsafe fn make_frozen_bitmaps(img: &image::RgbaImage) -> Result<FrozenBitmaps> {
        let w = img.width() as i32;
        let h = img.height() as i32;
        if w <= 0 || h <= 0 {
            return Err(anyhow!("frozen image has zero size"));
        }
        let screen_dc = GetDC(None);
        if screen_dc.0.is_null() {
            return Err(anyhow!("GetDC(NULL) returned null"));
        }
        let result = (|| -> Result<FrozenBitmaps> {
            let full = alloc_dib_section(screen_dc, w, h, img, false)?;
            let dimmed = alloc_dib_section(screen_dc, w, h, img, true)?;
            Ok(FrozenBitmaps { full, dimmed })
        })();
        ReleaseDC(None, screen_dc);
        result
    }

    unsafe fn alloc_dib_section(
        dc: HDC,
        w: i32,
        h: i32,
        img: &image::RgbaImage,
        dim: bool,
    ) -> Result<HBITMAP> {
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(
            dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .map_err(|e| anyhow!("CreateDIBSection: {e}"))?;
        if hbmp.0.is_null() || bits.is_null() {
            return Err(anyhow!("CreateDIBSection returned null"));
        }
        let src = img.as_raw();
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, src.len());
        // Convert RGBA → BGRA, dimming RGB channels if requested.
        if dim {
            for (s, d) in src.chunks(4).zip(dst.chunks_mut(4)) {
                d[0] = ((s[2] as u16) * 115 / 255) as u8; // B
                d[1] = ((s[1] as u16) * 115 / 255) as u8; // G
                d[2] = ((s[0] as u16) * 115 / 255) as u8; // R
                d[3] = 255;
            }
        } else {
            for (s, d) in src.chunks(4).zip(dst.chunks_mut(4)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = 255;
            }
        }
        Ok(hbmp)
    }

    unsafe fn draw_label(hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, text: &str) {
        let mut lf = LOGFONTW::default();
        lf.lfHeight = -16;
        lf.lfWeight = FW_SEMIBOLD.0 as i32;
        lf.lfCharSet = FONT_CHARSET(0);
        let face_str = format!("{}\0", crate::platform::fonts::FACE_NAME);
        let face: Vec<u16> = face_str.encode_utf16().collect();
        for (i, c) in face.iter().enumerate() {
            if i < lf.lfFaceName.len() { lf.lfFaceName[i] = *c; }
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

    /// Return the HWND of the top-level window under the given virtual-screen
    /// point, excluding our overlay. Uses EnumWindows (Z-ordered) and the
    /// first visible window whose rect contains the point wins.
    unsafe fn window_under_virtual_point(vx: i32, vy: i32, overlay: HWND) -> Option<isize> {
        use windows::Win32::Foundation::BOOL;
        use windows::Win32::Foundation::TRUE;
        use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

        struct Search {
            pt: POINT,
            overlay: isize,
            found: Option<isize>,
        }
        let mut search = Search {
            pt: POINT { x: vx, y: vy },
            overlay: overlay.0 as isize,
            found: None,
        };

        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = &mut *(lparam.0 as *mut Search);
            if hwnd.0 as isize == search.overlay { return TRUE; }
            if !IsWindowVisible(hwnd).as_bool() { return TRUE; }

            // Filter cloaked (UWP minimized to tray, etc.)
            let mut cloaked: u32 = 0;
            let _ = DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            );
            if cloaked != 0 { return TRUE; }

            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() { return TRUE; }
            if search.pt.x >= rect.left && search.pt.x < rect.right
                && search.pt.y >= rect.top && search.pt.y < rect.bottom
            {
                search.found = Some(hwnd.0 as isize);
                return windows::Win32::Foundation::FALSE;
            }
            TRUE
        }

        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut search as *mut _ as isize));
        search.found
    }

    /// Return the window's rect converted to overlay-client coords.
    unsafe fn window_client_rect(hwnd_isize: isize, virtual_rect: Rect) -> Option<RECT> {
        let hwnd = HWND(hwnd_isize as *mut _);
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() { return None; }
        Some(RECT {
            left: r.left - virtual_rect.x,
            top: r.top - virtual_rect.y,
            right: r.right - virtual_rect.x,
            bottom: r.bottom - virtual_rect.y,
        })
    }

}
