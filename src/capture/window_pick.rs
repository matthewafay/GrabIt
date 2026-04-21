//! Window capture via `PrintWindow(PW_RENDERFULLCONTENT)`.
//!
//! `PrintWindow` asks the DWM to render a specific top-level window into a
//! bitmap. With the `PW_RENDERFULLCONTENT` flag (Win10 1803+) it captures
//! the window as the user sees it, including hardware-accelerated surfaces.
//! This is a pragmatic choice for M1 — WGC (Windows.Graphics.Capture) would
//! be a bit more robust for protected-content and occluded scenarios, but
//! PrintWindow covers the common cases with an order of magnitude less
//! COM plumbing.

use super::Rect;
use anyhow::{anyhow, Result};
use image::RgbaImage;

// Flag constant — PW_RENDERFULLCONTENT is not re-exported by windows 0.58.
#[cfg(windows)]
const PW_RENDERFULLCONTENT: u32 = 0x00000002;

#[cfg(windows)]
pub fn capture_window(hwnd_isize: isize) -> Result<(RgbaImage, Rect)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
        ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::Storage::Xps::PrintWindow;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let hwnd = HWND(hwnd_isize as *mut _);
    if hwnd.0.is_null() {
        return Err(anyhow!("null HWND"));
    }

    unsafe {
        // Use the full window rect (frame + content). Users most often want
        // the whole window as shown on screen.
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return Err(anyhow!("GetWindowRect failed"));
        }
        let w = (wr.right - wr.left).max(1);
        let h = (wr.bottom - wr.top).max(1);

        let screen_dc = GetDC(HWND::default());
        if screen_dc.0.is_null() {
            return Err(anyhow!("GetDC(NULL) returned null"));
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        let bmp = CreateCompatibleBitmap(screen_dc, w, h);
        let old = SelectObject(mem_dc, HGDIOBJ(bmp.0));

        let ok = PrintWindow(
            hwnd,
            mem_dc,
            windows::Win32::Storage::Xps::PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
        );
        if !ok.as_bool() {
            SelectObject(mem_dc, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
            return Err(anyhow!("PrintWindow returned 0"));
        }

        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // top-down
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;

        let stride = (w as usize) * 4;
        let mut buffer = vec![0u8; stride * (h as usize)];
        let scanlines = GetDIBits(
            mem_dc,
            bmp,
            0,
            h as u32,
            Some(buffer.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);

        if scanlines <= 0 {
            return Err(anyhow!("GetDIBits returned {scanlines}"));
        }

        for px in buffer.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 0xff;
        }

        let img = RgbaImage::from_raw(w as u32, h as u32, buffer)
            .ok_or_else(|| anyhow!("image buffer size mismatch"))?;

        Ok((
            img,
            Rect { x: wr.left, y: wr.top, width: w as u32, height: h as u32 },
        ))
    }
}

#[cfg(not(windows))]
pub fn capture_window(_hwnd_isize: isize) -> Result<(RgbaImage, Rect)> {
    Err(anyhow!("window capture is Windows-only"))
}
