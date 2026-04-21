//! GDI `BitBlt` capture path. Used for M0 fullscreen captures and kept as a
//! permanent fallback for pre-1903 Windows 10 and edge cases the WGC path
//! can't handle (e.g. `WS_EX_LAYERED` windows that refuse WGC sessions).
//!
//! Flags:
//! - `SRCCOPY | CAPTUREBLT` is required to capture layered/transparent
//!   windows correctly.
//!
//! Cursor is NOT included here — capture of the cursor as a separate layer
//! happens in `cursor::sample()` up-front, so WGC/GDI can stay cursor-free.

use super::Rect;
use anyhow::{anyhow, Result};
use image::RgbaImage;

#[cfg(windows)]
pub fn capture_virtual_desktop() -> Result<(RgbaImage, Rect)> {
    let v = crate::platform::monitors::virtual_desktop_rect();
    capture_region(v)
}

#[cfg(windows)]
pub fn capture_region(r: Rect) -> Result<(RgbaImage, Rect)> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
    };

    unsafe {
        let x = r.x;
        let y = r.y;
        let w = r.width as i32;
        let h = r.height as i32;
        if w <= 0 || h <= 0 {
            return Err(anyhow!("capture region has zero size"));
        }

        let src_dc = GetDC(HWND::default());
        if src_dc.0.is_null() {
            return Err(anyhow!("GetDC(NULL) returned null"));
        }
        let mem_dc = CreateCompatibleDC(src_dc);
        if mem_dc.0.is_null() {
            ReleaseDC(HWND::default(), src_dc);
            return Err(anyhow!("CreateCompatibleDC failed"));
        }
        let bmp = CreateCompatibleBitmap(src_dc, w, h);
        if bmp.0.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), src_dc);
            return Err(anyhow!("CreateCompatibleBitmap failed"));
        }
        let old = SelectObject(mem_dc, HGDIOBJ(bmp.0));

        let ok = BitBlt(
            mem_dc,
            0, 0, w, h,
            src_dc,
            x, y,
            SRCCOPY | CAPTUREBLT,
        );
        if ok.is_err() {
            SelectObject(mem_dc, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), src_dc);
            return Err(anyhow!("BitBlt failed"));
        }

        // Pull pixels into a top-down BGRA buffer.
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = w;
        info.bmiHeader.biHeight = -h; // negative => top-down
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

        // Always clean up even if GetDIBits failed.
        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), src_dc);

        if scanlines <= 0 {
            return Err(anyhow!("GetDIBits returned {scanlines}"));
        }

        // GDI delivered BGRA; flip to RGBA and force opaque alpha (desktop
        // capture has no meaningful alpha channel).
        for px in buffer.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 0xff;
        }

        let img = RgbaImage::from_raw(w as u32, h as u32, buffer)
            .ok_or_else(|| anyhow!("image buffer size mismatch"))?;

        Ok((img, Rect { x, y, width: w as u32, height: h as u32 }))
    }
}

#[cfg(not(windows))]
pub fn capture_virtual_desktop() -> Result<(RgbaImage, Rect)> {
    Err(anyhow!("GDI capture is Windows-only"))
}

#[cfg(not(windows))]
pub fn capture_region(_r: Rect) -> Result<(RgbaImage, Rect)> {
    Err(anyhow!("GDI capture is Windows-only"))
}
