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
    /// Register every `(binding, command)` pair with the OS. Bindings that
    /// fail to register (unsupported key, or another app already owns the
    /// combo) are logged and skipped — other bindings still register.
    pub fn install(
        _cmd_tx: Sender<Command>,
        bindings: &[(HotkeyBinding, Command)],
    ) -> Result<Self> {
        let manager = GlobalHotKeyManager::new().context("create hotkey manager")?;
        let mut mapping: Vec<(u32, Command)> = Vec::new();

        for (binding, cmd) in bindings {
            match binding.as_hotkey() {
                Ok(hotkey) => match manager.register(hotkey) {
                    Ok(()) => {
                        info!("registered hotkey {} \u{2192} {cmd:?}", binding.raw);
                        mapping.push((hotkey.id(), cmd.clone()));
                    }
                    Err(e) => {
                        warn!(
                            "could not register hotkey {}: {e} (another app or invalid combo); skipping",
                            binding.raw
                        );
                    }
                },
                Err(e) => warn!("invalid hotkey {}: {e}; skipping", binding.raw),
            }
        }

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
