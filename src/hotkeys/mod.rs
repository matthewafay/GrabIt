pub mod bindings;

use crate::app::Command;
use anyhow::{Context, Result};
use bindings::HotkeyBinding;
use crossbeam_channel::Sender;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use log::{debug, info, warn};
use parking_lot::Mutex;

/// A compact description of a preset-bound hotkey, fed into
/// `Registrar::refresh_hotkeys`. The `chord` is the canonicalised
/// accelerator string; `preset_name` is the key used by `Command::CapturePreset`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetHotkey {
    pub chord: String,
    pub preset_name: String,
}

/// Installed hotkey state. Dropping this unregisters all bound hotkeys with
/// the OS.
pub struct Registrar {
    manager: GlobalHotKeyManager,
    /// The global (settings-level) bindings installed at startup. Kept
    /// pinned so a presets refresh doesn't unregister them.
    #[allow(dead_code)]
    global: Vec<Registered>,
    /// Preset-bound hotkeys — swapped wholesale on `refresh_hotkeys`.
    presets: Vec<Registered>,
}

#[derive(Debug, Clone)]
struct Registered {
    id: u32,
    hotkey: HotKey,
    command: Command,
    /// The canonical chord string, used for diffing + logging.
    chord: String,
}

/// Map of hotkey id -> dispatched Command, consulted by `on_event`. Holds
/// both the global and preset-bound entries so either kind fires the
/// correct command from the same event receiver.
static DISPATCH: Mutex<Vec<(u32, Command)>> = Mutex::new(Vec::new());

/// Collision report from a refresh call — returned so the settings UI can
/// surface "Ctrl+Shift+1 is already taken" to the user. A collision here
/// means either another app owns the chord, or two presets bind the same one.
#[derive(Debug, Clone)]
pub struct RefreshReport {
    /// Presets that registered successfully (preset_name, chord).
    pub registered: Vec<(String, String)>,
    /// Presets whose chord could not be registered (preset_name, chord, reason).
    pub failed: Vec<(String, String, String)>,
}

impl Registrar {
    /// Register every `(binding, command)` pair with the OS. Bindings that
    /// fail to register (unsupported key, or another app already owns the
    /// combo) are logged and skipped — other bindings still register.
    pub fn install(
        _cmd_tx: Sender<Command>,
        bindings: &[(HotkeyBinding, Command)],
    ) -> Result<Self> {
        let manager = GlobalHotKeyManager::new().context("create hotkey manager")?;
        let mut global: Vec<Registered> = Vec::new();

        for (binding, cmd) in bindings {
            match binding.as_hotkey() {
                Ok(hotkey) => match manager.register(hotkey) {
                    Ok(()) => {
                        info!("registered hotkey {} \u{2192} {cmd:?}", binding.raw);
                        global.push(Registered {
                            id: hotkey.id(),
                            hotkey,
                            command: cmd.clone(),
                            chord: binding.raw.clone(),
                        });
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

        let this = Self { manager, global, presets: Vec::new() };
        this.rebuild_dispatch();
        Ok(this)
    }

    /// Replace the currently-installed **preset** hotkeys with `next`. The
    /// global (settings) hotkeys are untouched. Returns a report of which
    /// chords registered and which collided. Safe to call at runtime.
    pub fn refresh_hotkeys(&mut self, next: &[PresetHotkey]) -> RefreshReport {
        // 1. Canonicalise + deduplicate. If two presets claim the same chord,
        //    report the second as a collision instead of silently dropping it.
        //    Chords that collide with a global (settings-level) binding also
        //    fall in here — the global wins.
        let mut seen: std::collections::HashSet<String> =
            self.global.iter().map(|g| g.chord.clone()).collect();
        let mut desired: Vec<PresetHotkey> = Vec::with_capacity(next.len());
        let mut report = RefreshReport { registered: Vec::new(), failed: Vec::new() };

        for entry in next {
            let canon = match bindings::parse_chord(&entry.chord) {
                Ok((c, _)) => c,
                Err(e) => {
                    report.failed.push((
                        entry.preset_name.clone(),
                        entry.chord.clone(),
                        format!("unparseable: {e}"),
                    ));
                    continue;
                }
            };
            if !seen.insert(canon.clone()) {
                report.failed.push((
                    entry.preset_name.clone(),
                    canon.clone(),
                    "duplicate chord in this session".into(),
                ));
                continue;
            }
            desired.push(PresetHotkey { chord: canon, preset_name: entry.preset_name.clone() });
        }

        // 2. Partition current preset bindings into keep/drop. "Keep" means
        //    both the chord and the target preset name match — if either
        //    changed, we must re-register so the dispatch map stays current.
        let mut kept: Vec<Registered> = Vec::new();
        let mut to_unregister: Vec<Registered> = Vec::new();
        for reg in self.presets.drain(..) {
            let matches_desired = desired.iter().any(|d| {
                d.chord == reg.chord
                    && matches!(&reg.command, Command::CapturePreset(n) if *n == d.preset_name)
            });
            if matches_desired {
                kept.push(reg);
            } else {
                to_unregister.push(reg);
            }
        }

        for reg in &to_unregister {
            if let Err(e) = self.manager.unregister(reg.hotkey) {
                warn!("unregister {} failed: {e}", reg.chord);
            } else {
                debug!("unregistered preset hotkey {}", reg.chord);
            }
        }

        // 3. Register any desired entry that wasn't kept.
        let existing_chords: std::collections::HashSet<String> =
            kept.iter().map(|r| r.chord.clone()).collect();
        for entry in &desired {
            if existing_chords.contains(&entry.chord) {
                report
                    .registered
                    .push((entry.preset_name.clone(), entry.chord.clone()));
                continue;
            }
            let hk = match bindings::parse_chord(&entry.chord) {
                Ok((_c, hk)) => hk,
                Err(e) => {
                    report.failed.push((
                        entry.preset_name.clone(),
                        entry.chord.clone(),
                        format!("parse: {e}"),
                    ));
                    continue;
                }
            };
            match self.manager.register(hk) {
                Ok(()) => {
                    info!(
                        "registered preset hotkey {} \u{2192} {}",
                        entry.chord, entry.preset_name
                    );
                    kept.push(Registered {
                        id: hk.id(),
                        hotkey: hk,
                        command: Command::CapturePreset(entry.preset_name.clone()),
                        chord: entry.chord.clone(),
                    });
                    report
                        .registered
                        .push((entry.preset_name.clone(), entry.chord.clone()));
                }
                Err(e) => {
                    warn!("register {} failed: {e}", entry.chord);
                    report.failed.push((
                        entry.preset_name.clone(),
                        entry.chord.clone(),
                        format!("OS rejected (already in use?): {e}"),
                    ));
                }
            }
        }

        self.presets = kept;
        self.rebuild_dispatch();
        report
    }

    fn rebuild_dispatch(&self) {
        let mut out = Vec::with_capacity(self.global.len() + self.presets.len());
        for r in &self.global {
            out.push((r.id, r.command.clone()));
        }
        for r in &self.presets {
            out.push((r.id, r.command.clone()));
        }
        *DISPATCH.lock() = out;
    }
}

impl Drop for Registrar {
    fn drop(&mut self) {
        for reg in self.global.iter().chain(self.presets.iter()) {
            debug!("dropping hotkey id {}", reg.id);
        }
        // self.manager's own Drop unregisters everything it owns.
    }
}

/// Return the `Command` this hotkey event maps to, or `None` for events
/// we don't bind (key release, unknown id). The worker thread decides
/// whether to run the command inline (captures) or forward to the main
/// loop (everything else).
pub fn command_for_event(ev: &GlobalHotKeyEvent) -> Option<Command> {
    if ev.state != HotKeyState::Pressed {
        return None;
    }
    let guard = DISPATCH.lock();
    guard
        .iter()
        .find(|(id, _)| *id == ev.id)
        .map(|(_, cmd)| cmd.clone())
}

// ───────────────────────────────────────────────────────────────────────────
// Tests: `refresh_hotkeys` diff logic, exercised against a mock so we don't
// need a live Win32 message queue. The mock only stands in for the subset of
// `Registrar` we care about — we can't drive the real GlobalHotKeyManager
// in unit tests because it creates an OS-level registration.
// ───────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal reimplementation of the diff the real Registrar performs.
    /// Kept in lock-step with `refresh_hotkeys` so the behaviour is
    /// testable without touching the OS.
    #[derive(Default)]
    struct MockRegistrar {
        presets: Vec<(String, String)>, // (chord, preset_name)
        actions: Vec<String>,
    }

    impl MockRegistrar {
        fn refresh(&mut self, next: &[PresetHotkey]) -> RefreshReport {
            let mut desired: Vec<(String, String)> = Vec::new();
            let mut seen: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            let mut report = RefreshReport { registered: vec![], failed: vec![] };

            for entry in next {
                let (canon, _) = match bindings::parse_chord(&entry.chord) {
                    Ok(c) => c,
                    Err(e) => {
                        report.failed.push((
                            entry.preset_name.clone(),
                            entry.chord.clone(),
                            format!("unparseable: {e}"),
                        ));
                        continue;
                    }
                };
                if !seen.insert(canon.clone()) {
                    report.failed.push((
                        entry.preset_name.clone(),
                        canon.clone(),
                        "duplicate chord in this session".into(),
                    ));
                    continue;
                }
                desired.push((canon, entry.preset_name.clone()));
            }

            // Unregister stale.
            let keep: Vec<(String, String)> = self
                .presets
                .iter()
                .filter(|(c, n)| desired.iter().any(|(dc, dn)| dc == c && dn == n))
                .cloned()
                .collect();
            for (c, _) in self.presets.iter() {
                if !keep.iter().any(|(kc, _)| kc == c) {
                    self.actions.push(format!("unregister:{c}"));
                }
            }

            let existing_chords: std::collections::HashSet<String> =
                keep.iter().map(|(c, _)| c.clone()).collect();
            let mut final_list = keep;
            for (c, n) in &desired {
                if !existing_chords.contains(c) {
                    self.actions.push(format!("register:{c}"));
                    final_list.push((c.clone(), n.clone()));
                }
                report.registered.push((n.clone(), c.clone()));
            }
            self.presets = final_list;
            report
        }
    }

    fn ph(chord: &str, name: &str) -> PresetHotkey {
        PresetHotkey { chord: chord.to_string(), preset_name: name.to_string() }
    }

    #[test]
    fn refresh_from_empty_registers_new_entries() {
        let mut r = MockRegistrar::default();
        let report = r.refresh(&[ph("Ctrl+Shift+1", "A"), ph("Ctrl+Shift+2", "B")]);
        assert_eq!(report.registered.len(), 2);
        assert_eq!(report.failed.len(), 0);
        assert!(r.actions.iter().any(|a| a == "register:Ctrl+Shift+1"));
        assert!(r.actions.iter().any(|a| a == "register:Ctrl+Shift+2"));
    }

    #[test]
    fn refresh_unregisters_removed_presets() {
        let mut r = MockRegistrar::default();
        r.refresh(&[ph("Ctrl+Shift+1", "A"), ph("Ctrl+Shift+2", "B")]);
        r.actions.clear();
        r.refresh(&[ph("Ctrl+Shift+1", "A")]);
        assert!(r.actions.iter().any(|a| a == "unregister:Ctrl+Shift+2"));
        assert!(!r.actions.iter().any(|a| a == "register:Ctrl+Shift+1"));
    }

    #[test]
    fn refresh_rebinds_when_preset_name_changes() {
        let mut r = MockRegistrar::default();
        r.refresh(&[ph("Ctrl+Shift+1", "A")]);
        r.actions.clear();
        r.refresh(&[ph("Ctrl+Shift+1", "RenamedA")]);
        // Same chord, different preset — must re-register.
        assert!(r.actions.iter().any(|a| a == "unregister:Ctrl+Shift+1"));
        assert!(r.actions.iter().any(|a| a == "register:Ctrl+Shift+1"));
    }

    #[test]
    fn refresh_reports_duplicate_chords() {
        let mut r = MockRegistrar::default();
        let report = r.refresh(&[ph("Ctrl+Shift+1", "A"), ph("ctrl+shift+1", "B")]);
        assert_eq!(report.registered.len(), 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "B");
    }

    #[test]
    fn refresh_reports_unparseable_chords() {
        let mut r = MockRegistrar::default();
        let report = r.refresh(&[ph("Ctrl+", "A"), ph("Ctrl+Shift+2", "B")]);
        assert_eq!(report.registered.len(), 1);
        assert_eq!(report.failed.len(), 1);
        assert!(report.failed[0].2.contains("unparseable"));
    }

    #[test]
    fn refresh_is_idempotent_on_same_input() {
        let mut r = MockRegistrar::default();
        let entries = [ph("Ctrl+Shift+1", "A"), ph("Ctrl+Shift+2", "B")];
        r.refresh(&entries);
        r.actions.clear();
        r.refresh(&entries);
        // Nothing should change.
        assert!(r.actions.is_empty(), "actions: {:?}", r.actions);
    }
}
