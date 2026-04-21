//! Application settings — loaded/saved as TOML under
//! `%APPDATA%\GrabIt\settings.toml`. Presets (feature #3) live alongside in
//! `presets/*.toml` and get their own module when M5 lands.

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
    /// Global hotkey for `CaptureAndAnnotate`. Default: Ctrl+Z.
    ///
    /// Heads-up: global hotkeys win over focused apps, so while this is set
    /// to Ctrl+Z, that keystroke won't reach other apps (including the
    /// editor's own Undo). Change this in `settings.toml` if you need Ctrl+Z
    /// back — something like `Ctrl+Shift+Z` or `Win+S` is a safer default in
    /// practice.
    pub annotate_hotkey: HotkeyBinding,
    /// Persisted state of the "Launch at startup" tray checkbox.
    pub launch_at_startup: bool,
    /// Include the cursor in captures (as a separate layer).
    pub include_cursor: bool,
    /// Copy every capture to the Windows clipboard on completion.
    pub copy_to_clipboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkey: HotkeyBinding::default(),
            annotate_hotkey: HotkeyBinding { raw: "Ctrl+Z".to_string() },
            launch_at_startup: true,
            include_cursor: true,
            copy_to_clipboard: true,
        }
    }
}

impl Settings {
    pub fn load_or_default(paths: &AppPaths) -> Self {
        let path = paths.settings_file();
        match std::fs::read_to_string(&path) {
            Ok(body) => match toml::from_str::<Settings>(&body) {
                Ok(s) => {
                    debug!("loaded settings from {}", path.display());
                    s
                }
                Err(e) => {
                    warn!("settings parse error, using defaults: {e}");
                    Self::default()
                }
            },
            Err(_) => {
                debug!("no settings file; using defaults");
                Self::default()
            }
        }
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let body = toml::to_string_pretty(self).context("serialize settings")?;
        let path = paths.settings_file();
        std::fs::write(&path, body)
            .with_context(|| format!("write {}", path.display()))?;
        debug!("saved settings to {}", path.display());
        Ok(())
    }
}
