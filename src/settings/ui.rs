//! Settings window — eframe form that edits `%APPDATA%\GrabIt\settings.toml`
//! and signals the tray to reload via a `.settings_refresh` marker file.

use crate::app::paths::AppPaths;
use crate::hotkeys::bindings::parse_chord;
use crate::settings::Settings;
use anyhow::Result;
use eframe::egui;

pub fn run_blocking(paths: AppPaths, initial: Settings) -> Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("GrabIt settings")
        .with_inner_size([620.0, 420.0])
        .with_resizable(false);

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "GrabIt settings",
        options,
        Box::new(move |_cc| Ok(Box::new(SettingsApp::new(initial, paths)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

struct SettingsApp {
    settings: Settings,
    paths: AppPaths,
    hotkey_buf: String,
    annotate_hotkey_buf: String,
    output_dir_buf: String,
    default_output_dir: String,
    status: String,
}

impl SettingsApp {
    fn new(settings: Settings, paths: AppPaths) -> Self {
        let hotkey_buf = settings.hotkey.raw.clone();
        let annotate_hotkey_buf = settings.annotate_hotkey.raw.clone();
        let output_dir_buf = settings.output_dir.clone().unwrap_or_default();
        let default_output_dir = paths.output_dir.display().to_string();
        Self {
            settings,
            paths,
            hotkey_buf,
            annotate_hotkey_buf,
            output_dir_buf,
            default_output_dir,
            status: String::new(),
        }
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Settings");
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(
                    "Hotkey chords: modifiers joined with + (e.g. Ctrl+Shift+Z, PrintScreen, Win+S).",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
            ui.add_space(10.0);

            egui::Grid::new("settings-grid")
                .num_columns(2)
                .spacing([16.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Fullscreen capture hotkey");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.hotkey_buf)
                            .desired_width(220.0),
                    );
                    ui.end_row();

                    ui.label("Annotate hotkey");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.annotate_hotkey_buf)
                            .desired_width(220.0),
                    );
                    ui.end_row();

                    ui.label("Launch at startup");
                    ui.checkbox(&mut self.settings.launch_at_startup, "");
                    ui.end_row();

                    ui.label("Include cursor in captures");
                    ui.checkbox(&mut self.settings.include_cursor, "");
                    ui.end_row();

                    ui.label("Copy every capture to clipboard");
                    ui.checkbox(&mut self.settings.copy_to_clipboard, "");
                    ui.end_row();

                    ui.label("Output folder");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.output_dir_buf)
                                .hint_text(format!("default: {}", self.default_output_dir))
                                .desired_width(280.0),
                        );
                        if ui.button("Browse…").clicked() {
                            let mut dlg = rfd::FileDialog::new();
                            if !self.output_dir_buf.trim().is_empty() {
                                dlg = dlg.set_directory(self.output_dir_buf.trim());
                            }
                            if let Some(folder) = dlg.pick_folder() {
                                self.output_dir_buf = folder.display().to_string();
                            }
                        }
                        if ui.button("Reset").clicked() {
                            self.output_dir_buf.clear();
                        }
                    });
                    ui.end_row();
                });

            ui.add_space(14.0);
            if !self.status.is_empty() {
                ui.label(
                    egui::RichText::new(&self.status)
                        .color(egui::Color32::from_rgb(220, 80, 80)),
                );
                ui.add_space(4.0);
            }

            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    if let Err(e) = parse_chord(&self.hotkey_buf) {
                        self.status = format!("Fullscreen hotkey invalid: {e}");
                        return;
                    }
                    if let Err(e) = parse_chord(&self.annotate_hotkey_buf) {
                        self.status = format!("Annotate hotkey invalid: {e}");
                        return;
                    }
                    self.settings.hotkey.raw = self.hotkey_buf.clone();
                    self.settings.annotate_hotkey.raw = self.annotate_hotkey_buf.clone();

                    let trimmed = self.output_dir_buf.trim();
                    if trimmed.is_empty() {
                        self.settings.output_dir = None;
                    } else {
                        let candidate = std::path::PathBuf::from(trimmed);
                        if let Err(e) = std::fs::create_dir_all(&candidate) {
                            self.status = format!("Output folder unusable: {e}");
                            return;
                        }
                        self.settings.output_dir = Some(trimmed.to_string());
                    }

                    if let Err(e) = self.settings.save(&self.paths) {
                        self.status = format!("Save failed: {e}");
                        return;
                    }
                    let marker = self.paths.data_dir.join(".settings_refresh");
                    let _ = std::fs::write(&marker, "");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("Cancel").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
    }
}
