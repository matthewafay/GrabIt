//! Application settings — loaded/saved as pretty JSON under
//! `%APPDATA%\GrabIt\settings.json`. Legacy installs that wrote
//! `settings.toml` are read once on first launch and re-persisted as JSON on
//! the next save. Presets (feature #3) live alongside in `presets/*.toml` and
//! get their own module when M5 lands.

pub mod ui;

use crate::app::paths::AppPaths;
use crate::hotkeys::bindings::HotkeyBinding;
use anyhow::{Context, Result};
use log::{debug, warn};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Global hotkey for `CaptureFullscreen`. Default: PrintScreen.
    pub hotkey: HotkeyBinding,
    /// Global hotkey for `CaptureAndAnnotate`. Default: Ctrl+X.
    ///
    /// Heads-up: global hotkeys win over focused apps, so while this is set
    /// to Ctrl+X, that keystroke won't reach other apps (including their
    /// own Cut shortcut). Change it in Settings or edit `settings.json`.
    pub annotate_hotkey: HotkeyBinding,
    /// Persisted state of the "Launch at startup" tray checkbox.
    pub launch_at_startup: bool,
    /// Include the cursor in captures (as a separate layer).
    pub include_cursor: bool,
    /// Copy every capture to the Windows clipboard on completion.
    pub copy_to_clipboard: bool,
    /// Override the capture output directory. `None` = default
    /// `%USERPROFILE%\Pictures\GrabIt`.
    pub output_dir: Option<String>,
    /// New arrows default to shadow = true when this is on. Per-arrow
    /// `shadow` field still rules the final render — this only seeds the
    /// default at creation time.
    pub arrow_shadow: bool,
    /// When false (default), the Arrow tool shows an 8-swatch palette.
    /// When true, it shows a full color picker plus a hex input field.
    pub arrow_advanced_color: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: HotkeyBinding::default(),
            annotate_hotkey: HotkeyBinding { raw: "Ctrl+X".to_string() },
            launch_at_startup: true,
            include_cursor: true,
            copy_to_clipboard: true,
            output_dir: None,
            arrow_shadow: true,
            arrow_advanced_color: false,
        }
    }
}

impl Settings {
    pub fn load_or_default(paths: &AppPaths) -> Self {
        let json_path = paths.settings_file();
        if let Ok(body) = std::fs::read_to_string(&json_path) {
            return match serde_json::from_str::<Settings>(&body) {
                Ok(s) => {
                    debug!("loaded settings from {}", json_path.display());
                    s
                }
                Err(e) => {
                    warn!("settings parse error ({}), using defaults: {e}", json_path.display());
                    Self::default()
                }
            };
        }

        // First launch after upgrade: no JSON file yet. Migrate the legacy
        // settings.toml if it exists; on next save we write settings.json.
        let toml_path = paths.legacy_settings_file();
        if let Ok(body) = std::fs::read_to_string(&toml_path) {
            match toml::from_str::<Settings>(&body) {
                Ok(s) => {
                    debug!("migrated legacy settings from {}", toml_path.display());
                    return s;
                }
                Err(e) => {
                    warn!("legacy settings parse error, using defaults: {e}");
                }
            }
        }

        debug!("no settings file; using defaults");
        Self::default()
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let body = serde_json::to_string_pretty(self).context("serialize settings")?;
        let path = paths.settings_file();
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
        debug!("saved settings to {}", path.display());
        Ok(())
    }
}
