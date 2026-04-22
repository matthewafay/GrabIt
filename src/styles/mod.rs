//! Quick styles for annotation tools (feature #19).
//!
//! A quick style is a named preset of tool-specific drawing parameters —
//! "Red arrow 4px", "Big yellow highlight", etc. Styles are keyed by tool
//! kind, so applying a style is a single lookup from `(Tool, name)` to a
//! `StyleValues` bundle that the editor copies into its active-tool fields.
//!
//! All styles live in one TOML file (`%APPDATA%\GrabIt\styles.toml`) — they
//! are small and load-at-open is cheap.

use crate::app::paths::AppPaths;
use anyhow::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tool kind a style belongs to. A subset of `editor::tools::Tool` — only
/// the tools with meaningful knobs get a style system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StyleToolKind {
    Arrow,
    Text,
    Callout,
    Rect,
    Ellipse,
    Step,
    Blur,
    Magnify,
}

impl StyleToolKind {
    pub fn label(self) -> &'static str {
        match self {
            StyleToolKind::Arrow => "Arrow",
            StyleToolKind::Text => "Text",
            StyleToolKind::Callout => "Callout",
            StyleToolKind::Rect => "Rect",
            StyleToolKind::Ellipse => "Ellipse",
            StyleToolKind::Step => "Step",
            StyleToolKind::Blur => "Blur",
            StyleToolKind::Magnify => "Magnify",
        }
    }

    pub const ALL: [StyleToolKind; 8] = [
        StyleToolKind::Arrow,
        StyleToolKind::Text,
        StyleToolKind::Callout,
        StyleToolKind::Rect,
        StyleToolKind::Ellipse,
        StyleToolKind::Step,
        StyleToolKind::Blur,
        StyleToolKind::Magnify,
    ];
}

/// Values that a style can set. The editor's toolbar has color/stroke/fill
/// sliders driven by `EditorApp` fields; a style just pokes those fields.
/// We keep one flat struct across all tools — fields a given tool doesn't
/// use are simply `None` and ignored on apply. This is nicer for TOML than
/// a per-tool sum type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleValues {
    /// Primary color (sRGBA, u8 per channel). For arrow: stroke color.
    /// For text: glyph color. For shape/callout: fill color.
    pub color: Option<[u8; 4]>,
    /// Secondary stroke color for shape/callout/magnifier borders.
    pub stroke_color: Option<[u8; 4]>,
    /// If true, shapes/callouts draw their fill; if false, outline-only.
    pub use_fill: Option<bool>,
    /// Stroke thickness / arrow thickness / magnifier border width.
    pub thickness: Option<f32>,
    /// Text size in document pixels.
    pub text_size: Option<f32>,
    /// Step circle radius in document pixels.
    pub step_radius: Option<f32>,
    /// Magnifier shape.
    pub magnify_circular: Option<bool>,
    /// Blur sigma in document pixels.
    pub blur_radius: Option<f32>,
}

/// A single named style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuickStyle {
    pub name: String,
    pub tool: StyleToolKind,
    #[serde(flatten)]
    pub values: StyleValues,
}

/// On-disk wrapper: `[[style]]` array so the TOML is easy to read.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StyleFile {
    #[serde(default, rename = "style")]
    styles: Vec<QuickStyle>,
}

/// In-memory style store. Lookups are linear; the expected cardinality is
/// a few dozen at most so a HashMap is over-engineered.
#[derive(Debug, Default, Clone)]
pub struct StyleStore {
    pub styles: Vec<QuickStyle>,
}

impl StyleStore {
    pub fn load(paths: &AppPaths) -> Self {
        let path = style_file(paths);
        match std::fs::read_to_string(&path) {
            Ok(body) => match toml::from_str::<StyleFile>(&body) {
                Ok(f) => {
                    debug!("loaded {} quick styles from {}", f.styles.len(), path.display());
                    StyleStore { styles: f.styles }
                }
                Err(e) => {
                    warn!("styles parse error ({}): {e}; using empty store", path.display());
                    StyleStore::default()
                }
            },
            Err(_) => {
                debug!("no styles file at {}; starting empty", path.display());
                StyleStore::default()
            }
        }
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let file = StyleFile { styles: self.styles.clone() };
        let body = toml::to_string_pretty(&file).context("serialize styles")?;
        let path = style_file(paths);
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
        debug!("saved {} styles to {}", self.styles.len(), path.display());
        Ok(())
    }

    /// Styles for a given tool, in insertion order.
    pub fn for_tool(&self, tool: StyleToolKind) -> impl Iterator<Item = &QuickStyle> {
        self.styles.iter().filter(move |s| s.tool == tool)
    }

    /// Upsert: if a style with the same `(tool, name)` exists, overwrite it;
    /// otherwise append.
    pub fn upsert(&mut self, style: QuickStyle) {
        if let Some(slot) = self
            .styles
            .iter_mut()
            .find(|s| s.tool == style.tool && s.name == style.name)
        {
            *slot = style;
        } else {
            self.styles.push(style);
        }
    }

    pub fn remove(&mut self, tool: StyleToolKind, name: &str) {
        self.styles.retain(|s| !(s.tool == tool && s.name == name));
    }
}

pub fn style_file(paths: &AppPaths) -> PathBuf {
    paths.data_dir.join("styles.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(store: &StyleStore) -> StyleStore {
        let file = StyleFile { styles: store.styles.clone() };
        let body = toml::to_string_pretty(&file).expect("serialize");
        let back: StyleFile = toml::from_str(&body).expect("parse");
        StyleStore { styles: back.styles }
    }

    #[test]
    fn style_roundtrip_arrow() {
        let mut store = StyleStore::default();
        store.upsert(QuickStyle {
            name: "Red 4px".into(),
            tool: StyleToolKind::Arrow,
            values: StyleValues {
                color: Some([220, 40, 40, 255]),
                thickness: Some(4.0),
                ..Default::default()
            },
        });
        let back = roundtrip(&store);
        assert_eq!(back.styles.len(), 1);
        assert_eq!(back.styles[0].tool, StyleToolKind::Arrow);
        assert_eq!(back.styles[0].values.thickness, Some(4.0));
        assert_eq!(back.styles[0].values.color, Some([220, 40, 40, 255]));
    }

    #[test]
    fn style_roundtrip_multi_tool() {
        let mut store = StyleStore::default();
        store.upsert(QuickStyle {
            name: "Big yellow".into(),
            tool: StyleToolKind::Text,
            values: StyleValues {
                color: Some([255, 220, 0, 255]),
                text_size: Some(64.0),
                ..Default::default()
            },
        });
        store.upsert(QuickStyle {
            name: "Heavy outline".into(),
            tool: StyleToolKind::Rect,
            values: StyleValues {
                stroke_color: Some([10, 10, 10, 255]),
                use_fill: Some(false),
                thickness: Some(8.0),
                ..Default::default()
            },
        });
        let back = roundtrip(&store);
        assert_eq!(back.styles.len(), 2);
        let text_style = back.for_tool(StyleToolKind::Text).next().unwrap();
        assert_eq!(text_style.values.text_size, Some(64.0));
        let rect_style = back.for_tool(StyleToolKind::Rect).next().unwrap();
        assert_eq!(rect_style.values.use_fill, Some(false));
    }

    #[test]
    fn upsert_replaces_same_name() {
        let mut store = StyleStore::default();
        store.upsert(QuickStyle {
            name: "x".into(),
            tool: StyleToolKind::Blur,
            values: StyleValues { blur_radius: Some(5.0), ..Default::default() },
        });
        store.upsert(QuickStyle {
            name: "x".into(),
            tool: StyleToolKind::Blur,
            values: StyleValues { blur_radius: Some(20.0), ..Default::default() },
        });
        assert_eq!(store.styles.len(), 1);
        assert_eq!(store.styles[0].values.blur_radius, Some(20.0));
    }

    #[test]
    fn remove_drops_matching_entry() {
        let mut store = StyleStore::default();
        store.upsert(QuickStyle {
            name: "x".into(),
            tool: StyleToolKind::Rect,
            values: StyleValues::default(),
        });
        store.upsert(QuickStyle {
            name: "y".into(),
            tool: StyleToolKind::Rect,
            values: StyleValues::default(),
        });
        store.remove(StyleToolKind::Rect, "x");
        assert_eq!(store.styles.len(), 1);
        assert_eq!(store.styles[0].name, "y");
    }
}
