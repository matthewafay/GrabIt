//! Named capture presets (feature #3) + per-preset hotkeys (feature #4).
//!
//! Each preset is a small TOML file under `%APPDATA%\GrabIt\presets\`.
//! The file name is the preset slug; the `name` field inside is the
//! display name. Presets are human-editable — the on-disk schema is flat
//! (no `#[serde(tag)]`) so authoring by hand is straightforward.
//!
//! Example `presets/region-3s.toml`:
//! ```toml
//! name = "Region + 3s delay"
//! target = "region"
//! delay_ms = 3000
//! include_cursor = false
//! hotkey = "Ctrl+Shift+1"
//! post_action = "editor"
//! filename_template = "GrabIt-{timestamp}"
//! subfolder = ""
//! ```
//!
//! `target` takes one of: `"fullscreen"`, `"region"`, `"window"`,
//! `"exact-dims"`, `"object"`. When `target = "exact-dims"`, `width` and
//! `height` (in physical pixels) must also be provided. `"object"` runs
//! the UIA element picker (feature #5).

use crate::app::paths::AppPaths;
use anyhow::{anyhow, Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Post-capture action. Presets can override the global copy-to-clipboard
/// default so the user can have a "quiet save only" preset bound to one
/// hotkey and a "save + open editor" preset on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PostAction {
    /// Save the PNG, also copy to clipboard. Do not open the editor.
    #[default]
    SaveOnly,
    /// Save the PNG and open it in the annotation editor.
    Editor,
    /// Copy to clipboard only — no disk file.
    CopyOnly,
}

impl PostAction {
    pub fn label(self) -> &'static str {
        match self {
            PostAction::SaveOnly => "Save only",
            PostAction::Editor => "Save + open editor",
            PostAction::CopyOnly => "Copy to clipboard only",
        }
    }

    pub const ALL: [PostAction; 3] =
        [PostAction::SaveOnly, PostAction::Editor, PostAction::CopyOnly];
}

/// Kind of capture target. Mirrors `CaptureTarget` but omits the runtime-only
/// `Interactive` (which is an overlay-driven flow) and `Region` with a
/// literal rect (presets can't know a rect up front — "region" in a preset
/// means "pop the interactive region overlay").
///
/// Serialized as a lower-case string for hand-editability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PresetTargetKind {
    Fullscreen,
    /// Run the interactive region selector overlay.
    #[default]
    Region,
    /// Run the interactive region selector overlay but resolve to a window
    /// click when the user clicks without dragging.
    Window,
    /// Run the exact-dimensions overlay with `width` x `height`.
    ExactDims,
    /// Run the UIA object / menu picker (feature #5, M6).
    Object,
}

impl PresetTargetKind {
    pub fn label(self) -> &'static str {
        match self {
            PresetTargetKind::Fullscreen => "Fullscreen",
            PresetTargetKind::Region => "Region",
            PresetTargetKind::Window => "Region / window",
            PresetTargetKind::ExactDims => "Exact dimensions",
            PresetTargetKind::Object => "Object / menu",
        }
    }

    pub const ALL: [PresetTargetKind; 5] = [
        PresetTargetKind::Fullscreen,
        PresetTargetKind::Region,
        PresetTargetKind::Window,
        PresetTargetKind::ExactDims,
        PresetTargetKind::Object,
    ];
}

/// A single preset record. Field order matches the expected TOML layout so
/// `toml::to_string_pretty` produces stable files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Preset {
    /// Human-readable name. Not required to be unique on disk (the file
    /// stem is the identity), but the UI should keep it unique.
    pub name: String,
    /// Capture-target variant.
    pub target: PresetTargetKind,
    /// Countdown delay before the capture runs. 0 = no delay.
    pub delay_ms: u32,
    /// Whether the cursor should be captured as a separate layer.
    pub include_cursor: bool,
    /// Exact-dims width, if `target = "exact-dims"`. Ignored otherwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: u32,
    /// Exact-dims height, if `target = "exact-dims"`. Ignored otherwise.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub height: u32,
    /// Global hotkey chord (e.g. `"Ctrl+Shift+1"`). Empty string = unbound.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hotkey: String,
    /// What to do after the capture completes.
    pub post_action: PostAction,
    /// Output filename template. Supports `{timestamp}` and `{window}`.
    /// Extension is always `.png` — the template is the stem only.
    pub filename_template: String,
    /// Relative subfolder under the output directory. Empty = root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subfolder: String,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

impl Default for Preset {
    fn default() -> Self {
        Self {
            name: "Untitled".to_string(),
            target: PresetTargetKind::Region,
            delay_ms: 0,
            include_cursor: false,
            width: 0,
            height: 0,
            hotkey: String::new(),
            post_action: PostAction::SaveOnly,
            filename_template: "GrabIt-{timestamp}".to_string(),
            subfolder: String::new(),
        }
    }
}

impl Preset {
    /// The default "Region + 3s delay, no cursor" preset called out in the
    /// plan's M5 verification step. Seeded on first run when no presets exist.
    pub fn region_3s_default() -> Self {
        Self {
            name: "Region + 3s delay".to_string(),
            target: PresetTargetKind::Region,
            delay_ms: 3_000,
            include_cursor: false,
            width: 0,
            height: 0,
            hotkey: String::new(),
            post_action: PostAction::SaveOnly,
            filename_template: "GrabIt-{timestamp}".to_string(),
            subfolder: String::new(),
        }
    }

    /// Slug used as the file stem. Strips characters that are awkward in
    /// file names; never empty (falls back to `"preset"`).
    pub fn slug(&self) -> String {
        let mut out = String::with_capacity(self.name.len());
        for ch in self.name.chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase());
            } else if matches!(ch, ' ' | '-' | '_') && !out.ends_with('-') {
                out.push('-');
            }
            // drop everything else
        }
        let trimmed = out.trim_matches('-').to_string();
        if trimmed.is_empty() {
            "preset".to_string()
        } else {
            trimmed
        }
    }

    /// Resolve the filename template to an absolute PNG path. `window_title`
    /// is used by the `{window}` token; empty strings are replaced with
    /// `"unknown"` to avoid producing weird paths like `GrabIt-.png`.
    pub fn resolve_png_path(
        &self,
        paths: &AppPaths,
        window_title: Option<&str>,
        now: chrono::DateTime<chrono::Local>,
    ) -> PathBuf {
        let mut dir = paths.output_dir.clone();
        if !self.subfolder.trim().is_empty() {
            // Disallow absolute / parent-traversal subfolders. Any `..` or
            // drive-letter component is stripped, keeping the output bound
            // to the configured output_dir.
            for part in self.subfolder.split(['/', '\\']) {
                if part.is_empty() || part == "." || part == ".." {
                    continue;
                }
                dir.push(sanitize_path_component(part));
            }
        }
        let stem = render_template(
            &self.filename_template,
            &now.format("%Y%m%d-%H%M%S").to_string(),
            window_title.unwrap_or("unknown"),
        );
        let stem = sanitize_path_component(&stem);
        dir.join(format!("{stem}.png"))
    }
}

/// Replace any character that is illegal in a Windows path component with
/// an underscore. Collapses consecutive underscores for readability.
fn sanitize_path_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control() {
            if !out.ends_with('_') {
                out.push('_');
            }
        } else {
            out.push(ch);
        }
    }
    let trimmed = out.trim_matches(|c: char| c == '.' || c.is_whitespace()).to_string();
    if trimmed.is_empty() {
        "capture".to_string()
    } else {
        trimmed
    }
}

fn render_template(template: &str, timestamp: &str, window: &str) -> String {
    template
        .replace("{timestamp}", timestamp)
        .replace("{window}", window)
}

/// In-memory preset collection. Owns both the preset records and the slugs
/// they were loaded from (so rename-and-save can clean up the old file).
#[derive(Debug, Default, Clone)]
pub struct PresetStore {
    pub presets: Vec<Preset>,
}

impl PresetStore {
    /// Load every preset under `paths.presets_dir`. Malformed files are
    /// logged and skipped — one bad file does not disable the whole feature.
    pub fn load(paths: &AppPaths) -> Self {
        let mut store = PresetStore::default();
        let dir = &paths.presets_dir;
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                debug!("no presets dir yet ({}): {e}", dir.display());
                return store;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match Self::load_file(&path) {
                Ok(preset) => {
                    debug!("loaded preset {:?} from {}", preset.name, path.display());
                    store.presets.push(preset);
                }
                Err(e) => warn!("skipping preset {}: {e}", path.display()),
            }
        }
        // Stable order across reloads.
        store.presets.sort_by_key(|p| p.name.to_lowercase());
        store
    }

    /// Load presets; if none exist, seed the default "Region + 3s delay".
    /// Returns the store plus `true` if a default was written to disk.
    pub fn load_or_seed_default(paths: &AppPaths) -> (Self, bool) {
        let store = Self::load(paths);
        if !store.presets.is_empty() {
            return (store, false);
        }
        info!("no presets found; seeding default 'Region + 3s delay'");
        let default = Preset::region_3s_default();
        let mut store = PresetStore::default();
        if let Err(e) = save_preset(paths, &default) {
            warn!("could not save default preset: {e}");
        }
        store.presets.push(default);
        (store, true)
    }

    fn load_file(path: &Path) -> Result<Preset> {
        let body = std::fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        let preset: Preset = toml::from_str(&body)
            .with_context(|| format!("parse {}", path.display()))?;
        Ok(preset)
    }

    /// Find a preset by display name. Case-sensitive — presets created in
    /// the UI must have unique names.
    pub fn find(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|p| p.name == name)
    }

    /// All presets that declare a non-empty hotkey chord. Filtered list
    /// fed to `Registrar::refresh_hotkeys`.
    pub fn bound_hotkeys(&self) -> Vec<(String, String)> {
        self.presets
            .iter()
            .filter(|p| !p.hotkey.trim().is_empty())
            .map(|p| (p.hotkey.clone(), p.name.clone()))
            .collect()
    }
}

/// Write a preset to disk. Overwrites any existing file with the same slug.
pub fn save_preset(paths: &AppPaths, preset: &Preset) -> Result<()> {
    let body = toml::to_string_pretty(preset).context("serialize preset")?;
    let path = paths.preset_file(&preset.slug());
    std::fs::create_dir_all(&paths.presets_dir)
        .with_context(|| format!("create presets dir {}", paths.presets_dir.display()))?;
    std::fs::write(&path, body)
        .with_context(|| format!("write preset {}", path.display()))?;
    debug!("saved preset {:?} → {}", preset.name, path.display());
    Ok(())
}

/// Delete the preset file for `slug`. No-op if the file is already gone.
pub fn delete_preset_file(paths: &AppPaths, slug: &str) -> Result<()> {
    let path = paths.preset_file(slug);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("remove {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(p: &Preset) -> Preset {
        let body = toml::to_string_pretty(p).expect("serialize");
        toml::from_str::<Preset>(&body).expect("parse")
    }

    #[test]
    fn preset_roundtrip_fullscreen() {
        let p = Preset {
            name: "Full".into(),
            target: PresetTargetKind::Fullscreen,
            delay_ms: 0,
            include_cursor: true,
            width: 0,
            height: 0,
            hotkey: "PrintScreen".into(),
            post_action: PostAction::SaveOnly,
            filename_template: "shot-{timestamp}".into(),
            subfolder: "".into(),
        };
        let back = roundtrip(&p);
        assert_eq!(back.name, "Full");
        assert_eq!(back.target, PresetTargetKind::Fullscreen);
        assert!(back.include_cursor);
        assert_eq!(back.hotkey, "PrintScreen");
    }

    #[test]
    fn preset_roundtrip_region_with_delay() {
        let p = Preset::region_3s_default();
        let back = roundtrip(&p);
        assert_eq!(back.target, PresetTargetKind::Region);
        assert_eq!(back.delay_ms, 3_000);
        assert!(!back.include_cursor);
    }

    #[test]
    fn region_3s_default_serialises_expected_shape() {
        let p = Preset::region_3s_default();
        let body = toml::to_string_pretty(&p).unwrap();
        // Flat fields, no nested tables — should stay hand-editable.
        assert!(body.contains("name = \"Region + 3s delay\""));
        assert!(body.contains("target = \"region\""));
        assert!(body.contains("delay_ms = 3000"));
        assert!(body.contains("include_cursor = false"));
        assert!(body.contains("post_action = \"save-only\""));
        // Optional fields skipped when empty/zero keep the file terse.
        assert!(!body.contains("width = 0"));
        assert!(!body.contains("hotkey = \"\""));
    }

    #[test]
    fn preset_roundtrip_window_post_editor() {
        let p = Preset {
            name: "Window → editor".into(),
            target: PresetTargetKind::Window,
            delay_ms: 0,
            include_cursor: true,
            width: 0,
            height: 0,
            hotkey: "Ctrl+Shift+W".into(),
            post_action: PostAction::Editor,
            filename_template: "win-{window}-{timestamp}".into(),
            subfolder: "windows".into(),
        };
        let back = roundtrip(&p);
        assert_eq!(back.target, PresetTargetKind::Window);
        assert_eq!(back.post_action, PostAction::Editor);
        assert_eq!(back.subfolder, "windows");
    }

    #[test]
    fn preset_roundtrip_object() {
        let p = Preset {
            name: "Menu item".into(),
            target: PresetTargetKind::Object,
            delay_ms: 0,
            include_cursor: false,
            width: 0,
            height: 0,
            hotkey: "Ctrl+Shift+O".into(),
            post_action: PostAction::Editor,
            filename_template: "obj-{timestamp}".into(),
            subfolder: "".into(),
        };
        let back = roundtrip(&p);
        assert_eq!(back.target, PresetTargetKind::Object);
        assert_eq!(back.hotkey, "Ctrl+Shift+O");
        // The lowercase-kebab serialisation should read as "object".
        let body = toml::to_string_pretty(&p).unwrap();
        assert!(body.contains("target = \"object\""), "body: {body}");
    }

    #[test]
    fn preset_all_list_contains_new_variants() {
        assert!(PresetTargetKind::ALL.contains(&PresetTargetKind::Object));
    }

    #[test]
    fn preset_roundtrip_exact_dims() {
        let p = Preset {
            name: "FHD".into(),
            target: PresetTargetKind::ExactDims,
            delay_ms: 500,
            include_cursor: false,
            width: 1920,
            height: 1080,
            hotkey: "Ctrl+Alt+1".into(),
            post_action: PostAction::CopyOnly,
            filename_template: "frame-{timestamp}".into(),
            subfolder: "".into(),
        };
        let back = roundtrip(&p);
        assert_eq!(back.target, PresetTargetKind::ExactDims);
        assert_eq!(back.width, 1920);
        assert_eq!(back.height, 1080);
        assert_eq!(back.post_action, PostAction::CopyOnly);
    }

    #[test]
    fn slug_strips_punctuation() {
        let p = Preset { name: "Region + 3s delay!".into(), ..Preset::default() };
        assert_eq!(p.slug(), "region-3s-delay");
    }

    #[test]
    fn slug_fallback_when_empty() {
        let p = Preset { name: "!!!".into(), ..Preset::default() };
        assert_eq!(p.slug(), "preset");
    }

    #[test]
    fn template_renders_tokens() {
        assert_eq!(
            render_template("GrabIt-{timestamp}-{window}", "20260101-120000", "Notepad"),
            "GrabIt-20260101-120000-Notepad"
        );
    }

    #[test]
    fn sanitize_drops_illegal_chars() {
        assert_eq!(sanitize_path_component("a:b/c*d"), "a_b_c_d");
        assert_eq!(sanitize_path_component("..."), "capture");
    }

    #[test]
    fn empty_template_is_sanitized() {
        let p = Preset { filename_template: "".into(), ..Preset::default() };
        let stem = render_template(&p.filename_template, "x", "y");
        assert_eq!(sanitize_path_component(&stem), "capture");
    }
}
