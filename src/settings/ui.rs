//! Settings window — Dioxus port of the form that edits
//! `%APPDATA%\GrabIt\settings.json` and signals the tray to reload via a
//! `.settings_refresh` marker file.
//!
//! Sections (matches `Settings` field order so the form is easy to scan
//! against the struct):
//!
//! - **Hotkeys** — three click-to-record fields for the global capture
//!   chords (fullscreen / annotate / GIF). The capture flow uses a
//!   real keyboard listener via `onkeydown` on the chord button while
//!   recording is active; Esc cancels, any other key + the live
//!   modifier state commits a chord string in `parse_chord`'s format.
//! - **Capture** — launch_at_startup, include_cursor, copy_to_clipboard,
//!   output_dir (with Browse + Reset).
//! - **Arrows** — the two arrow defaults.
//! - **GIF** — fps / loop / max-seconds / gif_record_cursor.
//!
//! Save validates every chord, persists the settings, drops the
//! `.settings_refresh` marker, and closes the window. The tray's main
//! loop picks up the marker on its next poll and re-registers hotkeys
//! plus refreshes the menu accelerators in place.

use crate::app::paths::AppPaths;
use crate::hotkeys::bindings::parse_chord;
use crate::settings::Settings;
use anyhow::Result;
use dioxus::desktop::{tao::window::WindowBuilder, Config, LogicalSize};
use dioxus::events::{Key, Modifiers};
use dioxus::prelude::*;
use log::warn;

const STYLES: &str = include_str!("ui.css");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordTarget {
    Fullscreen,
    Annotate,
    Gif,
}

#[derive(Clone)]
struct InitialState {
    paths: AppPaths,
    settings: Settings,
}

pub fn run_blocking(paths: AppPaths, initial: Settings) -> Result<()> {
    let cfg = Config::new().with_window(
        WindowBuilder::new()
            .with_title("GrabIt — Settings")
            .with_inner_size(LogicalSize::new(640.0, 620.0))
            .with_min_inner_size(LogicalSize::new(560.0, 480.0)),
    );

    dioxus::LaunchBuilder::desktop()
        .with_cfg(cfg)
        .with_context(InitialState {
            paths,
            settings: initial,
        })
        .launch(settings_app);

    Ok(())
}

#[component]
fn settings_app() -> Element {
    let initial = use_context::<InitialState>();

    // Per-field signals — finer-grained reactivity than holding the
    // whole `Settings` struct in one signal, and Save only has to
    // assemble a fresh Settings from the current values.
    let hotkey_buf = use_signal(|| initial.settings.hotkey.raw.clone());
    let annotate_hotkey_buf = use_signal(|| initial.settings.annotate_hotkey.raw.clone());
    let gif_hotkey_buf = use_signal(|| initial.settings.gif_hotkey.raw.clone());

    let launch_at_startup = use_signal(|| initial.settings.launch_at_startup);
    let include_cursor = use_signal(|| initial.settings.include_cursor);
    let copy_to_clipboard = use_signal(|| initial.settings.copy_to_clipboard);
    let output_dir_buf = use_signal(|| initial.settings.output_dir.clone().unwrap_or_default());

    let arrow_shadow = use_signal(|| initial.settings.arrow_shadow);
    let arrow_advanced_color = use_signal(|| initial.settings.arrow_advanced_color);

    let gif_fps = use_signal(|| initial.settings.gif_fps);
    let gif_loop_count = use_signal(|| initial.settings.gif_loop_count);
    let gif_max_seconds = use_signal(|| initial.settings.gif_max_seconds);
    let gif_record_cursor = use_signal(|| initial.settings.gif_record_cursor);

    let recording = use_signal(|| Option::<RecordTarget>::None);
    let captured = use_signal(|| Option::<String>::None);
    let status = use_signal(|| String::new());

    let default_output_dir = initial.paths.output_dir.display().to_string();

    rsx! {
        style { "{STYLES}" }
        div { class: "app",
            header { class: "toolbar",
                h1 { "Settings" }
                p { "Hotkeys, capture defaults, output location, GIF recording" }
            }

            main { class: "main",
                Hotkeys {
                    hotkey_buf: hotkey_buf,
                    annotate_hotkey_buf: annotate_hotkey_buf,
                    gif_hotkey_buf: gif_hotkey_buf,
                    recording: recording,
                    captured: captured,
                }
                CaptureSection {
                    launch_at_startup: launch_at_startup,
                    include_cursor: include_cursor,
                    copy_to_clipboard: copy_to_clipboard,
                    output_dir_buf: output_dir_buf,
                    default_output_dir: default_output_dir.clone(),
                }
                ArrowsSection {
                    arrow_shadow: arrow_shadow,
                    arrow_advanced_color: arrow_advanced_color,
                }
                GifSection {
                    gif_fps: gif_fps,
                    gif_loop_count: gif_loop_count,
                    gif_max_seconds: gif_max_seconds,
                    gif_record_cursor: gif_record_cursor,
                }
            }

            FooterBar {
                hotkey_buf: hotkey_buf,
                annotate_hotkey_buf: annotate_hotkey_buf,
                gif_hotkey_buf: gif_hotkey_buf,
                launch_at_startup: launch_at_startup,
                include_cursor: include_cursor,
                copy_to_clipboard: copy_to_clipboard,
                output_dir_buf: output_dir_buf,
                arrow_shadow: arrow_shadow,
                arrow_advanced_color: arrow_advanced_color,
                gif_fps: gif_fps,
                gif_loop_count: gif_loop_count,
                gif_max_seconds: gif_max_seconds,
                gif_record_cursor: gif_record_cursor,
                status: status,
            }
        }
    }
}

#[component]
fn Hotkeys(
    hotkey_buf: Signal<String>,
    annotate_hotkey_buf: Signal<String>,
    gif_hotkey_buf: Signal<String>,
    recording: Signal<Option<RecordTarget>>,
    captured: Signal<Option<String>>,
) -> Element {
    rsx! {
        section { class: "section",
            h2 { "Hotkeys" }
            p { class: "section-hint",
                "Click a field and press the combo you want, then Confirm. Esc cancels."
            }
            HotkeyRow {
                label: "Fullscreen capture".to_string(),
                target: RecordTarget::Fullscreen,
                buf: hotkey_buf,
                recording: recording,
                captured: captured,
            }
            HotkeyRow {
                label: "Annotate".to_string(),
                target: RecordTarget::Annotate,
                buf: annotate_hotkey_buf,
                recording: recording,
                captured: captured,
            }
            HotkeyRow {
                label: "Record GIF".to_string(),
                target: RecordTarget::Gif,
                buf: gif_hotkey_buf,
                recording: recording,
                captured: captured,
            }
        }
    }
}

#[component]
fn HotkeyRow(
    label: String,
    target: RecordTarget,
    buf: Signal<String>,
    recording: Signal<Option<RecordTarget>>,
    captured: Signal<Option<String>>,
) -> Element {
    let is_recording = *recording.read() == Some(target);

    let on_record_click = move |_| {
        recording.set(Some(target));
        captured.set(None);
    };

    // The capture handler reads the live modifier state from the event
    // (Dioxus exposes both a Key enum and a Modifiers struct). Esc
    // cancels; any other key + the current modifier set commits a chord
    // string in `parse_chord`'s expected format.
    let on_keydown = move |evt: KeyboardEvent| {
        if *recording.read() != Some(target) {
            return;
        }
        let key = evt.key();
        if matches!(key, Key::Escape) {
            recording.set(None);
            captured.set(None);
            return;
        }
        if let Some(chord) = format_chord(&key, &evt.modifiers()) {
            captured.set(Some(chord));
        }
    };

    let on_confirm = move |_| {
        if let Some(c) = captured.read().clone() {
            buf.set(c);
        }
        recording.set(None);
        captured.set(None);
    };

    let on_cancel = move |_| {
        recording.set(None);
        captured.set(None);
    };

    let chord_text = buf.read().clone();
    let captured_display = captured.read().clone();

    rsx! {
        div { class: "row",
            div { class: "label", "{label}" }
            div { class: "control",
                if is_recording {
                    div {
                        class: if captured_display.is_some() { "chord-recording captured" } else { "chord-recording" },
                        // tabindex + autofocus so this div actually
                        // receives keydown events while recording.
                        tabindex: "0",
                        autofocus: true,
                        onkeydown: on_keydown,
                        span { class: "text",
                            {if let Some(ref c) = captured_display { format!("Captured: {c}") } else { "Press combo…".to_string() }}
                        }
                        button {
                            class: "primary",
                            disabled: captured_display.is_none(),
                            onclick: on_confirm,
                            "Confirm"
                        }
                        button {
                            class: "ghost",
                            onclick: on_cancel,
                            "Cancel"
                        }
                    }
                } else {
                    button {
                        class: "chord-button",
                        onclick: on_record_click,
                        "{chord_text}"
                    }
                }
            }
        }
    }
}

#[component]
fn CaptureSection(
    launch_at_startup: Signal<bool>,
    include_cursor: Signal<bool>,
    copy_to_clipboard: Signal<bool>,
    output_dir_buf: Signal<String>,
    default_output_dir: String,
) -> Element {
    rsx! {
        section { class: "section",
            h2 { "Capture" }
            ToggleRow {
                label: "Launch at startup".to_string(),
                value: launch_at_startup,
            }
            ToggleRow {
                label: "Include cursor in captures".to_string(),
                value: include_cursor,
            }
            ToggleRow {
                label: "Copy every capture to clipboard".to_string(),
                value: copy_to_clipboard,
            }
            div { class: "row",
                div { class: "label", "Output folder" }
                div { class: "control path-row",
                    input {
                        r#type: "text",
                        value: "{output_dir_buf}",
                        placeholder: "default: {default_output_dir}",
                        oninput: move |evt| output_dir_buf.set(evt.value()),
                    }
                    button {
                        class: "ghost",
                        onclick: move |_| {
                            // rfd::pick_folder is blocking; the brief
                            // freeze is the same behavior as the old
                            // eframe path. Picks open at the current
                            // path if set, otherwise OS default.
                            let mut dlg = rfd::FileDialog::new();
                            let cur = output_dir_buf.read().trim().to_string();
                            if !cur.is_empty() {
                                dlg = dlg.set_directory(&cur);
                            }
                            if let Some(folder) = dlg.pick_folder() {
                                output_dir_buf.set(folder.display().to_string());
                            }
                        },
                        "Browse…"
                    }
                    button {
                        class: "ghost",
                        onclick: move |_| output_dir_buf.set(String::new()),
                        "Reset"
                    }
                }
            }
        }
    }
}

#[component]
fn ArrowsSection(arrow_shadow: Signal<bool>, arrow_advanced_color: Signal<bool>) -> Element {
    rsx! {
        section { class: "section",
            h2 { "Arrows" }
            ToggleRow {
                label: "Default new arrows to drop shadow".to_string(),
                value: arrow_shadow,
            }
            ToggleRow {
                label: "Advanced color mode (picker + hex)".to_string(),
                value: arrow_advanced_color,
            }
            p { class: "section-hint",
                "Tip: hold Shift while dragging an arrow to snap its angle to 15°."
            }
        }
    }
}

#[component]
fn GifSection(
    gif_fps: Signal<u32>,
    gif_loop_count: Signal<u16>,
    gif_max_seconds: Signal<u32>,
    gif_record_cursor: Signal<bool>,
) -> Element {
    rsx! {
        section { class: "section",
            h2 { "GIF" }
            NumberRow {
                label: "Frames per second".to_string(),
                suffix: "fps".to_string(),
                value: gif_fps,
                min: 5,
                max: 60,
            }
            U16NumberRow {
                label: "Loop count (0 = infinite)".to_string(),
                suffix: String::new(),
                value: gif_loop_count,
                min: 0,
                max: 10_000,
            }
            NumberRow {
                label: "Max recording length".to_string(),
                suffix: "s".to_string(),
                value: gif_max_seconds,
                min: 1,
                max: 600,
            }
            ToggleRow {
                label: "Include cursor in GIF frames".to_string(),
                value: gif_record_cursor,
            }
        }
    }
}

/// Standard label + boolean toggle row.
#[component]
fn ToggleRow(label: String, value: Signal<bool>) -> Element {
    let checked = *value.read();
    rsx! {
        div { class: "row",
            div { class: "label", "{label}" }
            div { class: "control",
                label { class: "toggle",
                    input {
                        r#type: "checkbox",
                        checked: "{checked}",
                        onchange: move |evt| value.set(evt.checked()),
                    }
                    span { class: "toggle-thumb" }
                }
            }
        }
    }
}

/// Bounded `u32` number input.
#[component]
fn NumberRow(
    label: String,
    suffix: String,
    value: Signal<u32>,
    min: u32,
    max: u32,
) -> Element {
    let v = *value.read();
    rsx! {
        div { class: "row",
            div { class: "label", "{label}" }
            div { class: "control",
                input {
                    class: "number",
                    r#type: "number",
                    min: "{min}",
                    max: "{max}",
                    value: "{v}",
                    oninput: move |evt| {
                        if let Ok(parsed) = evt.value().parse::<u32>() {
                            value.set(parsed.clamp(min, max));
                        }
                    },
                }
                if !suffix.is_empty() {
                    span { class: "number-suffix", "{suffix}" }
                }
            }
        }
    }
}

/// Bounded `u16` number input — separate component because the signal
/// type matters for `oninput`'s parse.
#[component]
fn U16NumberRow(
    label: String,
    suffix: String,
    value: Signal<u16>,
    min: u16,
    max: u16,
) -> Element {
    let v = *value.read();
    rsx! {
        div { class: "row",
            div { class: "label", "{label}" }
            div { class: "control",
                input {
                    class: "number",
                    r#type: "number",
                    min: "{min}",
                    max: "{max}",
                    value: "{v}",
                    oninput: move |evt| {
                        if let Ok(parsed) = evt.value().parse::<u16>() {
                            value.set(parsed.clamp(min, max));
                        }
                    },
                }
                if !suffix.is_empty() {
                    span { class: "number-suffix", "{suffix}" }
                }
            }
        }
    }
}

#[component]
#[allow(clippy::too_many_arguments)]
fn FooterBar(
    hotkey_buf: Signal<String>,
    annotate_hotkey_buf: Signal<String>,
    gif_hotkey_buf: Signal<String>,
    launch_at_startup: Signal<bool>,
    include_cursor: Signal<bool>,
    copy_to_clipboard: Signal<bool>,
    output_dir_buf: Signal<String>,
    arrow_shadow: Signal<bool>,
    arrow_advanced_color: Signal<bool>,
    gif_fps: Signal<u32>,
    gif_loop_count: Signal<u16>,
    gif_max_seconds: Signal<u32>,
    gif_record_cursor: Signal<bool>,
    status: Signal<String>,
) -> Element {
    let status_text = status.read().clone();
    let is_error = !status_text.is_empty() && !status_text.starts_with("Reset");

    // Pull paths from context rather than a prop — `AppPaths` doesn't
    // implement `PartialEq`, which Dioxus requires for prop memo
    // comparison. Context bypasses that check entirely.
    let paths_for_save = use_context::<InitialState>().paths;
    let on_save = move |_| {
        // Validate every chord first.
        if let Err(e) = parse_chord(hotkey_buf.read().as_str()) {
            status.set(format!("Fullscreen hotkey invalid: {e}"));
            return;
        }
        if let Err(e) = parse_chord(annotate_hotkey_buf.read().as_str()) {
            status.set(format!("Annotate hotkey invalid: {e}"));
            return;
        }
        if let Err(e) = parse_chord(gif_hotkey_buf.read().as_str()) {
            status.set(format!("GIF hotkey invalid: {e}"));
            return;
        }

        // Resolve output_dir; create the directory before persisting so
        // a typo can't leave the user with an unusable override.
        let trimmed = output_dir_buf.read().trim().to_string();
        let output_dir = if trimmed.is_empty() {
            None
        } else {
            if let Err(e) = std::fs::create_dir_all(&trimmed) {
                status.set(format!("Output folder unusable: {e}"));
                return;
            }
            Some(trimmed)
        };

        let mut s = Settings::default();
        s.hotkey.raw = hotkey_buf.read().clone();
        s.annotate_hotkey.raw = annotate_hotkey_buf.read().clone();
        s.gif_hotkey.raw = gif_hotkey_buf.read().clone();
        s.launch_at_startup = *launch_at_startup.read();
        s.include_cursor = *include_cursor.read();
        s.copy_to_clipboard = *copy_to_clipboard.read();
        s.output_dir = output_dir;
        s.arrow_shadow = *arrow_shadow.read();
        s.arrow_advanced_color = *arrow_advanced_color.read();
        s.gif_fps = (*gif_fps.read()).clamp(5, 60);
        s.gif_loop_count = *gif_loop_count.read();
        s.gif_max_seconds = (*gif_max_seconds.read()).max(1);
        s.gif_record_cursor = *gif_record_cursor.read();

        if let Err(e) = s.save(&paths_for_save) {
            status.set(format!("Save failed: {e}"));
            return;
        }
        // Drop the marker; the tray picks this up on its next poll
        // and re-registers hotkeys + refreshes the menu accelerators.
        // fsync the file before closing so the tray's PeekMessageW
        // tick doesn't race the directory entry update — bare
        // fs::write doesn't flush on Windows, and our subprocess
        // exits immediately after this call.
        let marker = paths_for_save.data_dir.join(".settings_refresh");
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&marker)
        {
            Ok(f) => {
                if let Err(e) = f.sync_all() {
                    warn!("settings: fsync refresh marker failed: {e}");
                }
            }
            Err(e) => warn!("settings: write refresh marker failed: {e}"),
        }
        // Close via the desktop window helper.
        let window = dioxus::desktop::window();
        window.close();
    };

    let on_cancel = move |_| {
        let window = dioxus::desktop::window();
        window.close();
    };

    let on_reset = move |_| {
        let fresh = Settings::default();
        hotkey_buf.set(fresh.hotkey.raw.clone());
        annotate_hotkey_buf.set(fresh.annotate_hotkey.raw.clone());
        gif_hotkey_buf.set(fresh.gif_hotkey.raw.clone());
        launch_at_startup.set(fresh.launch_at_startup);
        include_cursor.set(fresh.include_cursor);
        copy_to_clipboard.set(fresh.copy_to_clipboard);
        output_dir_buf.set(String::new());
        arrow_shadow.set(fresh.arrow_shadow);
        arrow_advanced_color.set(fresh.arrow_advanced_color);
        gif_fps.set(fresh.gif_fps);
        gif_loop_count.set(fresh.gif_loop_count);
        gif_max_seconds.set(fresh.gif_max_seconds);
        gif_record_cursor.set(fresh.gif_record_cursor);
        status.set("Reset — click Save to apply, Cancel to discard.".to_string());
    };

    rsx! {
        footer { class: "footer",
            div {
                class: if is_error { "status" } else { "status ok" },
                "{status_text}"
            }
            div { class: "actions",
                button { class: "danger", onclick: on_reset, "Reset to defaults" }
                button { class: "ghost", onclick: on_cancel, "Cancel" }
                button { class: "primary", onclick: on_save, "Save" }
            }
        }
    }
}

/// Translate a Dioxus `Key` + the live modifier state into a chord
/// string in the format `parse_chord` expects (e.g. "Ctrl+Shift+X").
/// Returns `None` for keys we don't bind (modifiers-only presses,
/// media keys, etc.).
fn format_chord(key: &Key, mods: &Modifiers) -> Option<String> {
    let token = key_token(key)?;
    let mut out = String::new();
    if mods.ctrl() {
        out.push_str("Ctrl+");
    }
    if mods.shift() {
        out.push_str("Shift+");
    }
    if mods.alt() {
        out.push_str("Alt+");
    }
    if mods.meta() {
        out.push_str("Win+");
    }
    out.push_str(token.as_str());
    Some(out)
}

/// Map a Dioxus `Key` to the canonical chord-token string our parser
/// understands. Returns `None` for modifier-only presses and media
/// keys.
fn key_token(k: &Key) -> Option<String> {
    Some(match k {
        Key::Character(s) => {
            let trimmed = s.trim();
            if trimmed.len() == 1 {
                let c = trimmed.chars().next().unwrap();
                if c.is_ascii_alphabetic() {
                    c.to_ascii_uppercase().to_string()
                } else if c.is_ascii_digit() {
                    c.to_string()
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        Key::F1 => "F1".into(),
        Key::F2 => "F2".into(),
        Key::F3 => "F3".into(),
        Key::F4 => "F4".into(),
        Key::F5 => "F5".into(),
        Key::F6 => "F6".into(),
        Key::F7 => "F7".into(),
        Key::F8 => "F8".into(),
        Key::F9 => "F9".into(),
        Key::F10 => "F10".into(),
        Key::F11 => "F11".into(),
        Key::F12 => "F12".into(),
        Key::Enter => "Enter".into(),
        Key::Tab => "Tab".into(),
        Key::Backspace => "Backspace".into(),
        Key::Delete => "Delete".into(),
        Key::Insert => "Insert".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::PageUp => "PageUp".into(),
        Key::PageDown => "PageDown".into(),
        Key::ArrowUp => "Up".into(),
        Key::ArrowDown => "Down".into(),
        Key::ArrowLeft => "Left".into(),
        Key::ArrowRight => "Right".into(),
        Key::PrintScreen => "PrintScreen".into(),
        // Space character arrives as Key::Character(" "); the
        // arm above returns None for it. Match here as a fallback
        // for the named variant some platforms emit instead.
        _ => return None,
    })
}
