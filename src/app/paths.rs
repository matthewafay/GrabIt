use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolved on-disk locations used by GrabIt. Created at startup; callers
/// can assume the directories already exist after `bootstrap()`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // presets/stamps/logs dirs are bootstrapped for later milestones.
pub struct AppPaths {
    /// `%APPDATA%\GrabIt`
    pub data_dir: PathBuf,
    /// `%APPDATA%\GrabIt\presets`
    pub presets_dir: PathBuf,
    /// `%APPDATA%\GrabIt\stamps`
    pub stamps_dir: PathBuf,
    /// `%APPDATA%\GrabIt\logs`
    pub logs_dir: PathBuf,
    /// `%USERPROFILE%\Pictures\GrabIt`
    pub output_dir: PathBuf,
}

#[allow(dead_code)] // log_file/preset_file/data_dir are used by later milestones.
impl AppPaths {
    pub fn bootstrap() -> Result<Self> {
        let data_dir = dirs::config_dir()
            .context("resolve %APPDATA%")?
            .join("GrabIt");
        let presets_dir = data_dir.join("presets");
        let stamps_dir = data_dir.join("stamps");
        let logs_dir = data_dir.join("logs");

        let output_dir = dirs::picture_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Pictures")))
            .context("resolve Pictures folder")?
            .join("GrabIt");

        for p in [&data_dir, &presets_dir, &stamps_dir, &logs_dir, &output_dir] {
            std::fs::create_dir_all(p)
                .with_context(|| format!("create {}", p.display()))?;
        }

        Ok(Self { data_dir, presets_dir, stamps_dir, logs_dir, output_dir })
    }

    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }

    /// One-shot migration path: pre-JSON installs wrote `settings.toml`. On
    /// load we read this only if the JSON file doesn't exist yet, then the
    /// next save rewrites as JSON.
    pub fn legacy_settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.toml")
    }

    pub fn log_file(&self) -> PathBuf {
        self.logs_dir.join("grabit.log")
    }

    pub fn preset_file(&self, name: &str) -> PathBuf {
        self.presets_dir.join(format!("{name}.toml"))
    }

    pub fn default_capture_filename(&self, ext: &str) -> PathBuf {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        self.output_dir.join(format!("GrabIt-{stamp}.{ext}"))
    }

    /// Spool directory for in-progress GIF recordings. Created lazily on
    /// first use; each recording gets its own UUID-named subdirectory under
    /// here so concurrent runs (or a crash mid-recording) don't collide.
    pub fn gif_temp_dir(&self) -> PathBuf {
        self.data_dir.join("gif-record")
    }

    /// Default filename for an exported GIF. Timestamped like the regular
    /// PNG output so multiple exports in the same minute don't overwrite.
    pub fn default_gif_filename(&self) -> PathBuf {
        self.default_capture_filename("gif")
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
}
