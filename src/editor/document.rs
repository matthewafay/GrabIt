//! Editor document model.
//!
//! A `Document` is what gets serialized to a `.grabit` file. It carries the
//! base image, an optional cursor layer (for feature #2), an ordered list of
//! annotations (empty at M0 — M3 introduces the `AnnotationNode` variants),
//! and the capture metadata from `capture::CaptureMetadata`.
//!
//! Format on disk: MessagePack via `rmp-serde`. MessagePack is chosen over
//! JSON because the base-image blob is binary; TOML is ruled out for the
//! same reason. Opening `.grabit` in a text editor will show binary soup,
//! which is expected.

use crate::capture::{CaptureMetadata, CaptureResult};
use anyhow::{Context, Result};
use image::{codecs::png::PngEncoder, ImageEncoder, RgbaImage};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Serialization version. Bump when the schema changes in a way that breaks
/// older readers; M0 starts at 1.
pub const DOCUMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub schema_version: u32,
    pub id: Uuid,
    /// PNG-encoded base image (chosen over raw RGBA to shrink file size by
    /// ~10x on typical screenshots).
    pub base_png: Vec<u8>,
    pub base_width: u32,
    pub base_height: u32,

    pub cursor: Option<SerializedCursor>,
    pub annotations: Vec<AnnotationNode>,
    pub metadata: CaptureMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerializedCursor {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Position of cursor top-left, relative to the base image's top-left.
    pub x: i32,
    pub y: i32,
}

/// Annotation scene-graph node. New variants are added as each annotation
/// tool lands. All coordinates are in image-pixel space (relative to the
/// top-left of `base_png`), not editor-canvas space — tools convert.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnnotationNode {
    Arrow {
        id: Uuid,
        start: [f32; 2],
        end: [f32; 2],
        /// sRGB color in RGBA order.
        color: [u8; 4],
        /// Stroke thickness in image pixels.
        thickness: f32,
    },
    Text {
        id: Uuid,
        /// Top-left anchor in image-pixel coordinates.
        position: [f32; 2],
        text: String,
        /// sRGB color in RGBA order.
        color: [u8; 4],
        /// Font size in image pixels (cap height + descender roughly = size_px).
        size_px: f32,
    },
}

/// Build a fresh `Document` from a `CaptureResult`. Annotations start empty.
pub fn from_capture(result: &CaptureResult) -> Result<Document> {
    let base_png = encode_png(&result.base).context("encode base PNG")?;
    let cursor = match &result.cursor {
        Some(c) => Some(SerializedCursor {
            png: encode_png(&c.image).context("encode cursor PNG")?,
            width: c.image.width(),
            height: c.image.height(),
            x: c.x,
            y: c.y,
        }),
        None => None,
    };

    Ok(Document {
        schema_version: DOCUMENT_SCHEMA_VERSION,
        id: Uuid::new_v4(),
        base_png,
        base_width: result.base.width(),
        base_height: result.base.height(),
        cursor,
        annotations: Vec::new(),
        metadata: result.metadata.clone(),
    })
}

/// Persist a freshly produced `CaptureResult` to disk as a `.grabit` file.
pub fn save_from_capture(result: &CaptureResult, path: &Path) -> Result<()> {
    let doc = from_capture(result)?;
    save(&doc, path)
}

/// Serialize a Document to a `.grabit` file on disk.
pub fn save(doc: &Document, path: &Path) -> Result<()> {
    let bytes = rmp_serde::to_vec_named(doc).context("serialize document")?;
    std::fs::write(path, bytes)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[allow(dead_code)]
pub fn load(path: &Path) -> Result<Document> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("read {}", path.display()))?;
    let doc: Document = rmp_serde::from_slice(&bytes).context("deserialize document")?;
    Ok(doc)
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity((img.width() * img.height() * 2) as usize);
    let encoder = PngEncoder::new(&mut buf);
    encoder
        .write_image(img.as_raw(), img.width(), img.height(), image::ExtendedColorType::Rgba8)
        .context("PNG encode")?;
    Ok(buf)
}
