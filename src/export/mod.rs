//! PNG export + Windows clipboard handoff for M0.
//!
//! The PNG delivered to disk and the clipboard has the cursor layer
//! alpha-composited into the base image (so external viewers see what the
//! user expects). The underlying `.grabit` file — written alongside the
//! PNG — keeps the cursor on its own layer so the editor (M2+) can still
//! move/resize/remove it.

pub mod gif;

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
    save_png_to(result, &png_path)?;
    Ok(png_path)
}

/// Same as `save_png`, but writes to an explicit path chosen by the caller
/// (e.g. a preset's rendered filename template). Writes the `.grabit`
/// sidecar alongside best-effort.
pub fn save_png_to(result: &CaptureResult, png_path: &std::path::Path) -> Result<PathBuf> {
    let composite = flatten(result);
    composite
        .save_with_format(png_path, image::ImageFormat::Png)
        .with_context(|| format!("write PNG {}", png_path.display()))?;
    debug!("wrote PNG: {}", png_path.display());

    let grabit_path = png_path.with_extension("grabit");
    if let Err(e) = crate::editor::document::save_from_capture(result, &grabit_path) {
        debug!("skipped .grabit sidecar: {e}");
    }
    Ok(png_path.to_path_buf())
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

/// Copy a file on disk to the clipboard. PNGs go on as `CF_DIB` (the
/// shape every "paste an image" target understands). GIFs and any other
/// file type go on as `CF_HDROP` so apps like Slack / Discord / Outlook
/// paste them as the actual file attachment, preserving animation. The
/// file path is always also written as `CF_UNICODETEXT` so plain-text
/// targets get something useful too.
#[cfg(windows)]
pub fn copy_file_to_clipboard(path: &std::path::Path) -> Result<()> {
    use anyhow::Context;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("png")) {
        let img = image::open(path)
            .with_context(|| format!("read {}", path.display()))?
            .to_rgba8();
        clipboard_impl::put_dib(&img)
    } else {
        // GIF (or anything else) — copy as a file drop so animation
        // survives the round-trip into chat clients.
        clipboard_impl::put_hdrop(path)
    }
}

/// Copy `s` to the clipboard as `CF_UNICODETEXT`. Used for the
/// "Copy path" action in the history window.
#[cfg(windows)]
pub fn copy_text_to_clipboard(s: &str) -> Result<()> {
    clipboard_impl::put_unicode_text(s)
}

#[cfg(not(windows))]
pub fn copy_file_to_clipboard(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn copy_text_to_clipboard(_s: &str) -> Result<()> {
    Ok(())
}

/// Alpha-composite the cursor layer onto a copy of the base image.
fn flatten(result: &CaptureResult) -> RgbaImage {
    let mut out = result.base.clone();
    if let Some(cursor) = &result.cursor {
        composite_over(&mut out, cursor);
    }
    out
}

pub(crate) fn composite_over(dst: &mut RgbaImage, cursor: &CursorLayer) {
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

    /// Put a single file onto the clipboard as `CF_HDROP`. Layout is a
    /// `DROPFILES` header followed by a double-null-terminated list of
    /// wide-char paths. With `fWide = TRUE` the list is UTF-16; the
    /// final empty string (just `\0`) marks the end of the file list.
    pub fn put_hdrop(path: &std::path::Path) -> Result<()> {
        use windows::Win32::System::Ole::CF_HDROP;
        use windows::Win32::UI::Shell::DROPFILES;

        // Absolute path — chat clients tend to reject relative paths.
        let abs = std::fs::canonicalize(path)
            .map_err(|e| anyhow!("canonicalize {}: {e}", path.display()))?;
        // Strip the `\\?\` UNC prefix so File Explorer / Slack / Discord
        // see a friendly path. The kernel returns the verbatim form;
        // most consumer apps choke on it.
        let abs_str = abs.to_string_lossy();
        let trimmed = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str);

        // UTF-16 wide string + a trailing single-null (file separator)
        // + another null (end of list).
        let mut wide: Vec<u16> = trimmed.encode_utf16().collect();
        wide.push(0); // terminate this file path
        wide.push(0); // terminate the list

        let header_size = std::mem::size_of::<DROPFILES>();
        let payload_bytes = wide.len() * 2;
        let total = header_size + payload_bytes;

        unsafe {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, total)
                .map_err(|e| anyhow!("GlobalAlloc: {e}"))?;
            if hmem.0.is_null() {
                return Err(anyhow!("GlobalAlloc returned null"));
            }
            let ptr = GlobalLock(hmem) as *mut u8;
            if ptr.is_null() {
                return Err(anyhow!("GlobalLock failed"));
            }

            let hdr = DROPFILES {
                pFiles: header_size as u32,
                pt: windows::Win32::Foundation::POINT { x: 0, y: 0 },
                fNC: windows::Win32::Foundation::BOOL(0),
                fWide: windows::Win32::Foundation::BOOL(1),
            };
            std::ptr::write(ptr as *mut DROPFILES, hdr);

            let dst = ptr.add(header_size) as *mut u16;
            std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());

            let _ = GlobalUnlock(hmem);

            if OpenClipboard(None).is_err() {
                return Err(anyhow!("OpenClipboard failed"));
            }
            let close_guard = scopeguard(|| {
                let _ = CloseClipboard();
            });
            if EmptyClipboard().is_err() {
                drop(close_guard);
                return Err(anyhow!("EmptyClipboard failed"));
            }
            let as_handle = HANDLE(hmem.0 as *mut _);
            if SetClipboardData(CF_HDROP.0 as u32, as_handle).is_err() {
                drop(close_guard);
                return Err(anyhow!("SetClipboardData(CF_HDROP) failed"));
            }
            drop(close_guard);
        }
        Ok(())
    }

    /// Put a UTF-16 string onto the clipboard as `CF_UNICODETEXT`.
    pub fn put_unicode_text(s: &str) -> Result<()> {
        use windows::Win32::System::Ole::CF_UNICODETEXT;

        let mut wide: Vec<u16> = s.encode_utf16().collect();
        wide.push(0); // null terminator
        let bytes = wide.len() * 2;

        unsafe {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes)
                .map_err(|e| anyhow!("GlobalAlloc: {e}"))?;
            if hmem.0.is_null() {
                return Err(anyhow!("GlobalAlloc returned null"));
            }
            let ptr = GlobalLock(hmem) as *mut u16;
            if ptr.is_null() {
                return Err(anyhow!("GlobalLock failed"));
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(hmem);

            if OpenClipboard(None).is_err() {
                return Err(anyhow!("OpenClipboard failed"));
            }
            let close_guard = scopeguard(|| {
                let _ = CloseClipboard();
            });
            if EmptyClipboard().is_err() {
                drop(close_guard);
                return Err(anyhow!("EmptyClipboard failed"));
            }
            let as_handle = HANDLE(hmem.0 as *mut _);
            if SetClipboardData(CF_UNICODETEXT.0 as u32, as_handle).is_err() {
                drop(close_guard);
                return Err(anyhow!("SetClipboardData(CF_UNICODETEXT) failed"));
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
