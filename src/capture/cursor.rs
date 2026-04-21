//! Cursor-as-layer sampler.
//!
//! The cursor is captured separately from the base image so feature #2
//! ("Edit cursor" — remove/resize/move) can manipulate it as an annotation
//! layer without damaging the underlying pixels. Flow:
//!
//! 1. `GetCursorInfo` — position + `CURSOR_SHOWING` flag + HCURSOR.
//! 2. `CopyIcon` — duplicate the cursor so its lifetime is ours, not the OS's.
//! 3. `GetIconInfo` — hotspot, AND-mask bitmap, XOR color bitmap.
//! 4. `DrawIconEx(DI_NORMAL)` onto a transparent compatible DC, read back
//!    as RGBA. `DI_NORMAL` handles color, alpha, and mask combination in
//!    one call so we don't have to replicate the cursor's blend logic.

use super::CursorLayer;
use anyhow::{anyhow, Result};
use image::RgbaImage;

#[cfg(windows)]
pub fn sample() -> Result<Option<CursorLayer>> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLACKNESS,
        DIB_RGB_COLORS, HGDIOBJ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CopyIcon, DestroyIcon, DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING,
        DI_NORMAL, ICONINFO,
    };

    unsafe {
        let mut ci = CURSORINFO::default();
        ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut ci).is_err() {
            return Ok(None);
        }
        if ci.flags != CURSOR_SHOWING {
            return Ok(None);
        }
        if ci.hCursor.0.is_null() {
            return Ok(None);
        }

        // Duplicate so the OS can't free it out from under us.
        let hicon = CopyIcon(ci.hCursor).map_err(|e| anyhow!("CopyIcon: {e}"))?;

        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            let _ = DestroyIcon(hicon);
            return Ok(None);
        }

        // ICONINFO bitmaps are owned by the caller after GetIconInfo; clean
        // them up whatever path we take below.
        struct IconInfoGuard(ICONINFO);
        impl Drop for IconInfoGuard {
            fn drop(&mut self) {
                unsafe {
                    if !self.0.hbmMask.0.is_null() {
                        let _ = DeleteObject(HGDIOBJ(self.0.hbmMask.0));
                    }
                    if !self.0.hbmColor.0.is_null() {
                        let _ = DeleteObject(HGDIOBJ(self.0.hbmColor.0));
                    }
                }
            }
        }
        let _info_guard = IconInfoGuard(info);

        // Cursor bitmaps are typically 32x32; some are 48x48 or larger. We
        // pick a bounding box from the mask bitmap. For simplicity we
        // allocate a fixed 64x64 RGBA buffer and let DrawIconEx clip.
        const SIZE: i32 = 64;

        let screen_dc = GetDC(HWND::default());
        if screen_dc.0.is_null() {
            let _ = DestroyIcon(hicon);
            return Ok(None);
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        let bmp = CreateCompatibleBitmap(screen_dc, SIZE, SIZE);
        let old = SelectObject(mem_dc, HGDIOBJ(bmp.0));

        // Clear to opaque black; DrawIconEx with DI_NORMAL will overwrite
        // with the cursor's own alpha-composited pixels.
        let _ = BitBlt(mem_dc, 0, 0, SIZE, SIZE, mem_dc, 0, 0, BLACKNESS);

        let drew = DrawIconEx(mem_dc, 0, 0, hicon, SIZE, SIZE, 0, None, DI_NORMAL).is_ok();

        let mut layer = None;
        if drew {
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = SIZE;
            bi.bmiHeader.biHeight = -SIZE;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB.0;

            let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
            let scanlines = GetDIBits(
                mem_dc,
                bmp,
                0,
                SIZE as u32,
                Some(buf.as_mut_ptr() as *mut _),
                &mut bi,
                DIB_RGB_COLORS,
            );
            if scanlines > 0 {
                for px in buf.chunks_exact_mut(4) {
                    px.swap(0, 2);
                    // DrawIconEx into an opaque black DC loses real alpha; we
                    // approximate by treating pure-black pixels as
                    // transparent. A proper solution (render into a 32bpp DIB
                    // with DI_NORMAL + PBGRA) lands in M1.
                    if px[0] == 0 && px[1] == 0 && px[2] == 0 {
                        px[3] = 0;
                    } else {
                        px[3] = 0xff;
                    }
                }
                let img = RgbaImage::from_raw(SIZE as u32, SIZE as u32, buf);
                layer = img.map(|image| CursorLayer {
                    image,
                    x: ci.ptScreenPos.x - info.xHotspot as i32,
                    y: ci.ptScreenPos.y - info.yHotspot as i32,
                });
            }
        }

        SelectObject(mem_dc, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);
        let _ = DestroyIcon(hicon);

        Ok(layer)
    }
}

#[cfg(not(windows))]
pub fn sample() -> Result<Option<CursorLayer>> {
    Ok(None)
}
