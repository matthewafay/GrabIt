//! Embed JetBrains Mono into the process so every text surface — the egui
//! editor, GDI overlay labels, and the text-annotation rasterizer — can use
//! the same face without requiring the user to install it system-wide.

pub const JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
pub const JETBRAINS_MONO_BOLD: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf");

pub const FACE_NAME: &str = "JetBrains Mono";

/// Register the embedded TTFs with GDI so `CreateFontIndirectW` calls with
/// `lfFaceName = "JetBrains Mono"` find the font even on machines where it
/// isn't installed. Call once at startup. Safe to call multiple times.
#[cfg(windows)]
pub fn register_with_gdi() {
    use log::debug;
    use windows::Win32::Graphics::Gdi::AddFontMemResourceEx;

    unsafe {
        for (name, bytes) in [
            ("Regular", JETBRAINS_MONO_REGULAR),
            ("Bold", JETBRAINS_MONO_BOLD),
        ] {
            let mut num_fonts: u32 = 0;
            let handle = AddFontMemResourceEx(
                bytes.as_ptr() as *const _,
                bytes.len() as u32,
                None,
                &mut num_fonts as *mut u32,
            );
            if handle.0.is_null() {
                debug!("AddFontMemResourceEx({name}) returned null; overlays will use system fallback");
            } else {
                debug!("registered JetBrains Mono {name} with GDI ({num_fonts} face(s))");
            }
        }
    }
}

#[cfg(not(windows))]
pub fn register_with_gdi() {}
