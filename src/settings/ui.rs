//! Settings window — eframe form that edits `%APPDATA%\GrabIt\settings.json`
//! and signals the tray to reload via a `.settings_refresh` marker file.
//!
//! Hotkey fields use a click-to-record flow: click the field, press the
//! desired combo, then Confirm (or Esc to cancel). The captured chord is
//! formatted the same way `parse_chord` expects, so there's no divergence
//! between what the UI shows and what the tray registers.

use crate::app::paths::AppPaths;
use crate::hotkeys::bindings::parse_chord;
use crate::settings::Settings;
use anyhow::Result;
use eframe::egui;

pub fn run_blocking(paths: AppPaths, initial: Settings) -> Result<()> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("GrabIt settings")
        .with_inner_size([620.0, 520.0])
        .with_min_inner_size([560.0, 460.0])
        .with_resizable(true);

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        "GrabIt settings",
        options,
        Box::new(move |_cc| Ok(Box::new(SettingsApp::new(initial, paths)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

/// Which hotkey field is currently in "press a combo" capture mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordTarget {
    Fullscreen,
    Annotate,
}

struct SettingsApp {
    settings: Settings,
    paths: AppPaths,
    hotkey_buf: String,
    annotate_hotkey_buf: String,
    output_dir_buf: String,
    default_output_dir: String,
    status: String,
    /// Hotkey field currently in capture mode, if any.
    recording: Option<RecordTarget>,
    /// Last captured chord string while in capture mode. Updates on every
    /// new key press; Confirm commits it into the underlying buffer.
    captured: Option<String>,
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
            recording: None,
            captured: None,
        }
    }

    /// Poll egui's per-frame input events while in capture mode and pull
    /// out the most recent non-Escape key press. Returns `true` if the
    /// caller should treat the recording as cancelled (Esc pressed).
    fn pump_capture(&mut self, ctx: &egui::Context) -> bool {
        if self.recording.is_none() {
            return false;
        }
        let mut cancel = false;
        let mut new_capture: Option<String> = None;
        ctx.input(|i| {
            // Read the modifier state once per frame rather than trusting
            // each `Event::Key`'s own `modifiers` field. In some egui event
            // orderings the per-event modifiers briefly lag the real state
            // (e.g. the Z in Ctrl+Z can arrive before the Ctrl-down event
            // has updated the event's own snapshot), which would capture
            // the chord as "Z" instead of "Ctrl+Z". Using the frame-level
            // state avoids that race entirely.
            let live_mods = i.modifiers;
            for ev in &i.events {
                if let egui::Event::Key { key, pressed: true, .. } = ev {
                    if *key == egui::Key::Escape {
                        cancel = true;
                        continue;
                    }
                    if let Some(chord) = format_captured_chord(live_mods, *key) {
                        new_capture = Some(chord);
                    }
                }
            }
        });
        if let Some(c) = new_capture {
            self.captured = Some(c);
        }
        cancel
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Listen for key events while recording. If Esc, cancel.
        if self.pump_capture(ctx) {
            self.recording = None;
            self.captured = None;
        }

        // Credit footer pinned to the bottom-right.
        egui::TopBottomPanel::bottom("settings-footer")
            .resizable(false)
            .show_separator_line(false)
            .show(ctx, |ui| {
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new("GrabIt 2026 - Matthew Fay")
                                .small()
                                .color(egui::Color32::GRAY),
                        );
                    },
                );
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Settings");
            ui.add_space(10.0);

            // ── Hotkeys section ────────────────────────────────────────
            section_header(ui, "Hotkeys");
            ui.label(
                egui::RichText::new(
                    "Click a field and press the key combo you want, then Confirm.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );
            ui.add_space(4.0);
            self.hotkey_row(ui, "Fullscreen capture", RecordTarget::Fullscreen);
            self.hotkey_row(ui, "Annotate", RecordTarget::Annotate);

            section_break(ui);

            // ── Capture section ────────────────────────────────────────
            section_header(ui, "Capture");
            egui::Grid::new("capture-grid")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
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
                                .hint_text(format!(
                                    "default: {}",
                                    self.default_output_dir
                                ))
                                .desired_width(260.0),
                        );
                        if ui.button("Browse\u{2026}").clicked() {
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

            section_break(ui);

            // ── Arrows section ─────────────────────────────────────────
            section_header(ui, "Arrows");
            egui::Grid::new("arrows-grid")
                .num_columns(2)
                .spacing([16.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Default new arrows to drop shadow");
                    ui.checkbox(&mut self.settings.arrow_shadow, "");
                    ui.end_row();

                    ui.label("Advanced color mode (picker + hex)");
                    ui.checkbox(&mut self.settings.arrow_advanced_color, "");
                    ui.end_row();
                });
            ui.label(
                egui::RichText::new(
                    "Tip: hold Shift while dragging an arrow to snap its angle to 15\u{00B0}.",
                )
                .small()
                .color(egui::Color32::GRAY),
            );

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
                // Put Reset at the right so it's not in the primary
                // save/cancel flow — avoids accidental clicks.
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.button("Reset to defaults").clicked() {
                            self.reset_to_defaults();
                        }
                    },
                );
            });
        });

        // While recording, force continuous repaint so newly pressed keys
        // show up in the capture label without user-driven repaints.
        if self.recording.is_some() {
            ctx.request_repaint();
        }
    }
}

impl SettingsApp {
    /// Wipe the in-memory form back to factory defaults. Doesn't touch
    /// disk — user still needs to click Save to persist. Cancel reverts.
    fn reset_to_defaults(&mut self) {
        let fresh = Settings::default();
        self.hotkey_buf = fresh.hotkey.raw.clone();
        self.annotate_hotkey_buf = fresh.annotate_hotkey.raw.clone();
        self.output_dir_buf.clear(); // empty string → use default output dir
        self.settings = fresh;
        self.recording = None;
        self.captured = None;
        self.status = "Reset — click Save to apply, Cancel to discard.".into();
    }

    /// A single hotkey row. When not recording, shows the current chord as
    /// a clickable button (click = enter record mode). When recording,
    /// shows a live "Press combo\u{2026}" / "Captured: …" label plus
    /// Confirm / Cancel buttons. Esc at any point cancels.
    fn hotkey_row(&mut self, ui: &mut egui::Ui, label: &str, target: RecordTarget) {
        let is_recording = self.recording == Some(target);
        ui.horizontal(|ui| {
            ui.add_sized([200.0, 24.0], egui::Label::new(label));
            if is_recording {
                let display = match &self.captured {
                    Some(c) => format!("Captured: {c}"),
                    None => "Press combo\u{2026}".to_string(),
                };
                let color = if self.captured.is_some() {
                    egui::Color32::from_rgb(90, 200, 120)
                } else {
                    egui::Color32::from_rgb(220, 180, 60)
                };
                ui.add_sized(
                    [220.0, 24.0],
                    egui::Label::new(egui::RichText::new(display).color(color)),
                );
                let confirm_enabled = self.captured.is_some();
                if ui
                    .add_enabled(
                        confirm_enabled,
                        egui::Button::new("Confirm"),
                    )
                    .clicked()
                {
                    if let Some(c) = self.captured.take() {
                        match target {
                            RecordTarget::Fullscreen => self.hotkey_buf = c,
                            RecordTarget::Annotate => self.annotate_hotkey_buf = c,
                        }
                    }
                    self.recording = None;
                }
                if ui.button("Cancel").clicked() {
                    self.recording = None;
                    self.captured = None;
                }
            } else {
                let current = match target {
                    RecordTarget::Fullscreen => self.hotkey_buf.clone(),
                    RecordTarget::Annotate => self.annotate_hotkey_buf.clone(),
                };
                if ui
                    .add_sized(
                        [220.0, 24.0],
                        egui::Button::new(egui::RichText::new(current).monospace()),
                    )
                    .clicked()
                {
                    self.recording = Some(target);
                    self.captured = None;
                    // Drop any keyboard focus so the next keystroke lands
                    // in our capture loop rather than a text edit.
                    ui.ctx().memory_mut(|m| m.stop_text_input());
                }
            }
        });
    }
}

fn section_header(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).strong().size(15.0));
    ui.add_space(4.0);
}

fn section_break(ui: &mut egui::Ui) {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(8.0);
}

/// Translate an egui key press (with modifiers) into the chord-string
/// format that `parse_chord` expects (e.g. `"Ctrl+Shift+X"`). Returns
/// `None` for keys we don't bind (modifiers-only presses, media keys, etc.).
fn format_captured_chord(mods: egui::Modifiers, key: egui::Key) -> Option<String> {
    let token = egui_key_token(key)?;
    let mut out = String::new();
    if mods.ctrl { out.push_str("Ctrl+"); }
    if mods.shift { out.push_str("Shift+"); }
    if mods.alt { out.push_str("Alt+"); }
    out.push_str(token);
    Some(out)
}

fn egui_key_token(k: egui::Key) -> Option<&'static str> {
    use egui::Key;
    Some(match k {
        Key::A => "A", Key::B => "B", Key::C => "C", Key::D => "D",
        Key::E => "E", Key::F => "F", Key::G => "G", Key::H => "H",
        Key::I => "I", Key::J => "J", Key::K => "K", Key::L => "L",
        Key::M => "M", Key::N => "N", Key::O => "O", Key::P => "P",
        Key::Q => "Q", Key::R => "R", Key::S => "S", Key::T => "T",
        Key::U => "U", Key::V => "V", Key::W => "W", Key::X => "X",
        Key::Y => "Y", Key::Z => "Z",
        Key::Num0 => "0", Key::Num1 => "1", Key::Num2 => "2",
        Key::Num3 => "3", Key::Num4 => "4", Key::Num5 => "5",
        Key::Num6 => "6", Key::Num7 => "7", Key::Num8 => "8",
        Key::Num9 => "9",
        Key::F1 => "F1", Key::F2 => "F2", Key::F3 => "F3", Key::F4 => "F4",
        Key::F5 => "F5", Key::F6 => "F6", Key::F7 => "F7", Key::F8 => "F8",
        Key::F9 => "F9", Key::F10 => "F10", Key::F11 => "F11", Key::F12 => "F12",
        Key::Space => "Space",
        Key::Enter => "Enter",
        Key::Tab => "Tab",
        Key::Backspace => "Backspace",
        Key::Delete => "Delete",
        Key::Insert => "Insert",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        Key::ArrowLeft => "Left",
        Key::ArrowRight => "Right",
        _ => return None,
    })
}
