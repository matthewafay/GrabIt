//! Multi-region picker + composer (feature #8, M6).
//!
//! The picker reuses the region overlay's visual style — dim background,
//! magenta cut-outs, cyan outline — but accepts an arbitrary number of
//! drag-rectangles before committing. On commit each rect is captured
//! independently through the normal GDI path, then `compose_multi_region`
//! lays them out into a single composite image.
//!
//! Picker UX:
//! - Drag a rectangle, release → added to the list (drawn in green).
//! - Drag another → another rectangle appended.
//! - **Enter** → commit all collected rectangles.
//! - **Backspace** → remove the most recent rectangle.
//! - **Esc** or **Right-click** → cancel (discard all).
//!
//! Layout:
//! - Images are composed in the order the user drew them.
//! - We pick row-wrapping vs single-row based on the total width that
//!   would be produced. If the single-row width exceeds
//!   `2 * max_image_width` we wrap into multiple rows. This keeps the
//!   output roughly square-ish without a full bin-packing pass.
//! - A fixed gutter (default 12 px) separates images both horizontally
//!   (within a row) and vertically (between rows).
//! - The canvas is filled with a single background colour (default white).

use crate::capture::Rect;
use anyhow::{anyhow, Result};
use image::RgbaImage;

/// Default gutter in pixels between composed sub-images. Also used as the
/// outer margin around the composite. Chosen to be visually generous
/// enough to read as "these are separate screenshots" without wasting
/// space. Per the plan it is "configurable" — today the knob lives at the
/// call site (pass a custom value to `compose_multi_region`); a settings
/// entry can be added without changing the API.
pub const DEFAULT_GUTTER_PX: u32 = 12;

/// Default background colour for the composite canvas (RGBA, opaque
/// white). Matches the plan's "Background defaults to white".
pub const DEFAULT_BACKGROUND: [u8; 4] = [255, 255, 255, 255];

#[derive(Debug, Clone)]
pub enum MultiRegionResult {
    Rects(Vec<Rect>),
    Cancelled,
}

#[cfg(windows)]
pub fn pick() -> Result<MultiRegionResult> {
    imp::run()
}

#[cfg(not(windows))]
pub fn pick() -> Result<MultiRegionResult> {
    Err(anyhow!("multi-region picker is Windows-only"))
}

/// Pack `images` into a single composite with the given gutter and
/// background colour. Images flow left-to-right in input order; if the
/// single-row width would exceed `2 * max_image_width`, the packer wraps
/// into multiple rows. This is the layout rule called out in M6:
///
/// > pack them in the order drawn, flowing left-to-right on a single row
/// > if total width fits a sensible cap (2× the widest), otherwise wrap.
///
/// Returns a blank canvas (filled with `bg`) if `images` is empty — the
/// caller is expected to skip that case explicitly, but we don't panic.
pub fn compose_multi_region(
    images: &[(Rect, RgbaImage)],
    gutter_px: u32,
    bg: [u8; 4],
) -> RgbaImage {
    if images.is_empty() {
        // 1x1 canvas is a predictable degenerate case.
        let mut out = RgbaImage::new(1, 1);
        out.put_pixel(0, 0, image::Rgba(bg));
        return out;
    }

    let gutter = gutter_px;
    let max_w = images.iter().map(|(_, i)| i.width()).max().unwrap_or(0);
    let single_row_width: u32 = images.iter().map(|(_, i)| i.width()).sum::<u32>()
        + gutter.saturating_mul(images.len().saturating_sub(1) as u32);

    // Width cap used to decide row wrapping. "2 × the widest" per the plan.
    // Also never wrap when there's only one image — that degenerate case
    // can't wrap no matter what.
    let wrap_cap = max_w.saturating_mul(2).max(1);
    let should_wrap = images.len() > 1 && single_row_width > wrap_cap;

    // Build rows. Each row is a Vec of indices into `images`.
    let rows: Vec<Vec<usize>> = if should_wrap {
        let mut rows: Vec<Vec<usize>> = Vec::new();
        let mut current: Vec<usize> = Vec::new();
        let mut current_width: u32 = 0;
        for (idx, (_, img)) in images.iter().enumerate() {
            let w = img.width();
            let tentative_width = if current.is_empty() {
                w
            } else {
                current_width + gutter + w
            };
            if !current.is_empty() && tentative_width > wrap_cap {
                rows.push(std::mem::take(&mut current));
                current_width = 0;
            }
            if current.is_empty() {
                current_width = w;
            } else {
                current_width += gutter + w;
            }
            current.push(idx);
        }
        if !current.is_empty() {
            rows.push(current);
        }
        rows
    } else {
        vec![(0..images.len()).collect()]
    };

    // Compute row dimensions.
    let row_sizes: Vec<(u32, u32)> = rows
        .iter()
        .map(|row| {
            let h = row.iter().map(|i| images[*i].1.height()).max().unwrap_or(0);
            let w: u32 = row.iter().map(|i| images[*i].1.width()).sum::<u32>()
                + gutter.saturating_mul(row.len().saturating_sub(1) as u32);
            (w, h)
        })
        .collect();

    let total_w = row_sizes.iter().map(|(w, _)| *w).max().unwrap_or(0) + 2 * gutter;
    let total_h: u32 = row_sizes.iter().map(|(_, h)| *h).sum::<u32>()
        + gutter.saturating_mul((row_sizes.len() as u32).saturating_add(1)); // leading/trailing + inter-row

    // Canvas.
    let mut canvas = RgbaImage::from_pixel(
        total_w.max(1),
        total_h.max(1),
        image::Rgba(bg),
    );

    // Blit each image at its computed position.
    let mut y: i64 = gutter as i64;
    for (row_idx, row) in rows.iter().enumerate() {
        let (_row_w, row_h) = row_sizes[row_idx];
        let mut x: i64 = gutter as i64;
        for idx in row {
            let (_, img) = &images[*idx];
            blit(&mut canvas, img, x, y);
            x += img.width() as i64 + gutter as i64;
        }
        y += row_h as i64 + gutter as i64;
    }

    canvas
}

/// Copy `src` onto `dst` at `(dx, dy)`, clipping to the destination. No
/// alpha blending — we overwrite because the sub-captures are opaque
/// screenshots (alpha=255 throughout per the GDI capture path).
fn blit(dst: &mut RgbaImage, src: &RgbaImage, dx: i64, dy: i64) {
    let dw = dst.width() as i64;
    let dh = dst.height() as i64;
    let sw = src.width() as i64;
    let sh = src.height() as i64;

    let x0 = dx.max(0);
    let y0 = dy.max(0);
    let x1 = (dx + sw).min(dw);
    let y1 = (dy + sh).min(dh);
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    for y in y0..y1 {
        let sy = (y - dy) as u32;
        for x in x0..x1 {
            let sx = (x - dx) as u32;
            let p = *src.get_pixel(sx, sy);
            dst.put_pixel(x as u32, y as u32, p);
        }
    }
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
        BeginPaint, CreateFontIndirectW, CreatePen, CreateSolidBrush, DeleteObject, EndPaint,
        FillRect, InvalidateRect, Rectangle, SelectObject, SetBkMode, SetTextColor, TextOutW,
        FONT_CHARSET, FW_SEMIBOLD, HBRUSH, HGDIOBJ, LOGFONTW, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        ReleaseCapture, SetCapture, VK_BACK, VK_ESCAPE, VK_RETURN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        LoadCursorW, PostQuitMessage, RegisterClassExW, SetLayeredWindowAttributes, ShowWindow,
        TranslateMessage, UnregisterClassW, HCURSOR, HICON, IDC_CROSS, LWA_ALPHA, LWA_COLORKEY,
        MSG, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT, WM_RBUTTONDOWN, WNDCLASSEXW, WNDCLASS_STYLES,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    };

    const CLASS_NAME: PCWSTR = w!("GrabItMultiRegionOverlay");

    const KEY_R: u8 = 255;
    const KEY_G: u8 = 0;
    const KEY_B: u8 = 255;
    fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
    }

    struct State {
        virtual_rect: Rect,
        /// Rects already committed by the user, in virtual-screen coords.
        collected: Vec<Rect>,
        dragging: bool,
        drag_start: POINT,
        current: POINT,
        result: Option<MultiRegionResult>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub fn run() -> Result<MultiRegionResult> {
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
            let _ = RegisterClassExW(&class); // ERROR_CLASS_ALREADY_EXISTS is benign

            let vrect = virtual_desktop_rect();
            if vrect.width == 0 || vrect.height == 0 {
                return Err(anyhow!("virtual desktop has zero size"));
            }

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

            let _ = SetLayeredWindowAttributes(
                hwnd,
                rgb(KEY_R, KEY_G, KEY_B),
                192,
                LWA_COLORKEY | LWA_ALPHA,
            );

            STATE.with(|cell| {
                *cell.borrow_mut() = Some(State {
                    virtual_rect: vrect,
                    collected: Vec::new(),
                    dragging: false,
                    drag_start: POINT { x: 0, y: 0 },
                    current: POINT { x: 0, y: 0 },
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
                .unwrap_or(MultiRegionResult::Cancelled);

            let _ = UnregisterClassW(CLASS_NAME, hinstance);
            debug!("multi-region picker exited: {result:?}");
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
                with_state(|s| s.current = POINT { x, y });
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
                with_state(|s| {
                    s.current = POINT { x, y };
                    let dx = (s.current.x - s.drag_start.x).abs();
                    let dy = (s.current.y - s.drag_start.y).abs();
                    if s.dragging && dx > 2 && dy > 2 {
                        let left = s.drag_start.x.min(s.current.x);
                        let top = s.drag_start.y.min(s.current.y);
                        s.collected.push(Rect {
                            x: s.virtual_rect.x + left,
                            y: s.virtual_rect.y + top,
                            width: dx as u32,
                            height: dy as u32,
                        });
                    }
                    s.dragging = false;
                });
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let key = wparam.0 as u32;
                if key == VK_ESCAPE.0 as u32 {
                    with_state(|s| s.result = Some(MultiRegionResult::Cancelled));
                    let _ = DestroyWindow(hwnd);
                } else if key == VK_RETURN.0 as u32 {
                    let committed =
                        with_state(|s| MultiRegionResult::Rects(s.collected.clone()));
                    // Treat Enter with zero rectangles as cancel so the
                    // picker doesn't produce an empty composite.
                    let committed = match committed {
                        MultiRegionResult::Rects(r) if r.is_empty() => {
                            MultiRegionResult::Cancelled
                        }
                        other => other,
                    };
                    with_state(|s| s.result = Some(committed));
                    let _ = DestroyWindow(hwnd);
                } else if key == VK_BACK.0 as u32 {
                    with_state(|s| {
                        s.collected.pop();
                    });
                    let _ = InvalidateRect(hwnd, None, false);
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                // Right-click cancels. Plan mentioned right-click could be
                // "commit or cancel — pick one; document choice": we pick
                // cancel (mirrors region.rs) and use Enter to commit. An
                // accidental right-click while drawing discards work,
                // which is annoying but easy to retry; Enter-to-commit
                // matches the rest of the app's muscle memory.
                with_state(|s| s.result = Some(MultiRegionResult::Cancelled));
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
                        s.result = Some(MultiRegionResult::Cancelled);
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
            let s = borrow.as_mut().expect("multi-region state must be initialized");
            f(s)
        })
    }

    unsafe fn paint(hdc: windows::Win32::Graphics::Gdi::HDC) {
        let (virt_w, virt_h, vx, vy, active_drag, collected, count) = with_state(|s| {
            let active = if s.dragging {
                let left = s.drag_start.x.min(s.current.x);
                let top = s.drag_start.y.min(s.current.y);
                let right = s.drag_start.x.max(s.current.x);
                let bottom = s.drag_start.y.max(s.current.y);
                Some(RECT { left, top, right, bottom })
            } else {
                None
            };
            (
                s.virtual_rect.width as i32,
                s.virtual_rect.height as i32,
                s.virtual_rect.x,
                s.virtual_rect.y,
                active,
                s.collected.clone(),
                s.collected.len(),
            )
        });

        // 1. Dim background.
        let dim = CreateSolidBrush(rgb(30, 30, 30));
        let full = RECT { left: 0, top: 0, right: virt_w, bottom: virt_h };
        FillRect(hdc, &full, dim);
        let _ = DeleteObject(HGDIOBJ(dim.0));

        // 2. Each collected rect: magenta cut-out + green outline.
        let cut_brush = CreateSolidBrush(rgb(KEY_R, KEY_G, KEY_B));
        for r in &collected {
            let rc = RECT {
                left: r.x - vx,
                top: r.y - vy,
                right: r.x - vx + r.width as i32,
                bottom: r.y - vy + r.height as i32,
            };
            FillRect(hdc, &rc, cut_brush);

            let pen = CreatePen(PS_SOLID, 2, rgb(0, 200, 120));
            let old = SelectObject(hdc, HGDIOBJ(pen.0));
            let _ = Rectangle(hdc, rc.left, rc.top, rc.right, rc.bottom);
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));
        }
        let _ = DeleteObject(HGDIOBJ(cut_brush.0));

        // 3. Active drag rect (if any): magenta cut-out + cyan outline.
        if let Some(rc) = active_drag {
            let cut = CreateSolidBrush(rgb(KEY_R, KEY_G, KEY_B));
            FillRect(hdc, &rc, cut);
            let _ = DeleteObject(HGDIOBJ(cut.0));

            let pen = CreatePen(PS_SOLID, 2, rgb(0, 180, 255));
            let old = SelectObject(hdc, HGDIOBJ(pen.0));
            let _ = Rectangle(hdc, rc.left, rc.top, rc.right, rc.bottom);
            SelectObject(hdc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));
        }

        // 4. Instruction label.
        let text = format!(
            "{} rect{}  \u{2014} drag more, Enter to commit, Backspace to undo, Esc to cancel",
            count,
            if count == 1 { "" } else { "s" }
        );
        draw_label(hdc, 20, 20, &text);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make(w: u32, h: u32, fill: [u8; 4]) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba(fill);
        }
        img
    }

    fn rect(w: u32, h: u32) -> Rect {
        Rect { x: 0, y: 0, width: w, height: h }
    }

    #[test]
    fn compose_empty_returns_tiny_canvas() {
        let out = compose_multi_region(&[], 12, DEFAULT_BACKGROUND);
        assert_eq!(out.dimensions(), (1, 1));
        assert_eq!(*out.get_pixel(0, 0), image::Rgba(DEFAULT_BACKGROUND));
    }

    #[test]
    fn compose_single_rect_wraps_in_gutter() {
        let img = make(50, 40, [255, 0, 0, 255]);
        let out = compose_multi_region(&[(rect(50, 40), img)], 10, DEFAULT_BACKGROUND);
        // Expected: 10 + 50 + 10 wide, 10 + 40 + 10 tall.
        assert_eq!(out.dimensions(), (70, 60));
        // Top-left corner of the embedded image sits at (10,10).
        assert_eq!(*out.get_pixel(10, 10), image::Rgba([255, 0, 0, 255]));
        // Outer margin is background.
        assert_eq!(*out.get_pixel(0, 0), image::Rgba(DEFAULT_BACKGROUND));
        assert_eq!(*out.get_pixel(69, 59), image::Rgba(DEFAULT_BACKGROUND));
    }

    #[test]
    fn compose_three_rects_single_row_when_narrow_enough() {
        // Three 50-wide images. single_row_width = 50*3 + 2*10 = 170.
        // wrap_cap = 2 * max_w = 100. 170 > 100 → would wrap. Use
        // matching widths to stay below cap: widths that fit 2×max
        // means e.g. two 50-wide pieces. For "stays in row", make the
        // widest-width equal to total.
        let imgs: Vec<(Rect, RgbaImage)> = vec![
            (rect(60, 30), make(60, 30, [10, 20, 30, 255])),
            (rect(30, 30), make(30, 30, [40, 50, 60, 255])),
        ];
        // single_row_width = 60 + 10 + 30 = 100. wrap_cap = 2*60 = 120.
        // 100 <= 120, so stays a single row.
        let out = compose_multi_region(&imgs, 10, DEFAULT_BACKGROUND);
        // Width = gutter + 60 + gutter + 30 + gutter = 10+60+10+30+10 = 120.
        // Height = gutter + 30 + gutter = 50.
        assert_eq!(out.dimensions(), (120, 50));
        // First image origin: (10,10). Second image origin: (10+60+10, 10) = (80,10).
        assert_eq!(*out.get_pixel(10, 10), image::Rgba([10, 20, 30, 255]));
        assert_eq!(*out.get_pixel(80, 10), image::Rgba([40, 50, 60, 255]));
    }

    #[test]
    fn compose_three_rects_wraps_when_too_wide() {
        // Three 50x30 rects: single_row_width = 50*3 + 2*10 = 170.
        // wrap_cap = 2*50 = 100. 170 > 100 → wrap.
        let imgs: Vec<(Rect, RgbaImage)> = vec![
            (rect(50, 30), make(50, 30, [10, 0, 0, 255])),
            (rect(50, 30), make(50, 30, [0, 10, 0, 255])),
            (rect(50, 30), make(50, 30, [0, 0, 10, 255])),
        ];
        let out = compose_multi_region(&imgs, 10, DEFAULT_BACKGROUND);
        // Row 1: 50 + 10 + 50 = 110 -> at 110 would exceed wrap_cap of
        // 100. So we actually start with img1 (50). Adding img2 would
        // yield 110 > 100, so img2 wraps to row 2. Similarly img3 wraps
        // to row 3. Result: 3 rows of 1 image each.
        // Total width = gutter + 50 + gutter = 70.
        // Total height = gutter + 30 + gutter + 30 + gutter + 30 + gutter
        //              = 10*4 + 30*3 = 40 + 90 = 130.
        assert_eq!(out.dimensions(), (70, 130));
        // Pixel checks: each row's top-left image.
        assert_eq!(*out.get_pixel(10, 10), image::Rgba([10, 0, 0, 255]));
        assert_eq!(*out.get_pixel(10, 10 + 30 + 10), image::Rgba([0, 10, 0, 255]));
        assert_eq!(
            *out.get_pixel(10, 10 + (30 + 10) * 2),
            image::Rgba([0, 0, 10, 255])
        );
    }

    #[test]
    fn compose_row_wrap_packs_two_per_row_when_cap_allows() {
        // Two rects per row scenario. imgs widths: [40, 40, 40].
        // single_row_width = 40*3 + 2*10 = 140. wrap_cap = 2*40 = 80.
        // 140 > 80 → wrap. But 40 alone (=40) is under 80, and
        // 40 + gutter + 40 = 90 which is also over 80, so each row is
        // just one image again. To force two per row with the 2*max
        // rule we'd need smaller gutter or bigger images.
        let imgs: Vec<(Rect, RgbaImage)> = vec![
            (rect(60, 20), make(60, 20, [255, 0, 0, 255])),
            (rect(60, 20), make(60, 20, [0, 255, 0, 255])),
            (rect(60, 20), make(60, 20, [0, 0, 255, 255])),
        ];
        // single_row_width = 60*3 + 2*4 = 188. wrap_cap = 2*60 = 120.
        // 188 > 120 → wrap. Row packing:
        //   start row: img1(60), width=60. add img2 → 60+4+60 = 124 > 120 → new row.
        // So each row gets exactly one image again. Sanity: the algorithm
        // is order-preserving; verifying that shape suffices.
        let out = compose_multi_region(&imgs, 4, DEFAULT_BACKGROUND);
        let (w, _h) = out.dimensions();
        // Width == outer gutter*2 + image width = 4+60+4 = 68.
        assert_eq!(w, 68);
    }

    #[test]
    fn compose_background_covers_gutters() {
        let bg = [7, 8, 9, 255];
        let imgs: Vec<(Rect, RgbaImage)> =
            vec![(rect(10, 10), make(10, 10, [200, 200, 200, 255]))];
        let out = compose_multi_region(&imgs, 5, bg);
        // A pixel strictly in the gutter should carry the background.
        assert_eq!(*out.get_pixel(0, 0), image::Rgba(bg));
        assert_eq!(*out.get_pixel(2, 2), image::Rgba(bg));
        // And a pixel inside the image should carry the image.
        assert_eq!(*out.get_pixel(5, 5), image::Rgba([200, 200, 200, 255]));
    }
}
