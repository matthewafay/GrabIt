pub mod menu;

use crate::app::Command;
use crate::presets::PresetStore;
use crate::settings::Settings;
use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use log::{debug, warn};
use parking_lot::Mutex;
use std::sync::Arc;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Handle that keeps the tray icon alive for the program's lifetime.
#[allow(dead_code)]
pub struct Tray {
    icon: TrayIcon,
    ids: TrayMenuIds,
    cmd_tx: Sender<Command>,
    /// The "Presets" submenu is rebuilt on hot reload — we hold a handle
    /// so we can clear & repopulate it in place.
    presets_submenu: Submenu,
    /// Preset menu items owned by the submenu; replaced on every rebuild.
    #[allow(dead_code)]
    preset_items: Vec<MenuItem>,
}

#[derive(Clone)]
pub struct TrayMenuIds {
    pub capture: tray_icon::menu::MenuId,
    pub capture_interactive: tray_icon::menu::MenuId,
    pub capture_annotate: tray_icon::menu::MenuId,
    pub capture_object: tray_icon::menu::MenuId,
    pub capture_delay_3: tray_icon::menu::MenuId,
    pub capture_delay_5: tray_icon::menu::MenuId,
    pub capture_delay_10: tray_icon::menu::MenuId,
    /// Preset exact-dimension entries. Pairs of `(menu_id, (w, h))` — the
    /// size list is defined centrally in `EXACT_DIMS_PRESETS` so adding a
    /// new preset only touches one place.
    pub capture_exact_dims: Vec<(tray_icon::menu::MenuId, (u32, u32))>,
    /// User-defined preset entries, pairs of `(menu_id, preset_name)`.
    /// Rebuilt on `RefreshHotkeys`.
    pub capture_presets: Vec<(tray_icon::menu::MenuId, String)>,
    pub open_output: tray_icon::menu::MenuId,
    pub settings: tray_icon::menu::MenuId,
    pub autostart: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

/// Preset sizes exposed in the "Capture exact size" submenu. Physical
/// pixels. Ordered largest → smallest because the most common request on a
/// modern display is a 1080p frame.
const EXACT_DIMS_PRESETS: &[(u32, u32, &str)] = &[
    (1920, 1080, "1920 x 1080 (FHD)"),
    (1600, 900, "1600 x 900"),
    (1280, 720, "1280 x 720 (HD)"),
    (1024, 768, "1024 x 768"),
    (800, 600, "800 x 600"),
    (640, 480, "640 x 480"),
    (500, 500, "500 x 500 (square)"),
];

// The current menu ids are threaded through a process-global so the menu-
// event receiver (static inside tray-icon) can resolve ids back to commands.
static MENU_IDS: Mutex<Option<Arc<TrayMenuIds>>> = Mutex::new(None);

impl Tray {
    pub fn install(
        cmd_tx: Sender<Command>,
        settings: &Settings,
        presets: &PresetStore,
    ) -> Result<Self> {
        let menu = Menu::new();

        let capture = MenuItem::new("Capture fullscreen", true, None);
        let capture_interactive = MenuItem::new("Capture region / window\u{2026}", true, None);
        let capture_annotate = MenuItem::new("Capture \u{0026} annotate\u{2026}", true, None);
        // M6: UIA element picker lives in the main capture section (per the
        // plan), not the Presets submenu.
        let capture_object = MenuItem::new("Capture object\u{2026}", true, None);

        let delay_submenu = Submenu::new("Capture with delay", true);
        let capture_delay_3 = MenuItem::new("3 seconds", true, None);
        let capture_delay_5 = MenuItem::new("5 seconds", true, None);
        let capture_delay_10 = MenuItem::new("10 seconds", true, None);
        delay_submenu.append(&capture_delay_3).context("append 3s")?;
        delay_submenu.append(&capture_delay_5).context("append 5s")?;
        delay_submenu.append(&capture_delay_10).context("append 10s")?;

        // "Capture exact size" submenu — feature #6. Presets keep the M1
        // wiring simple; a free-form WxH dialog can come later without
        // changing the command enum.
        let exact_submenu = Submenu::new("Capture exact size\u{2026}", true);
        let mut exact_items: Vec<MenuItem> = Vec::with_capacity(EXACT_DIMS_PRESETS.len());
        let mut exact_ids: Vec<(tray_icon::menu::MenuId, (u32, u32))> =
            Vec::with_capacity(EXACT_DIMS_PRESETS.len());
        for (w, h, label) in EXACT_DIMS_PRESETS {
            let item = MenuItem::new(*label, true, None);
            exact_submenu
                .append(&item)
                .with_context(|| format!("append exact preset {w}x{h}"))?;
            exact_ids.push((item.id().clone(), (*w, *h)));
            exact_items.push(item);
        }

        // "Presets" submenu (feature #3) — user-defined captures bound to
        // preset records on disk. Populated once here; `rebuild_presets`
        // rebuilds it on hot reload without reinstalling the whole tray.
        let presets_submenu = Submenu::new("Presets", true);
        let (preset_items, preset_ids) = populate_presets_submenu(&presets_submenu, presets)?;

        let open_output = MenuItem::new("Open output folder", true, None);
        let settings_item = MenuItem::new("Settings\u{2026}", true, None);
        let autostart = CheckMenuItem::new(
            "Launch at startup",
            true,
            settings.launch_at_startup,
            None,
        );
        let quit = MenuItem::new("Quit GrabIt", true, None);

        menu.append(&capture).context("append capture item")?;
        menu.append(&capture_interactive).context("append interactive item")?;
        menu.append(&capture_annotate).context("append annotate item")?;
        menu.append(&capture_object).context("append object item")?;
        menu.append(&delay_submenu).context("append delay submenu")?;
        menu.append(&exact_submenu).context("append exact submenu")?;
        menu.append(&presets_submenu).context("append presets submenu")?;
        menu.append(&PredefinedMenuItem::separator()).ok();
        menu.append(&open_output).context("append output item")?;
        menu.append(&settings_item).context("append settings item")?;
        menu.append(&autostart).context("append autostart item")?;
        menu.append(&PredefinedMenuItem::separator()).ok();
        menu.append(&quit).context("append quit item")?;

        // Silence unused-binding warnings for preset items — they are owned
        // by the submenu; we only need their ids.
        let _exact_items = exact_items;
        let ids = TrayMenuIds {
            capture: capture.id().clone(),
            capture_interactive: capture_interactive.id().clone(),
            capture_annotate: capture_annotate.id().clone(),
            capture_object: capture_object.id().clone(),
            capture_delay_3: capture_delay_3.id().clone(),
            capture_delay_5: capture_delay_5.id().clone(),
            capture_delay_10: capture_delay_10.id().clone(),
            capture_exact_dims: exact_ids,
            capture_presets: preset_ids,
            open_output: open_output.id().clone(),
            settings: settings_item.id().clone(),
            autostart: autostart.id().clone(),
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
            presets_submenu,
            preset_items,
        })
    }

    /// Rebuild the tray's "Presets" submenu from the current `PresetStore`.
    /// Called after the settings UI edits a preset (add / rename / delete)
    /// so the tray reflects the on-disk state without a restart.
    pub fn rebuild_presets(&mut self, presets: &PresetStore) -> Result<()> {
        // Remove existing entries. `tray-icon`'s Submenu exposes `remove` but
        // not an "iterate items" helper; we track the items we installed.
        for item in self.preset_items.drain(..) {
            if let Err(e) = self.presets_submenu.remove(&item) {
                debug!("remove preset item: {e}");
            }
        }
        let (items, preset_ids) = populate_presets_submenu(&self.presets_submenu, presets)?;
        self.preset_items = items;
        self.ids.capture_presets = preset_ids;
        *MENU_IDS.lock() = Some(Arc::new(self.ids.clone()));
        Ok(())
    }
}

/// `(menu_id, preset_name)` pairs — used to map a tray menu click back to
/// the preset it triggers.
type PresetIdPairs = Vec<(tray_icon::menu::MenuId, String)>;

/// Helper that fills a Submenu with one MenuItem per user preset. Returns
/// the owned items (so the caller can keep them alive) plus the id table
/// used to dispatch clicks back to `Command::CapturePreset`.
fn populate_presets_submenu(
    submenu: &Submenu,
    presets: &PresetStore,
) -> Result<(Vec<MenuItem>, PresetIdPairs)> {
    let mut items = Vec::with_capacity(presets.presets.len());
    let mut ids = Vec::with_capacity(presets.presets.len());
    if presets.presets.is_empty() {
        // Inert placeholder so the user knows the menu exists.
        let placeholder = MenuItem::new("(no presets yet)", false, None);
        submenu.append(&placeholder).context("append preset placeholder")?;
        items.push(placeholder);
        return Ok((items, ids));
    }
    for p in &presets.presets {
        let label = if p.hotkey.is_empty() {
            p.name.clone()
        } else {
            format!("{}  ({})", p.name, p.hotkey)
        };
        let item = MenuItem::new(label, true, None);
        submenu
            .append(&item)
            .with_context(|| format!("append preset {:?}", p.name))?;
        ids.push((item.id().clone(), p.name.clone()));
        items.push(item);
    }
    Ok((items, ids))
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
    } else if ev.id == ids.capture_interactive {
        Some(Command::CaptureInteractive)
    } else if ev.id == ids.capture_annotate {
        Some(Command::CaptureAndAnnotate)
    } else if ev.id == ids.capture_object {
        Some(Command::CaptureObject)
    } else if ev.id == ids.capture_delay_3 {
        Some(Command::CaptureWithDelay { delay_ms: 3_000 })
    } else if ev.id == ids.capture_delay_5 {
        Some(Command::CaptureWithDelay { delay_ms: 5_000 })
    } else if ev.id == ids.capture_delay_10 {
        Some(Command::CaptureWithDelay { delay_ms: 10_000 })
    } else if let Some((_, (w, h))) =
        ids.capture_exact_dims.iter().find(|(id, _)| *id == ev.id)
    {
        Some(Command::CaptureExactDims { width: *w, height: *h })
    } else if let Some((_, name)) =
        ids.capture_presets.iter().find(|(id, _)| *id == ev.id)
    {
        Some(Command::CapturePreset(name.clone()))
    } else if ev.id == ids.open_output {
        Some(Command::OpenOutputFolder)
    } else if ev.id == ids.settings {
        Some(Command::OpenSettings)
    } else if ev.id == ids.autostart {
        Some(Command::ToggleAutostart)
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

pub fn on_tray_event(_ev: TrayIconEvent, _cmd_tx: &Sender<Command>) {
    // Left-click / double-click bindings can be wired here later. For M0 the
    // menu is the only surface.
}
