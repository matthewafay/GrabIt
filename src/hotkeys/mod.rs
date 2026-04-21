pub mod bindings;

use crate::app::Command;
use anyhow::{Context, Result};
use bindings::HotkeyBinding;
use crossbeam_channel::Sender;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use log::{debug, info, warn};
use parking_lot::Mutex;

/// Installed hotkey state. Dropping this unregisters all bound hotkeys with
/// the OS.
pub struct Registrar {
    // Held for its Drop — when the Registrar is dropped, the manager
    // unregisters every hotkey we owned.
    #[allow(dead_code)]
    manager: GlobalHotKeyManager,
    registered: Vec<(u32, Command)>,
}

// Map of hotkey id -> dispatched Command, consulted by `on_event`.
static DISPATCH: Mutex<Vec<(u32, Command)>> = Mutex::new(Vec::new());

impl Registrar {
    pub fn install(_cmd_tx: Sender<Command>, binding: &HotkeyBinding) -> Result<Self> {
        let manager = GlobalHotKeyManager::new().context("create hotkey manager")?;

        let hotkey = binding.as_hotkey().context("parse capture hotkey")?;
        manager.register(hotkey).context("register capture hotkey")?;
        let id = hotkey.id();
        info!("registered capture hotkey: {}", binding.raw);

        let mapping = vec![(id, Command::CaptureFullscreen)];
        *DISPATCH.lock() = mapping.clone();

        Ok(Self { manager, registered: mapping })
    }
}

impl Drop for Registrar {
    fn drop(&mut self) {
        for (id, _) in &self.registered {
            // unregister by id isn't exposed directly; the manager's Drop
            // unregisters everything it owns, which is what we want here.
            debug!("dropping hotkey id {id}");
        }
        // self.manager's own Drop unregisters the hotkey when this Drop body returns.
    }
}

pub fn on_event(ev: GlobalHotKeyEvent, cmd_tx: &Sender<Command>) {
    if ev.state != HotKeyState::Pressed {
        return;
    }
    let guard = DISPATCH.lock();
    if let Some((_, cmd)) = guard.iter().find(|(id, _)| *id == ev.id) {
        if let Err(e) = cmd_tx.send(cmd.clone()) {
            warn!("hotkey command send failed: {e}");
        }
    }
}
