//! Editor document model.
//!
//! A `Document` is what gets serialized to a `.grabit` file. It carries the
//! base image, an optional cursor layer (for feature #2), an ordered list of
//! annotations, and the capture metadata from `capture::CaptureMetadata`.
//!
//! Format on disk: MessagePack via `rmp-serde`. MessagePack is chosen over
//! JSON because the base-image blob is binary; TOML is ruled out for the
//! same reason. Opening `.grabit` in a text editor will show binary soup,
//! which is expected.
//!
//! Schema versions:
//! - v1 (M0–M2 + initial M3): only `Arrow` and `Text` annotation variants.
//! - v2 (M3 complete): adds `Callout`, `Shape`, `Step`, `Stamp`, `Magnify`.
//!   Because the enum is serialized with an internal `"kind"` tag via serde,
//!   v1 files still deserialize cleanly as long as readers know the old
//!   variant tags — which they do, since the two original variants are
//!   unchanged. v1 files simply have `schema_version = 1` and no new
//!   variants. We do not rewrite v1 files on load.
//! - v3 (M4): adds `Blur` and `CaptureInfo` annotation variants plus two
//!   top-level document fields (`edge_effect`, `border`). New fields are
//!   tagged `#[serde(default)]` so v1/v2 files still deserialize cleanly.
//! - v4: `AnnotationNode::Text` changes shape from `position: [f32; 2]` to
//!   `rect: [f32; 4]` (drag-to-create text box with word-wrap). v1/v2/v3
//!   documents that contain any old-shape Text nodes will fail to
//!   deserialize — that is an accepted migration cost. Documents with no
//!   Text nodes continue to load from any prior version.

use crate::capture::{CaptureMetadata, CaptureResult};
use anyhow::{Context, Result};
use image::{codecs::png::PngEncoder, ImageEncoder, RgbaImage};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// Serialization version. Bump when the schema changes in a way that breaks
/// older readers. M3 introduced five new `AnnotationNode` variants; because
/// rmp-serde's internally-tagged enums gracefully accept the older (smaller)
/// variant set, v1 files still load. M4 adds two more annotation variants
/// plus two top-level fields (`edge_effect`, `border`); the new fields are
/// `#[serde(default)]` so v1/v2 documents still deserialize cleanly. v4
/// reshapes the `Text` variant (`position` → `rect`): documents with no
/// Text nodes load fine across versions; documents that contain old-shape
/// Text nodes will fail to deserialize (accepted migration cost).
pub const DOCUMENT_SCHEMA_VERSION: u32 = 4;

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

    /// M4: torn-edge effect applied at flatten-time on one side of the image.
    /// `None` (default) means "no effect" — v1/v2 docs load with this unset.
    #[serde(default)]
    pub edge_effect: Option<EdgeEffect>,

    /// M4: image border (solid band + optional drop shadow) applied at
    /// flatten-time. `None` (default) means no border. v1/v2 docs default.
    #[serde(default)]
    pub border: Option<Border>,
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

/// Rect kind for the `Shape` annotation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Rect,
    Ellipse,
}

/// Which fields to include in a capture-info stamp.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Timestamp,
    WindowTitle,
    ProcessName,
    OsVersion,
    MonitorInfo,
}

impl FieldKind {
    pub fn label(self) -> &'static str {
        match self {
            FieldKind::Timestamp => "Timestamp",
            FieldKind::WindowTitle => "Window title",
            FieldKind::ProcessName => "Process name",
            FieldKind::OsVersion => "OS version",
            FieldKind::MonitorInfo => "Monitor info",
        }
    }
}

/// Anchor for a capture-info stamp on the base image.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureInfoPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl CaptureInfoPosition {
    pub fn label(self) -> &'static str {
        match self {
            CaptureInfoPosition::TopLeft => "Top left",
            CaptureInfoPosition::TopRight => "Top right",
            CaptureInfoPosition::BottomLeft => "Bottom left",
            CaptureInfoPosition::BottomRight => "Bottom right",
        }
    }
}

/// Style bundle for the capture-info banner.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CaptureInfoStyle {
    /// Box fill (RGBA).
    pub fill: [u8; 4],
    /// Text color (RGBA).
    pub text_color: [u8; 4],
    /// Font size in image pixels.
    pub text_size: f32,
    /// Inner padding on all sides (image pixels).
    pub padding: f32,
}

impl Default for CaptureInfoStyle {
    fn default() -> Self {
        Self {
            fill: [20, 20, 20, 200],
            text_color: [240, 240, 240, 255],
            text_size: 14.0,
            padding: 8.0,
        }
    }
}

/// Which edge of the base image gets a torn-paper cutout.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Torn-edge effect applied at flatten-time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EdgeEffect {
    pub edge: Edge,
    /// Depth (in image pixels) the jagged teeth extend INTO the image from
    /// the selected edge.
    pub depth: f32,
    /// Tooth period in image pixels — distance between peaks.
    pub teeth: f32,
}

impl Default for EdgeEffect {
    fn default() -> Self {
        Self { edge: Edge::Bottom, depth: 14.0, teeth: 18.0 }
    }
}

/// Image border + optional drop shadow applied at flatten-time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Border {
    pub color: [u8; 4],
    pub width: f32,
    /// If non-zero, render a drop-shadow of this radius/offset outside the
    /// border. Shadow is drawn as a soft RGBA box.
    pub shadow_radius: f32,
    pub shadow_offset: [f32; 2],
    pub shadow_color: [u8; 4],
}

impl Default for Border {
    fn default() -> Self {
        Self {
            color: [30, 30, 30, 255],
            width: 6.0,
            shadow_radius: 0.0,
            shadow_offset: [0.0, 0.0],
            shadow_color: [0, 0, 0, 128],
        }
    }
}

/// Which built-in stamp (or a user-supplied PNG blob).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StampSource {
    /// References a stamp shipped in the binary. Name must match one of the
    /// identifiers returned by `tools::stamp::builtin_names`.
    Builtin { name: String },
    /// Inlined PNG bytes — for user-imported stamps (M5) or stamps from
    /// round-tripped `.grabit` files whose original builtin is missing.
    Inline { png: Vec<u8> },
}

/// Horizontal text alignment inside a `Text` annotation's wrap-width box.
/// Applied per visual line at rasterize-time and to the live preview's
/// `LayoutJob`, so the two paths match. Serialized kebab-case so the wire
/// format is human-readable if anyone hex-dumps a `.grabit`. A new field
/// on `AnnotationNode::Text` uses `#[serde(default)]`, so legacy v4 docs
/// without the tag deserialize with `Left`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Shaft dash pattern for an `Arrow` annotation. `#[serde(default)]` on the
/// node keeps pre-existing arrows loading as `Solid`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum ArrowLineStyle {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

/// Head/tail rendering for an `Arrow` annotation. `#[serde(default)]` on the
/// node keeps pre-existing arrows loading as `FilledTriangle`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum ArrowHeadStyle {
    #[default]
    FilledTriangle,
    /// Triangle stroked but not filled — flatter, more modern look.
    OutlineTriangle,
    /// Two short strokes forming `→` chevron, no filled shape.
    LineOnly,
    /// No head at all — just a line.
    None,
    /// Filled triangle at both endpoints (for equivalence / comparison).
    DoubleEnded,
}

/// Per-paragraph list style for a `Text` annotation. Applied at rasterize-
/// and preview-time: each `\n`-separated paragraph gets a marker prefix
/// (`"• "` for `Bullet`, `"1. "`, `"2. "`, … for `Numbered`). Empty
/// paragraphs stay empty and do NOT consume a number. Wrapped continuation
/// lines are hanging-indented so body text aligns past the marker. Stored
/// with `#[serde(default)]` on the node so legacy v4 docs load as `None`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum TextListStyle {
    #[default]
    None,
    Bullet,
    Numbered,
}

/// Annotation scene-graph node. Variants are added as each annotation tool
/// lands. All coordinates are in image-pixel space (relative to the top-left
/// of `base_png`), not editor-canvas space — tools convert.
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
        /// If true, render a soft dark drop shadow under the arrow.
        /// Per-annotation toggle; `#[serde(default)]` keeps pre-shadow docs
        /// (all schemas ≤ v4) loading with the shadow off.
        #[serde(default)]
        shadow: bool,
        /// Shaft dash pattern. `#[serde(default)]` → Solid for older docs.
        #[serde(default)]
        line_style: ArrowLineStyle,
        /// Head/cap treatment. `#[serde(default)]` → FilledTriangle.
        #[serde(default)]
        head_style: ArrowHeadStyle,
        /// Optional quadratic-bezier control point (image-pixel coords). If
        /// `Some`, the arrow renders as a curve from `start` through the
        /// control to `end`. If `None`, it's a straight line (legacy
        /// behaviour). `#[serde(default)]` → `None`.
        #[serde(default)]
        control: Option<[f32; 2]>,
    },
    Text {
        id: Uuid,
        /// Text-box bounds in image-pixel coordinates: `[min_x, min_y,
        /// max_x, max_y]`. Text word-wraps at `max_x`; `max_y` defines the
        /// minimum render height but text overflowing below is still drawn
        /// so nothing visibly disappears.
        rect: [f32; 4],
        text: String,
        /// sRGB color in RGBA order.
        color: [u8; 4],
        /// Font size in image pixels (cap height + descender roughly = size_px).
        size_px: f32,
        /// If true, render a frosted-glass backdrop (gaussian blur of the
        /// base pixels + translucent white tint) behind the text. Optional
        /// per-annotation toggle; `#[serde(default)]` keeps v4 docs loading.
        #[serde(default)]
        frosted: bool,
        /// If true, render a soft dark drop shadow behind the text rect.
        /// Per-annotation toggle; `#[serde(default)]` keeps v4 docs loading.
        #[serde(default)]
        shadow: bool,
        /// Horizontal alignment of each wrapped line inside the text-box.
        /// `#[serde(default)]` so pre-alignment v4 docs (and all earlier
        /// schemas) load as `Left`.
        #[serde(default)]
        align: TextAlign,
        /// Per-paragraph list style (None / Bullet / Numbered). Applied by
        /// rasterize + preview. `#[serde(default)]` so pre-list documents
        /// deserialize with `None`.
        #[serde(default)]
        list: TextListStyle,
    },
    /// Speech-balloon with a tail. The balloon is an axis-aligned rounded
    /// rectangle; the tail is a triangular pointer whose tip is at `tail`.
    Callout {
        id: Uuid,
        /// Balloon body in image-pixel coords: `[min_x, min_y, max_x, max_y]`.
        rect: [f32; 4],
        /// Tail tip (moves independently of the balloon).
        tail: [f32; 2],
        text: String,
        /// Fill (RGBA) for the balloon body.
        fill: [u8; 4],
        /// Stroke color.
        stroke: [u8; 4],
        /// Stroke thickness in image pixels.
        stroke_width: f32,
        /// Text color.
        text_color: [u8; 4],
        /// Text size in image pixels.
        text_size: f32,
    },
    /// Rectangle or ellipse with an outline and optional fill.
    Shape {
        id: Uuid,
        shape: ShapeKind,
        /// `[min_x, min_y, max_x, max_y]` in image-pixel coords.
        rect: [f32; 4],
        stroke: [u8; 4],
        stroke_width: f32,
        /// Fill color; an alpha of 0 means "no fill" (just outline).
        fill: [u8; 4],
    },
    /// Auto-numbered step marker — a filled circle with a centered digit.
    Step {
        id: Uuid,
        /// Center of the circle in image-pixel coords.
        center: [f32; 2],
        /// Radius in image pixels.
        radius: f32,
        /// The integer displayed in the circle (1-based, auto-assigned on
        /// creation; user can edit later).
        number: u32,
        fill: [u8; 4],
        text_color: [u8; 4],
    },
    /// PNG sticker. Rendered with aspect-preserving fit into `rect`.
    Stamp {
        id: Uuid,
        source: StampSource,
        /// `[min_x, min_y, max_x, max_y]` — the bounding box into which the
        /// stamp is drawn with alpha blending.
        rect: [f32; 4],
    },
    /// Loupe / magnifier: copies the pixels in `source_rect` and draws them
    /// scaled into `target_rect`, with an optional border.
    Magnify {
        id: Uuid,
        /// Source region sampled from the base image.
        source_rect: [f32; 4],
        /// Destination rect the magnified pixels are drawn into.
        target_rect: [f32; 4],
        /// Border stroke around the target rect.
        border: [u8; 4],
        border_width: f32,
        /// If true, the target rect is clipped to an ellipse.
        circular: bool,
    },
    /// Non-destructive gaussian blur over a region of the base image.
    /// The `.grabit` stores only the rect + radius; at flatten/export time
    /// the pixels of `base` inside `rect` are gaussian-blurred and blitted
    /// onto the output. Preview draws a cheap stippled overlay to hint.
    Blur {
        id: Uuid,
        /// `[min_x, min_y, max_x, max_y]` in image-pixel coords.
        rect: [f32; 4],
        /// Gaussian sigma in image pixels. 8–20 is typical.
        radius_px: f32,
    },
    /// Capture-info banner. Reads live `CaptureMetadata` at flatten-time.
    /// The node only stores which fields the user wants and where the block
    /// goes — the actual string content is materialised from the document's
    /// metadata during rasterize::flatten.
    CaptureInfo {
        id: Uuid,
        position: CaptureInfoPosition,
        fields: Vec<FieldKind>,
        style: CaptureInfoStyle,
    },
}

impl AnnotationNode {
    pub fn id(&self) -> Uuid {
        match self {
            AnnotationNode::Arrow { id, .. } => *id,
            AnnotationNode::Text { id, .. } => *id,
            AnnotationNode::Callout { id, .. } => *id,
            AnnotationNode::Shape { id, .. } => *id,
            AnnotationNode::Step { id, .. } => *id,
            AnnotationNode::Stamp { id, .. } => *id,
            AnnotationNode::Magnify { id, .. } => *id,
            AnnotationNode::Blur { id, .. } => *id,
            AnnotationNode::CaptureInfo { id, .. } => *id,
        }
    }
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
        edge_effect: None,
        border: None,
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

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Rect;
    use chrono::TimeZone;

    fn stub_metadata() -> CaptureMetadata {
        CaptureMetadata {
            captured_at: chrono::Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            foreground_title: None,
            foreground_process: None,
            os_version: "test".into(),
            monitors: vec![],
            capture_rect: Rect { x: 0, y: 0, width: 4, height: 4 },
        }
    }

    fn stub_doc() -> Document {
        // 2x2 transparent base PNG so the doc is valid without a real capture.
        let img = RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        let base_png = encode_png(&img).unwrap();
        Document {
            schema_version: DOCUMENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            base_png,
            base_width: 2,
            base_height: 2,
            cursor: None,
            annotations: Vec::new(),
            metadata: stub_metadata(),
            edge_effect: None,
            border: None,
        }
    }

    fn round_trip(doc: &Document) -> Document {
        let bytes = rmp_serde::to_vec_named(doc).expect("serialize");
        rmp_serde::from_slice(&bytes).expect("deserialize")
    }

    #[test]
    fn round_trip_arrow() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Arrow {
            id: Uuid::new_v4(),
            start: [1.0, 2.0],
            end: [3.0, 4.0],
            color: [10, 20, 30, 40],
            thickness: 2.5,
            shadow: false,
            line_style: ArrowLineStyle::default(),
            head_style: ArrowHeadStyle::default(),
            control: None,
        });
        let back = round_trip(&doc);
        assert_eq!(back.annotations.len(), 1);
        assert!(matches!(back.annotations[0], AnnotationNode::Arrow { .. }));
    }

    #[test]
    fn round_trip_text() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Text {
            id: Uuid::new_v4(),
            rect: [5.0, 6.0, 105.0, 56.0],
            text: "hello\nworld".into(),
            color: [1, 2, 3, 4],
            size_px: 18.0,
            frosted: false,
            shadow: false,
            align: TextAlign::Left,
            list: TextListStyle::None,
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Text { rect, text, .. } = &back.annotations[0] {
            assert_eq!(text, "hello\nworld");
            assert_eq!(*rect, [5.0, 6.0, 105.0, 56.0]);
        } else {
            panic!("wrong variant");
        }
    }

    /// Frosted / shadow flags + alignment round-trip when set. Also
    /// confirms the `#[serde(default)]` path: an old-shape v4 Text
    /// document (no `frosted` / `shadow` / `align` fields on the wire)
    /// deserialises with both flags off and align = `Left`.
    #[test]
    fn round_trip_text_effects_flags() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Text {
            id: Uuid::new_v4(),
            rect: [0.0, 0.0, 100.0, 40.0],
            text: "hi".into(),
            color: [10, 20, 30, 255],
            size_px: 16.0,
            frosted: true,
            shadow: true,
            align: TextAlign::Center,
            list: TextListStyle::None,
        });
        let back = round_trip(&doc);
        match &back.annotations[0] {
            AnnotationNode::Text { frosted, shadow, align, .. } => {
                assert!(*frosted);
                assert!(*shadow);
                assert_eq!(*align, TextAlign::Center);
            }
            _ => panic!("wrong variant"),
        }

        // Old-shape v4 document that predates the flags: build a pared-down
        // companion enum that serialises without the new fields and confirm
        // it deserialises cleanly with defaults.
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LegacyNode {
            Text {
                id: Uuid,
                rect: [f32; 4],
                text: String,
                color: [u8; 4],
                size_px: f32,
            },
        }
        #[derive(Serialize)]
        struct LegacyDoc {
            schema_version: u32,
            id: Uuid,
            base_png: Vec<u8>,
            base_width: u32,
            base_height: u32,
            cursor: Option<SerializedCursor>,
            annotations: Vec<LegacyNode>,
            metadata: CaptureMetadata,
            edge_effect: Option<EdgeEffect>,
            border: Option<Border>,
        }
        let base = stub_doc();
        let legacy = LegacyDoc {
            schema_version: 4,
            id: base.id,
            base_png: base.base_png.clone(),
            base_width: base.base_width,
            base_height: base.base_height,
            cursor: None,
            annotations: vec![LegacyNode::Text {
                id: Uuid::new_v4(),
                rect: [1.0, 2.0, 3.0, 4.0],
                text: "old".into(),
                color: [1, 2, 3, 4],
                size_px: 12.0,
            }],
            metadata: base.metadata.clone(),
            edge_effect: None,
            border: None,
        };
        let bytes = rmp_serde::to_vec_named(&legacy).unwrap();
        let loaded: Document = rmp_serde::from_slice(&bytes).expect("legacy text loads");
        match &loaded.annotations[0] {
            AnnotationNode::Text { frosted, shadow, align, .. } => {
                assert!(!*frosted);
                assert!(!*shadow);
                assert_eq!(*align, TextAlign::Left);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Round-trip Text with each non-default `align` value, and confirm a
    /// legacy-shape Text (frosted/shadow on the wire but no `align` key)
    /// defaults to `Left`.
    #[test]
    fn round_trip_text_align() {
        for variant in [TextAlign::Center, TextAlign::Right] {
            let mut doc = stub_doc();
            doc.annotations.push(AnnotationNode::Text {
                id: Uuid::new_v4(),
                rect: [0.0, 0.0, 100.0, 40.0],
                text: "hi".into(),
                color: [10, 20, 30, 255],
                size_px: 16.0,
                frosted: false,
                shadow: false,
                align: variant,
                list: TextListStyle::None,
            });
            let back = round_trip(&doc);
            match &back.annotations[0] {
                AnnotationNode::Text { align, .. } => assert_eq!(*align, variant),
                _ => panic!("wrong variant"),
            }
        }

        // Legacy v4 document with frosted/shadow but no `align` key must
        // deserialise with `align = Left`.
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LegacyNoAlign {
            Text {
                id: Uuid,
                rect: [f32; 4],
                text: String,
                color: [u8; 4],
                size_px: f32,
                frosted: bool,
                shadow: bool,
            },
        }
        #[derive(Serialize)]
        struct LegacyDoc {
            schema_version: u32,
            id: Uuid,
            base_png: Vec<u8>,
            base_width: u32,
            base_height: u32,
            cursor: Option<SerializedCursor>,
            annotations: Vec<LegacyNoAlign>,
            metadata: CaptureMetadata,
            edge_effect: Option<EdgeEffect>,
            border: Option<Border>,
        }
        let base = stub_doc();
        let legacy = LegacyDoc {
            schema_version: 4,
            id: base.id,
            base_png: base.base_png.clone(),
            base_width: base.base_width,
            base_height: base.base_height,
            cursor: None,
            annotations: vec![LegacyNoAlign::Text {
                id: Uuid::new_v4(),
                rect: [1.0, 2.0, 3.0, 4.0],
                text: "old".into(),
                color: [1, 2, 3, 4],
                size_px: 12.0,
                frosted: true,
                shadow: false,
            }],
            metadata: base.metadata.clone(),
            edge_effect: None,
            border: None,
        };
        let bytes = rmp_serde::to_vec_named(&legacy).unwrap();
        let loaded: Document =
            rmp_serde::from_slice(&bytes).expect("legacy text-no-align loads");
        match &loaded.annotations[0] {
            AnnotationNode::Text { align, frosted, .. } => {
                assert_eq!(*align, TextAlign::Left);
                assert!(*frosted);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Round-trip Text with each non-default list style, and confirm a
    /// legacy-shape Text (align on the wire but no `list` key) defaults to
    /// `TextListStyle::None`.
    #[test]
    fn round_trip_text_list() {
        for variant in [TextListStyle::Bullet, TextListStyle::Numbered] {
            let mut doc = stub_doc();
            doc.annotations.push(AnnotationNode::Text {
                id: Uuid::new_v4(),
                rect: [0.0, 0.0, 100.0, 40.0],
                text: "a\nb".into(),
                color: [10, 20, 30, 255],
                size_px: 16.0,
                frosted: false,
                shadow: false,
                align: TextAlign::Left,
                list: variant,
            });
            let back = round_trip(&doc);
            match &back.annotations[0] {
                AnnotationNode::Text { list, .. } => assert_eq!(*list, variant),
                _ => panic!("wrong variant"),
            }
        }

        // Legacy v4 document with frosted/shadow/align but no `list` key —
        // must deserialize with `list = None`.
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LegacyNoList {
            Text {
                id: Uuid,
                rect: [f32; 4],
                text: String,
                color: [u8; 4],
                size_px: f32,
                frosted: bool,
                shadow: bool,
                align: TextAlign,
            },
        }
        #[derive(Serialize)]
        struct LegacyDoc {
            schema_version: u32,
            id: Uuid,
            base_png: Vec<u8>,
            base_width: u32,
            base_height: u32,
            cursor: Option<SerializedCursor>,
            annotations: Vec<LegacyNoList>,
            metadata: CaptureMetadata,
            edge_effect: Option<EdgeEffect>,
            border: Option<Border>,
        }
        let base = stub_doc();
        let legacy = LegacyDoc {
            schema_version: 4,
            id: base.id,
            base_png: base.base_png.clone(),
            base_width: base.base_width,
            base_height: base.base_height,
            cursor: None,
            annotations: vec![LegacyNoList::Text {
                id: Uuid::new_v4(),
                rect: [1.0, 2.0, 3.0, 4.0],
                text: "old".into(),
                color: [1, 2, 3, 4],
                size_px: 12.0,
                frosted: false,
                shadow: false,
                align: TextAlign::Right,
            }],
            metadata: base.metadata.clone(),
            edge_effect: None,
            border: None,
        };
        let bytes = rmp_serde::to_vec_named(&legacy).unwrap();
        let loaded: Document =
            rmp_serde::from_slice(&bytes).expect("legacy text-no-list loads");
        match &loaded.annotations[0] {
            AnnotationNode::Text { list, align, .. } => {
                assert_eq!(*list, TextListStyle::None);
                assert_eq!(*align, TextAlign::Right);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_callout() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Callout {
            id: Uuid::new_v4(),
            rect: [10.0, 10.0, 60.0, 40.0],
            tail: [5.0, 50.0],
            text: "note".into(),
            fill: [255, 255, 220, 230],
            stroke: [0, 0, 0, 255],
            stroke_width: 2.0,
            text_color: [0, 0, 0, 255],
            text_size: 16.0,
        });
        let back = round_trip(&doc);
        assert!(matches!(back.annotations[0], AnnotationNode::Callout { .. }));
    }

    #[test]
    fn round_trip_shape() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Shape {
            id: Uuid::new_v4(),
            shape: ShapeKind::Ellipse,
            rect: [0.0, 0.0, 100.0, 50.0],
            stroke: [200, 0, 0, 255],
            stroke_width: 3.0,
            fill: [0, 0, 0, 0],
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Shape { shape, .. } = &back.annotations[0] {
            assert_eq!(*shape, ShapeKind::Ellipse);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn round_trip_step() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Step {
            id: Uuid::new_v4(),
            center: [40.0, 40.0],
            radius: 20.0,
            number: 3,
            fill: [220, 40, 40, 255],
            text_color: [255, 255, 255, 255],
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Step { number, .. } = &back.annotations[0] {
            assert_eq!(*number, 3);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn round_trip_stamp_builtin() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Stamp {
            id: Uuid::new_v4(),
            source: StampSource::Builtin { name: "check".into() },
            rect: [0.0, 0.0, 64.0, 64.0],
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Stamp {
            source: StampSource::Builtin { name }, ..
        } = &back.annotations[0]
        {
            assert_eq!(name, "check");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn round_trip_stamp_inline() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Stamp {
            id: Uuid::new_v4(),
            source: StampSource::Inline { png: vec![0x89, b'P', b'N', b'G'] },
            rect: [0.0, 0.0, 64.0, 64.0],
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Stamp {
            source: StampSource::Inline { png }, ..
        } = &back.annotations[0]
        {
            assert_eq!(png.len(), 4);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn round_trip_magnify() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Magnify {
            id: Uuid::new_v4(),
            source_rect: [10.0, 10.0, 30.0, 30.0],
            target_rect: [100.0, 100.0, 200.0, 200.0],
            border: [255, 255, 255, 255],
            border_width: 3.0,
            circular: true,
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Magnify { circular, .. } = &back.annotations[0] {
            assert!(*circular);
        } else {
            panic!("wrong variant");
        }
    }

    /// v1 documents (Arrow/Text only, schema_version = 1) must still load.
    /// We manufacture a v1-shaped document by serialising with a rewritten
    /// schema_version and asserting reload succeeds.
    #[test]
    fn v1_documents_still_load() {
        let mut doc = stub_doc();
        doc.schema_version = 1;
        doc.annotations.push(AnnotationNode::Arrow {
            id: Uuid::new_v4(),
            start: [0.0, 0.0],
            end: [10.0, 10.0],
            color: [255, 0, 0, 255],
            thickness: 4.0,
            shadow: false,
            line_style: ArrowLineStyle::default(),
            head_style: ArrowHeadStyle::default(),
            control: None,
        });
        let back = round_trip(&doc);
        assert_eq!(back.schema_version, 1);
        assert_eq!(back.annotations.len(), 1);
    }

    #[test]
    fn round_trip_blur() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::Blur {
            id: Uuid::new_v4(),
            rect: [0.0, 0.0, 32.0, 32.0],
            radius_px: 12.0,
        });
        let back = round_trip(&doc);
        if let AnnotationNode::Blur { radius_px, .. } = &back.annotations[0] {
            assert!((*radius_px - 12.0).abs() < f32::EPSILON);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn round_trip_capture_info() {
        let mut doc = stub_doc();
        doc.annotations.push(AnnotationNode::CaptureInfo {
            id: Uuid::new_v4(),
            position: CaptureInfoPosition::BottomRight,
            fields: vec![
                FieldKind::Timestamp,
                FieldKind::WindowTitle,
                FieldKind::OsVersion,
            ],
            style: CaptureInfoStyle::default(),
        });
        let back = round_trip(&doc);
        if let AnnotationNode::CaptureInfo { position, fields, .. } = &back.annotations[0] {
            assert_eq!(*position, CaptureInfoPosition::BottomRight);
            assert_eq!(fields.len(), 3);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn round_trip_edge_effect() {
        let mut doc = stub_doc();
        doc.edge_effect = Some(EdgeEffect {
            edge: Edge::Right,
            depth: 20.0,
            teeth: 24.0,
        });
        let back = round_trip(&doc);
        let e = back.edge_effect.expect("edge_effect round-trip");
        assert_eq!(e.edge, Edge::Right);
        assert!((e.depth - 20.0).abs() < f32::EPSILON);
    }

    #[test]
    fn round_trip_border() {
        let mut doc = stub_doc();
        doc.border = Some(Border {
            color: [200, 100, 50, 255],
            width: 8.0,
            shadow_radius: 6.0,
            shadow_offset: [2.0, 3.0],
            shadow_color: [0, 0, 0, 150],
        });
        let back = round_trip(&doc);
        let b = back.border.expect("border round-trip");
        assert_eq!(b.color, [200, 100, 50, 255]);
        assert!((b.width - 8.0).abs() < f32::EPSILON);
        assert!((b.shadow_radius - 6.0).abs() < f32::EPSILON);
    }

    /// v2 documents (M3, no top-level `edge_effect` / `border` fields) must
    /// still deserialize cleanly into the v3 struct with the new fields
    /// defaulting to `None`. We fake a v2 document by serializing a minimal
    /// version of the on-wire shape with the old field set.
    #[test]
    fn v2_documents_still_load_with_defaults() {
        // Serialize a shape that OMITS the new fields entirely — we achieve
        // this by constructing a minimal companion struct, serializing it
        // with rmp-serde, and then deserializing it as the real Document.
        #[derive(Serialize)]
        struct DocV2 {
            schema_version: u32,
            id: Uuid,
            base_png: Vec<u8>,
            base_width: u32,
            base_height: u32,
            cursor: Option<SerializedCursor>,
            annotations: Vec<AnnotationNode>,
            metadata: CaptureMetadata,
        }

        let base = stub_doc();
        let v2 = DocV2 {
            schema_version: 2,
            id: base.id,
            base_png: base.base_png.clone(),
            base_width: base.base_width,
            base_height: base.base_height,
            cursor: None,
            annotations: vec![AnnotationNode::Arrow {
                id: Uuid::new_v4(),
                start: [0.0, 0.0],
                end: [4.0, 4.0],
                color: [255, 0, 0, 255],
                thickness: 2.0,
                shadow: false,
                line_style: ArrowLineStyle::default(),
                head_style: ArrowHeadStyle::default(),
                control: None,
            }],
            metadata: base.metadata.clone(),
        };
        let bytes = rmp_serde::to_vec_named(&v2).unwrap();
        let loaded: Document = rmp_serde::from_slice(&bytes).expect("v2 loads as v3");
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.annotations.len(), 1);
        assert!(loaded.edge_effect.is_none());
        assert!(loaded.border.is_none());
    }
}
