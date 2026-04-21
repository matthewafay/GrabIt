pub mod menu;

use crate::app::Command;
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
}

#[derive(Clone)]
pub struct TrayMenuIds {
    pub capture: tray_icon::menu::MenuId,
    pub capture_interactive: tray_icon::menu::MenuId,
    pub capture_annotate: tray_icon::menu::MenuId,
    pub capture_delay_3: tray_icon::menu::MenuId,
    pub capture_delay_5: tray_icon::menu::MenuId,
    pub capture_delay_10: tray_icon::menu::MenuId,
    pub open_output: tray_icon::menu::MenuId,
    pub settings: tray_icon::menu::MenuId,
    pub autostart: tray_icon::menu::MenuId,
    pub quit: tray_icon::menu::MenuId,
}

// The current menu ids are threaded through a process-global so the menu-
// event receiver (static inside tray-icon) can resolve ids back to commands.
static MENU_IDS: Mutex<Option<Arc<TrayMenuIds>>> = Mutex::new(None);

impl Tray {
    pub fn install(cmd_tx: Sender<Command>, settings: &Settings) -> Result<Self> {
        let menu = Menu::new();

        let capture = MenuItem::new("Capture fullscreen", true, None);
        let capture_interactive = MenuItem::new("Capture region / window\u{2026}", true, None);
        let capture_annotate = MenuItem::new("Capture \u{0026} annotate\u{2026}", true, None);

        let delay_submenu = Submenu::new("Capture with delay", true);
        let capture_delay_3 = MenuItem::new("3 seconds", true, None);
        let capture_delay_5 = MenuItem::new("5 seconds", true, None);
        let capture_delay_10 = MenuItem::new("10 seconds", true, None);
        delay_submenu.append(&capture_delay_3).context("append 3s")?;
        delay_submenu.append(&capture_delay_5).context("append 5s")?;
        delay_submenu.append(&capture_delay_10).context("append 10s")?;

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
        menu.append(&delay_submenu).context("append delay submenu")?;
        menu.append(&PredefinedMenuItem::separator()).ok();
        menu.append(&open_output).context("append output item")?;
        menu.append(&settings_item).context("append settings item")?;
        menu.append(&autostart).context("append autostart item")?;
        menu.append(&PredefinedMenuItem::separator()).ok();
        menu.append(&quit).context("append quit item")?;

        let ids = TrayMenuIds {
            capture: capture.id().clone(),
            capture_interactive: capture_interactive.id().clone(),
            capture_annotate: capture_annotate.id().clone(),
            capture_delay_3: capture_delay_3.id().clone(),
            capture_delay_5: capture_delay_5.id().clone(),
            capture_delay_10: capture_delay_10.id().clone(),
            open_output: open_output.id().clone(),
            settings: settings_item.id().clone(),
            autostart: autostart.id().clone(),
            quit: quit.id().clone(),
        };
        *MENU_IDS.lock() = Some(Arc::new(ids.clone()));

        let icon = TrayIconBuilder::new()
            .with_tooltip("GrabIt")
            .with_icon(fallback_icon())
            .with_menu(Box::new(menu))
            .build()
            .context("build tray icon")?;

        Ok(Self { icon, ids, cmd_tx })
    }
}

/// 16x16 solid-teal placeholder icon so the tray is visible before a real
/// .ico is supplied. Documented in `assets/icons/README.md`.
fn fallback_icon() -> Icon {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x14, 0xb8, 0xa6, 0xff]); // teal
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
    } else if ev.id == ids.capture_delay_3 {
        Some(Command::CaptureWithDelay { delay_ms: 3_000 })
    } else if ev.id == ids.capture_delay_5 {
        Some(Command::CaptureWithDelay { delay_ms: 5_000 })
    } else if ev.id == ids.capture_delay_10 {
        Some(Command::CaptureWithDelay { delay_ms: 10_000 })
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
