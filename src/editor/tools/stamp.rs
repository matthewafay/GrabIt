//! Stamp tool.
//!
//! Ships four built-in PNGs embedded via `include_bytes!`: arrow, check, x,
//! star. User stamps (`%APPDATA%\GrabIt\stamps\`) are explicitly deferred to
//! M5 per the plan.

use crate::editor::document::{AnnotationNode, StampSource};
use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::sync::OnceLock;
use uuid::Uuid;

/// `(name, png_bytes)` for every built-in stamp.
pub const BUILTINS: &[(&str, &[u8])] = &[
    ("arrow", include_bytes!("../../../assets/stamps/arrow.png")),
    ("check", include_bytes!("../../../assets/stamps/check.png")),
    ("x",     include_bytes!("../../../assets/stamps/x.png")),
    ("star",  include_bytes!("../../../assets/stamps/star.png")),
];

#[allow(dead_code)] // kept for potential re-use; Stamp tool was removed from the UI.
pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|(n, _)| *n)
}

#[allow(dead_code)] // Used by M5 user-stamp import / previewing.
pub fn builtin_bytes(name: &str) -> Option<&'static [u8]> {
    BUILTINS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// Decoded cache for built-in stamps (decoded lazily once per process).
static DECODED: OnceLock<Vec<(&'static str, RgbaImage)>> = OnceLock::new();

fn decoded() -> &'static [(&'static str, RgbaImage)] {
    DECODED.get_or_init(|| {
        BUILTINS
            .iter()
            .filter_map(|(name, bytes)| {
                image::load_from_memory(bytes)
                    .ok()
                    .map(|img| (*name, img.to_rgba8()))
            })
            .collect()
    })
}

pub fn decoded_builtin(name: &str) -> Option<&'static RgbaImage> {
    decoded().iter().find(|(n, _)| *n == name).map(|(_, img)| img)
}

/// Resolve any `StampSource` to an RGBA image, whether it's builtin or
/// inline. Inline sources decode each time (rare, small payloads).
pub fn resolve(source: &StampSource) -> Result<RgbaImage> {
    match source {
        StampSource::Builtin { name } => decoded_builtin(name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown builtin stamp: {name}")),
        StampSource::Inline { png } => Ok(image::load_from_memory(png)?.to_rgba8()),
    }
}

#[allow(dead_code)] // kept for potential re-use; Stamp tool was removed from the UI.
pub fn make(source: StampSource, rect: [f32; 4]) -> AnnotationNode {
    AnnotationNode::Stamp {
        id: Uuid::new_v4(),
        source,
        rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_decode() {
        for name in builtin_names() {
            let img = decoded_builtin(name).expect("decode");
            assert!(img.width() > 0);
            assert!(img.height() > 0);
        }
    }
}
