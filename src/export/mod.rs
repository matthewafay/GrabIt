//! PNG export + Windows clipboard handoff for M0.
//!
//! The PNG delivered to disk and the clipboard has the cursor layer
//! alpha-composited into the base image (so external viewers see what the
//! user expects). The underlying `.grabit` file — written alongside the
//! PNG — keeps the cursor on its own layer so the editor (M2+) can still
//! move/resize/remove it.

use crate::app::paths::AppPaths;
use crate::capture::{CaptureResult, CursorLayer};
use anyhow::{Context, Result};
use image::{Rgba, RgbaImage};
use log::debug;
use std::path::PathBuf;

/// Save the capture as PNG and write a sibling `.grabit` file preserving the
/// layer structure. Returns the PNG path.
pub fn save_png(result: &CaptureResult, paths: &AppPaths) -> Result<PathBuf> {
    let png_path = paths.default_capture_filename("png");
    let composite = flatten(result);
    composite
        .save_with_format(&png_path, image::ImageFormat::Png)
        .with_context(|| format!("write PNG {}", png_path.display()))?;
    debug!("wrote PNG: {}", png_path.display());

    // `.grabit` companion — best-effort. A failure here should not fail the
    // user-visible capture.
    let grabit_path = png_path.with_extension("grabit");
    if let Err(e) = crate::editor::document::save_from_capture(result, &grabit_path) {
        debug!("skipped .grabit sidecar: {e}");
    }

    Ok(png_path)
}

/// Copy the (flattened) capture to the Windows clipboard as CF_DIB.
pub fn copy_to_clipboard(result: &CaptureResult) -> Result<()> {
    let composite = flatten(result);
    #[cfg(windows)]
    {
        clipboard_impl::put_dib(&composite)
    }
    #[cfg(not(windows))]
    {
        let _ = composite;
        Ok(())
    }
}

/// Alpha-composite the cursor layer onto a copy of the base image.
fn flatten(result: &CaptureResult) -> RgbaImage {
    let mut out = result.base.clone();
    if let Some(cursor) = &result.cursor {
        composite_over(&mut out, cursor);
    }
    out
}

fn composite_over(dst: &mut RgbaImage, cursor: &CursorLayer) {
    let (dw, dh) = dst.dimensions();
    let (cw, ch) = cursor.image.dimensions();
    for cy in 0..ch {
        let dy = cursor.y + cy as i32;
        if dy < 0 || dy >= dh as i32 { continue; }
        for cx in 0..cw {
            let dx = cursor.x + cx as i32;
            if dx < 0 || dx >= dw as i32 { continue; }
            let src = *cursor.image.get_pixel(cx, cy);
            if src.0[3] == 0 { continue; }
            let dst_px = dst.get_pixel_mut(dx as u32, dy as u32);
            *dst_px = blend(*dst_px, src);
        }
    }
}

fn blend(dst: Rgba<u8>, src: Rgba<u8>) -> Rgba<u8> {
    let sa = src.0[3] as u32;
    let da = dst.0[3] as u32;
    let inv = 255 - sa;
    // Straight (non-premultiplied) over-compositing.
    let r = (src.0[0] as u32 * sa + dst.0[0] as u32 * inv) / 255;
    let g = (src.0[1] as u32 * sa + dst.0[1] as u32 * inv) / 255;
    let b = (src.0[2] as u32 * sa + dst.0[2] as u32 * inv) / 255;
    let a = sa + (da * inv) / 255;
    Rgba([r as u8, g as u8, b as u8, a.min(255) as u8])
}

#[cfg(windows)]
mod clipboard_impl {
    use anyhow::{anyhow, Result};
    use image::RgbaImage;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Gdi::{BITMAPINFOHEADER, BI_RGB};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    use windows::Win32::System::Ole::CF_DIB;

    pub fn put_dib(img: &RgbaImage) -> Result<()> {
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return Err(anyhow!("clipboard image is empty"));
        }

        let stride = (w as usize) * 4;
        let header_size = std::mem::size_of::<BITMAPINFOHEADER>();
        let pixel_bytes = stride * (h as usize);
        let total = header_size + pixel_bytes;

        unsafe {
            // GMEM_MOVEABLE memory; ownership transfers to the clipboard on
            // successful SetClipboardData.
            let hmem = GlobalAlloc(GMEM_MOVEABLE, total)
                .map_err(|e| anyhow!("GlobalAlloc: {e}"))?;
            if hmem.0.is_null() {
                return Err(anyhow!("GlobalAlloc returned null"));
            }

            let ptr = GlobalLock(hmem) as *mut u8;
            if ptr.is_null() {
                return Err(anyhow!("GlobalLock failed"));
            }

            // Write BITMAPINFOHEADER (bottom-up DIB: negative height means
            // top-down, most viewers accept either but many legacy apps
            // expect bottom-up from the clipboard).
            let hdr = BITMAPINFOHEADER {
                biSize: header_size as u32,
                biWidth: w as i32,
                biHeight: h as i32, // bottom-up
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: pixel_bytes as u32,
                biXPelsPerMeter: 2835, // ~72dpi; not critical
                biYPelsPerMeter: 2835,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            std::ptr::write(ptr as *mut BITMAPINFOHEADER, hdr);

            // Copy pixels bottom-up and RGBA->BGRA.
            let pixels = img.as_raw();
            let dst_pixels = ptr.add(header_size);
            for y in 0..h as usize {
                let src_row = &pixels[y * stride..(y + 1) * stride];
                let dst_y = (h as usize - 1) - y; // flip
                let dst_row = dst_pixels.add(dst_y * stride);
                for x in 0..w as usize {
                    let s = &src_row[x * 4..x * 4 + 4];
                    let d = dst_row.add(x * 4);
                    *d.add(0) = s[2]; // B
                    *d.add(1) = s[1]; // G
                    *d.add(2) = s[0]; // R
                    *d.add(3) = s[3]; // A
                }
            }

            let _ = GlobalUnlock(hmem);

            if OpenClipboard(None).is_err() {
                return Err(anyhow!("OpenClipboard failed"));
            }
            let close_guard = scopeguard(|| { let _ = CloseClipboard(); });

            if EmptyClipboard().is_err() {
                drop(close_guard);
                return Err(anyhow!("EmptyClipboard failed"));
            }

            // Ownership of hmem transfers on success.
            let as_handle = HANDLE(hmem.0 as *mut _);
            if SetClipboardData(CF_DIB.0 as u32, as_handle).is_err() {
                drop(close_guard);
                return Err(anyhow!("SetClipboardData failed"));
            }

            drop(close_guard);
        }
        Ok(())
    }

    // Minimal drop-guard helper so we close the clipboard even on early
    // returns without pulling in the `scopeguard` crate for one call site.
    fn scopeguard<F: FnMut()>(f: F) -> Guard<F> { Guard(f) }
    struct Guard<F: FnMut()>(F);
    impl<F: FnMut()> Drop for Guard<F> {
        fn drop(&mut self) { (self.0)(); }
    }
}
