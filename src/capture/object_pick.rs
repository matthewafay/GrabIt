//! Object / menu picker overlay (feature #5, M6).
//!
//! Hover over any UI element on screen and capture it as a rectangle. The
//! picker uses `IUIAutomation::ElementFromPoint` to hit-test the element
//! beneath the cursor, then reads its `CurrentBoundingRectangle` and draws
//! an outline. A `SetWinEventHook` listening for `EVENT_SYSTEM_MENUPOPUP*`
//! records the most recent menu HWND so the picker can still target popup
//! menus (which auto-dismiss on most input) while they are visible.
//!
//! UX:
//! - Move the mouse → the element under the cursor highlights.
//! - Press **F3** → capture the highlighted element's bounding rect.
//! - Press **Esc** → cancel.
//!
//! Design notes:
//! - The overlay is `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`
//!   so mouse events pass straight through to the real window underneath.
//!   UIA runs against the real hit — not our overlay. The trade-off is that
//!   we can't use `WM_LBUTTONUP` to commit, hence F3 via a low-level
//!   keyboard hook.
//! - `ElementFromPoint` is expensive (cross-process COM + remote tree
//!   traversal). We throttle with a `WM_TIMER` running at ~15 Hz; mouse
//!   movement alone does not trigger a lookup.
//! - COM is initialised STA and uninitialised symmetrically — matches the
//!   convention the rest of the app uses (the main thread already runs STA
//!   for WGC/tray, but we re-init defensively so this module is callable
//!   from anywhere).
//! - `SetWinEventHook` is installed on the picker thread before the loop
//!   and unhooked on exit. The `WINEVENT_OUTOFCONTEXT` flag means the
//!   callback runs on the same thread that pumped the event (no DLL
//!   injection required), so the thread-local state it touches is safe.

use crate::capture::Rect;
use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub enum ObjectPickResult {
    /// The resolved bounding rectangle in virtual-screen coordinates.
    Region(Rect),
    Cancelled,
}

#[cfg(windows)]
pub fn pick() -> Result<ObjectPickResult> {
    imp::run()
}

#[cfg(not(windows))]
pub fn pick() -> Result<ObjectPickResult> {
    Err(anyhow!("object picker is Windows-only"))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use log::debug;
    use std::cell::RefCell;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        COLORREF, GetLastError, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateFontIndirectW, CreatePen, CreateSolidBrush, DeleteObject, EndPaint,
        FillRect, InvalidateRect, Rectangle, SelectObject, SetBkMode, SetTextColor, TextOutW,
        FONT_CHARSET, FW_SEMIBOLD, HBRUSH, HGDIOBJ, LOGFONTW, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, HWINEVENTHOOK, IUIAutomation, IUIAutomationElement, SetWinEventHook,
        UnhookWinEvent,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_ESCAPE, VK_F3,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
        GetMessageW, KillTimer, LoadCursorW, PostQuitMessage, RegisterClassExW,
        SetLayeredWindowAttributes, SetTimer, ShowWindow, TranslateMessage, UnregisterClassW,
        EVENT_SYSTEM_MENUPOPUPEND, EVENT_SYSTEM_MENUPOPUPSTART, HCURSOR, HICON, IDC_CROSS,
        LWA_ALPHA, LWA_COLORKEY, MSG, SW_SHOWNOACTIVATE, WINEVENT_OUTOFCONTEXT,
        WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_DESTROY, WM_NCCREATE, WM_PAINT, WM_TIMER,
        WNDCLASSEXW, WNDCLASS_STYLES, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
    };

    const CLASS_NAME: PCWSTR = w!("GrabItObjectOverlay");

    /// Polling cadence for UIA `ElementFromPoint`. A 66 ms period gives
    /// ~15 lookups per second — responsive but nowhere near the
    /// ~1000 Hz that a naïve `WM_MOUSEMOVE` handler would hit. UIA calls
    /// are cross-process COM and routinely take tens of ms in complex
    /// apps (browsers, Office); any faster and we'd back up the queue.
    const POLL_INTERVAL_MS: u32 = 66;
    const POLL_TIMER_ID: usize = 0x9001;

    // Color key: magenta = fully transparent via LWA_COLORKEY.
    const KEY_R: u8 = 255;
    const KEY_G: u8 = 0;
    const KEY_B: u8 = 255;
    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    struct State {
        virtual_rect: Rect,
        /// UIA instance — kept alive for the lifetime of the picker.
        uia: IUIAutomation,
        /// Most recent bounding rect resolved by the picker (in virtual-
        /// screen coords). `None` until the first successful hit-test.
        last_rect: Option<Rect>,
        /// Short descriptive label for the last element (name/class). Shown
        /// next to the outline so the user can confirm they're aiming at
        /// the right thing.
        last_label: String,
        /// HWND of the most recently-opened popup menu, or null. Tracked
        /// via the WinEvent hook so the picker can surface "menu active"
        /// in the UI and, if needed, steer UIA toward that subtree.
        active_menu_hwnd: isize,
        result: Option<ObjectPickResult>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub fn run() -> Result<ObjectPickResult> {
        unsafe {
            // Initialise COM as STA on this thread. The main thread is
            // already APARTMENTTHREADED (set at startup for WGC), so
            // CoInitializeEx will return RPC_E_CHANGED_MODE / S_FALSE
            // rather than S_OK — either is fine. Pair with CoUninitialize
            // on exit.
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let uninit_com = hr.is_ok() || hr.0 == 1; // S_FALSE means already init'd
            if !uninit_com {
                debug!("CoInitializeEx returned {hr:?}; treating as already initialised");
            }

            let uia: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| anyhow!("CoCreateInstance(CUIAutomation): {e}"))?;

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
            let _ = RegisterClassExW(&class); // ERROR_CLASS_ALREADY_EXISTS is benign

            let vrect = crate::platform::monitors::virtual_desktop_rect();
            if vrect.width == 0 || vrect.height == 0 {
                let _ = UnregisterClassW(CLASS_NAME, hinstance);
                if uninit_com {
                    CoUninitialize();
                }
                return Err(anyhow!("virtual desktop has zero size"));
            }

            // WS_EX_TRANSPARENT makes the overlay click-through — mouse
            // events fall to the real window so UIA sees the real hit.
            let style_ex = WS_EX_LAYERED
                | WS_EX_TOPMOST
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | WS_EX_TRANSPARENT;
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
                let _ = UnregisterClassW(CLASS_NAME, hinstance);
                if uninit_com {
                    CoUninitialize();
                }
                return Err(anyhow!(
                    "CreateWindowEx returned null (GetLastError: {:?})",
                    GetLastError()
                ));
            }

            // Magenta = transparent; everything else at 90% alpha so the
            // outline stays crisp against busy backgrounds.
            let _ = SetLayeredWindowAttributes(
                hwnd,
                rgb(KEY_R, KEY_G, KEY_B),
                230,
                LWA_COLORKEY | LWA_ALPHA,
            );

            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    virtual_rect: vrect,
                    uia,
                    last_rect: None,
                    last_label: String::new(),
                    active_menu_hwnd: 0,
                    result: None,
                });
            });

            // Install the menu-popup WinEvent hook. WINEVENT_OUTOFCONTEXT
            // dispatches the callback on this thread via the message queue,
            // so no DLL injection / cross-process state is involved.
            let hook = SetWinEventHook(
                EVENT_SYSTEM_MENUPOPUPSTART,
                EVENT_SYSTEM_MENUPOPUPEND,
                HMODULE(std::ptr::null_mut()),
                Some(menu_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetTimer(hwnd, POLL_TIMER_ID, POLL_INTERVAL_MS, None);

            // Modal message loop. Commit/cancel are driven by the global
            // key state polled on each timer tick (our click-through
            // overlay cannot receive focus, so WM_KEYDOWN would never fire).
            let mut msg = MSG::default();
            while GetMessageW(&mut msg as *mut MSG, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg as *const MSG);
                DispatchMessageW(&msg as *const MSG);
            }

            // Cleanup — always runs, even on error paths below.
            if !hook.0.is_null() {
                let _ = UnhookWinEvent(hook);
            }
            let _ = UnregisterClassW(CLASS_NAME, hinstance);

            let result = STATE
                .with(|cell| cell.borrow_mut().take().and_then(|s| s.result))
                .unwrap_or(ObjectPickResult::Cancelled);

            if uninit_com {
                CoUninitialize();
            }

            debug!("object picker exited: {result:?}");
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
            WM_TIMER if wparam.0 == POLL_TIMER_ID => {
                on_poll_tick(hwnd);
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
                with_state_opt(|s| {
                    if s.result.is_none() {
                        s.result = Some(ObjectPickResult::Cancelled);
                    }
                });
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(hwnd, POLL_TIMER_ID);
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Per-tick logic: check commit/cancel keys, hit-test the cursor
    /// through UIA, and invalidate if the highlight changed.
    unsafe fn on_poll_tick(hwnd: HWND) {
        // Commit: F3. Cancel: Escape. GetAsyncKeyState returns a SHORT —
        // bit 0x8000 = currently down, bit 0x0001 = pressed since last call.
        let f3 = GetAsyncKeyState(VK_F3.0 as i32) as u16;
        let esc = GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16;
        if (esc & 0x8000) != 0 {
            with_state_opt(|s| s.result = Some(ObjectPickResult::Cancelled));
            let _ = DestroyWindow(hwnd);
            return;
        }
        if (f3 & 0x8000) != 0 {
            let committed = with_state(|s| s.last_rect);
            if let Some(rect) = committed {
                with_state(|s| s.result = Some(ObjectPickResult::Region(rect)));
                let _ = DestroyWindow(hwnd);
                return;
            }
            // F3 pressed but no element resolved yet — ignore, user may be
            // still moving the cursor.
        }

        // Resolve the element under the cursor.
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() {
            return;
        }

        let (uia, vrect) = with_state(|s| (s.uia.clone(), s.virtual_rect));

        let element = match uia.ElementFromPoint(pt) {
            Ok(e) => e,
            Err(e) => {
                // ElementFromPoint can legitimately fail mid-menu-close
                // or over protected surfaces. Log at debug, keep polling.
                debug!("ElementFromPoint({},{}): {e}", pt.x, pt.y);
                return;
            }
        };

        let bounds = match element.CurrentBoundingRectangle() {
            Ok(r) if r.right > r.left && r.bottom > r.top => r,
            Ok(_) => return,
            Err(_) => return,
        };

        let new_rect = Rect {
            x: bounds.left,
            y: bounds.top,
            width: (bounds.right - bounds.left).max(1) as u32,
            height: (bounds.bottom - bounds.top).max(1) as u32,
        };

        // Clip to the virtual desktop so overflow elements (fullscreen
        // windows that extend to -1,-1) don't produce absurd capture rects.
        let clipped = clip_to_virtual(new_rect, vrect);

        let label = element_label(&element);

        let changed = with_state(|s| {
            let changed = s.last_rect != Some(clipped) || s.last_label != label;
            s.last_rect = Some(clipped);
            s.last_label = label;
            changed
        });

        if changed {
            let _ = InvalidateRect(hwnd, None, true);
        }
    }

    fn clip_to_virtual(r: Rect, v: Rect) -> Rect {
        let x0 = r.x.max(v.x);
        let y0 = r.y.max(v.y);
        let x1 = (r.x + r.width as i32).min(v.x + v.width as i32);
        let y1 = (r.y + r.height as i32).min(v.y + v.height as i32);
        Rect {
            x: x0,
            y: y0,
            width: (x1 - x0).max(1) as u32,
            height: (y1 - y0).max(1) as u32,
        }
    }

    unsafe fn element_label(e: &IUIAutomationElement) -> String {
        let name = e.CurrentName().map(|b| b.to_string()).unwrap_or_default();
        let class = e.CurrentClassName().map(|b| b.to_string()).unwrap_or_default();
        match (name.is_empty(), class.is_empty()) {
            (false, false) => format!("{name}  [{class}]"),
            (false, true) => name,
            (true, false) => class,
            (true, true) => String::from("(unnamed element)"),
        }
    }

    /// WinEvent callback for `EVENT_SYSTEM_MENUPOPUPSTART` /
    /// `EVENT_SYSTEM_MENUPOPUPEND`. Because we registered with
    /// `WINEVENT_OUTOFCONTEXT` this runs on the picker thread via the
    /// message queue, so touching the thread-local state is safe.
    unsafe extern "system" fn menu_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        _idobject: i32,
        _idchild: i32,
        _ideventthread: u32,
        _dwmseventtime: u32,
    ) {
        with_state_opt(|s| match event {
            EVENT_SYSTEM_MENUPOPUPSTART => {
                s.active_menu_hwnd = hwnd.0 as isize;
                debug!("menu popup started: hwnd={:?}", hwnd.0);
            }
            EVENT_SYSTEM_MENUPOPUPEND => {
                if s.active_menu_hwnd == hwnd.0 as isize {
                    s.active_menu_hwnd = 0;
                }
                debug!("menu popup ended: hwnd={:?}", hwnd.0);
            }
            _ => {}
        });
    }

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
        STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let s = borrow.as_mut().expect("object-pick state must be initialized");
            f(s)
        })
    }

    /// Variant that tolerates missing state (e.g. WinEvent callback firing
    /// after we've torn the cell down during shutdown).
    fn with_state_opt(f: impl FnOnce(&mut State)) {
        STATE.with(|cell| {
            let mut borrow = cell.borrow_mut();
            if let Some(s) = borrow.as_mut() {
                f(s);
            }
        });
    }

    unsafe fn paint(hdc: windows::Win32::Graphics::Gdi::HDC) {
        let (virt_w, virt_h, vx, vy, maybe_rect, label, menu_active) = with_state(|s| {
            (
                s.virtual_rect.width as i32,
                s.virtual_rect.height as i32,
                s.virtual_rect.x,
                s.virtual_rect.y,
                s.last_rect,
                s.last_label.clone(),
                s.active_menu_hwnd != 0,
            )
        });

        // Fill the entire overlay with magenta so everything is transparent
        // except the outline we draw on top. This lets the real UI beneath
        // show through exactly as the user is interacting with it.
        let clear = CreateSolidBrush(rgb(KEY_R, KEY_G, KEY_B));
        let full = RECT { left: 0, top: 0, right: virt_w, bottom: virt_h };
        FillRect(hdc, &full, clear);
        let _ = DeleteObject(HGDIOBJ(clear.0));

        if let Some(rect) = maybe_rect {
            // Convert from virtual-screen coords to overlay-client coords.
            let l = rect.x - vx;
            let t = rect.y - vy;
            let r = l + rect.width as i32;
            let b = t + rect.height as i32;

            let pen_color = if menu_active {
                rgb(255, 180, 0) // amber = menu pinned
            } else {
                rgb(0, 200, 120) // green = normal hover
            };
            let pen = CreatePen(PS_SOLID, 3, pen_color);
            let old = SelectObject(hdc, HGDIOBJ(pen.0));
            // We need the interior magenta to remain transparent — so we
            // can't use GDI's stock solid brush. Select a NULL_BRUSH by
            // creating a magenta one that blends into the color key (the
            // Rectangle call uses the currently-selected brush for fill).
            let fill = CreateSolidBrush(rgb(KEY_R, KEY_G, KEY_B));
            let old_brush = SelectObject(hdc, HGDIOBJ(fill.0));
            let _ = Rectangle(hdc, l, t, r, b);
            SelectObject(hdc, old_brush);
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));
            let _ = DeleteObject(HGDIOBJ(fill.0));

            // Info label above the rect where possible.
            let header = if menu_active {
                format!("{label}  \u{2014} menu pinned  \u{2014} F3 capture / Esc cancel")
            } else {
                format!("{label}  \u{2014} F3 capture / Esc cancel")
            };
            let label_y = if t > 28 { t - 22 } else { b + 6 };
            draw_label(hdc, l + 4, label_y, &header);
        } else {
            draw_label(
                hdc,
                20,
                20,
                "Hover any UI element  \u{2014} F3 to capture, Esc to cancel",
            );
        }
    }

    unsafe fn draw_label(hdc: windows::Win32::Graphics::Gdi::HDC, x: i32, y: i32, text: &str) {
        let mut lf = LOGFONTW {
            lfHeight: -16,
            lfWeight: FW_SEMIBOLD.0 as i32,
            lfCharSet: FONT_CHARSET(0),
            ..Default::default()
        };
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
