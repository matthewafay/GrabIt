pub mod menu;

use crate::app::Command;
use crate::presets::PresetStore;
use crate::settings::Settings;
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use log::{debug, warn};
use parking_lot::Mutex;
use std::sync::Arc;
use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Handle that keeps the tray icon alive for the program's lifetime.
#[allow(dead_code)]
pub struct Tray {
    icon: TrayIcon,
    ids: TrayMenuIds,
    cmd_tx: Sender<Command>,
    /// Live handles on the capture MenuItems so we can update their
    /// accelerator labels in-place when settings change — avoids a full
    /// tray reinstall (which flashes the icon) or a menu swap (which
    /// didn't reliably update the visible chord on Windows).
    capture_item: MenuItem,
    capture_annotate_item: MenuItem,
    record_gif_item: MenuItem,
}

#[derive(Clone)]
pub struct TrayMenuIds {
    pub capture: tray_icon::menu::MenuId,
    pub capture_annotate: tray_icon::menu::MenuId,
    pub record_gif: tray_icon::menu::MenuId,
    pub open_output: tray_icon::menu::MenuId,
    pub history: tray_icon::menu::MenuId,
    pub settings: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

// The current menu ids are threaded through a process-global so the menu-
// event receiver (static inside tray-icon) can resolve ids back to commands.
static MENU_IDS: Mutex<Option<Arc<TrayMenuIds>>> = Mutex::new(None);

impl Tray {
    pub fn install(
        cmd_tx: Sender<Command>,
        settings: &Settings,
        _presets: &PresetStore,
    ) -> Result<Self> {
        let menu = Menu::new();

        let fullscreen_accel = parse_accelerator(&settings.hotkey.raw);
        let annotate_accel = parse_accelerator(&settings.annotate_hotkey.raw);
        let gif_accel = parse_accelerator(&settings.gif_hotkey.raw);

        let capture_item = MenuItem::new("Capture fullscreen", true, fullscreen_accel);
        let capture_annotate_item = MenuItem::new(
            "Capture \u{0026} annotate\u{2026}",
            true,
            annotate_accel,
        );
        let record_gif_item = MenuItem::new("Record GIF\u{2026}", true, gif_accel);
        let open_output = MenuItem::new("Open output folder", true, None);
        let history_item = MenuItem::new("History\u{2026}", true, None);
        let settings_item = MenuItem::new("Settings\u{2026}", true, None);
        let quit = MenuItem::new("Quit GrabIt", true, None);

        menu.append(&capture_item).context("append capture item")?;
        menu.append(&capture_annotate_item).context("append annotate item")?;
        menu.append(&record_gif_item).context("append record gif item")?;
        menu.append(&PredefinedMenuItem::separator()).ok();
        menu.append(&open_output).context("append output item")?;
        menu.append(&history_item).context("append history item")?;
        menu.append(&settings_item).context("append settings item")?;
        menu.append(&PredefinedMenuItem::separator()).ok();
        menu.append(&quit).context("append quit item")?;

        let ids = TrayMenuIds {
            capture: capture_item.id().clone(),
            capture_annotate: capture_annotate_item.id().clone(),
            record_gif: record_gif_item.id().clone(),
            open_output: open_output.id().clone(),
            history: history_item.id().clone(),
            settings: settings_item.id().clone(),
            quit: quit.id().clone(),
        };
        *MENU_IDS.lock() = Some(Arc::new(ids.clone()));

        let icon = TrayIconBuilder::new()
            .with_tooltip("GrabIt")
            .with_icon(load_app_icon())
            .with_menu(Box::new(menu))
            .build()
            .context("build tray icon")?;

        Ok(Self {
            icon,
            ids,
            cmd_tx,
            capture_item,
            capture_annotate_item,
            record_gif_item,
        })
    }

    /// Push the latest hotkey chords onto the live menu items without
    /// tearing down the tray icon. `set_accelerator` rewrites the visible
    /// shortcut text (the right-aligned "Ctrl+Shift+A" you see on the menu
    /// row); `set_text` re-renders the full item so the change takes
    /// effect on the next tray open. Cheap and side-effect-free — no
    /// flicker, no lost events.
    pub fn refresh_hotkey_labels(&self, settings: &Settings) {
        let fullscreen_accel = parse_accelerator(&settings.hotkey.raw);
        let annotate_accel = parse_accelerator(&settings.annotate_hotkey.raw);
        let gif_accel = parse_accelerator(&settings.gif_hotkey.raw);

        if let Err(e) = self.capture_item.set_accelerator(fullscreen_accel) {
            warn!("tray: set fullscreen accelerator failed: {e}");
        }
        // Re-setting the item text forces muda to re-format the visible
        // "text\t<accelerator>" string on Windows. Without this, some
        // builds leave the previous accelerator rendered even though the
        // internal accelerator was updated.
        self.capture_item.set_text("Capture fullscreen");

        if let Err(e) = self
            .capture_annotate_item
            .set_accelerator(annotate_accel)
        {
            warn!("tray: set annotate accelerator failed: {e}");
        }
        self.capture_annotate_item
            .set_text("Capture \u{0026} annotate\u{2026}");

        if let Err(e) = self.record_gif_item.set_accelerator(gif_accel) {
            warn!("tray: set record-gif accelerator failed: {e}");
        }
        self.record_gif_item.set_text("Record GIF\u{2026}");
    }

    /// Kept as a no-op for API compatibility with the main-loop reload path.
    /// Presets no longer appear in the tray menu — they still register their
    /// hotkeys via `hotkeys::Registrar::refresh_hotkeys`, but there is no
    /// tray entry to rebuild.
    pub fn rebuild_presets(&mut self, _presets: &PresetStore) -> Result<()> {
        Ok(())
    }
}

/// Decode the embedded PNG logo into a tray-icon `Icon`. Falls back to a
/// solid-color stub if decoding fails (which shouldn't happen — the PNG is
/// baked into the binary via `include_bytes!`).
fn load_app_icon() -> Icon {
    const PNG: &[u8] = include_bytes!("../../assets/icons/grabit.png");
    match image::load_from_memory(PNG) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            Icon::from_rgba(rgba.into_raw(), w, h).unwrap_or_else(|_| fallback_icon())
        }
        Err(_) => fallback_icon(),
    }
}

/// Solid-teal 16x16 stub, used only if the embedded logo fails to decode.
fn fallback_icon() -> Icon {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x14, 0xb8, 0xa6, 0xff]);
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid placeholder icon")
}

pub fn on_menu_event(ev: MenuEvent, cmd_tx: &Sender<Command>) {
    let guard = MENU_IDS.lock();
    let Some(ids) = guard.as_ref() else { return };

    let cmd = if ev.id == ids.capture {
        Some(Command::CaptureFullscreen)
    } else if ev.id == ids.capture_annotate {
        Some(Command::CaptureAndAnnotate)
    } else if ev.id == ids.record_gif {
        Some(Command::CaptureGif)
    } else if ev.id == ids.open_output {
        Some(Command::OpenOutputFolder)
    } else if ev.id == ids.history {
        Some(Command::OpenHistory)
    } else if ev.id == ids.settings {
        Some(Command::OpenSettings)
    } else if ev.id == ids.quit {
        Some(Command::Quit)
    } else {
        debug!("unknown tray menu id {:?}", ev.id);
        None
    };

    if let Some(cmd) = cmd {
        if let Err(e) = cmd_tx.send(cmd) {
            warn!("tray command send failed: {e}");
        }
    }
}

/// Convert a GrabIt chord string (e.g. "Ctrl+X", "PrintScreen") into a
/// muda/tray-icon Accelerator for display on a menu item. Returns `None`
/// on a parse error — the menu item will still render, just without the
/// shortcut hint.
fn parse_accelerator(chord: &str) -> Option<Accelerator> {
    use std::str::FromStr;
    match Accelerator::from_str(chord) {
        Ok(a) => Some(a),
        Err(e) => {
            debug!("tray accelerator parse failed for {chord:?}: {e}");
            None
        }
    }
}

pub fn on_tray_event(_ev: TrayIconEvent, _cmd_tx: &Sender<Command>) {
    // Left-click / double-click bindings can be wired here later. For M0 the
    // menu is the only surface.
}
