//! Hotkey binding parser: serializable accelerator strings like
//! `Ctrl+Shift+X` or `PrintScreen` that round-trip through TOML and compile
//! to `global_hotkey::hotkey::HotKey`.
//!
//! M5 adds preset-bound hotkeys (feature #4). Presets store their chord as
//! a plain string; to check validity at the UI layer, callers use the
//! standalone `parse_chord` helper.

use anyhow::{anyhow, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBinding {
    /// Raw accelerator string, e.g. "Ctrl+Shift+X" or "PrintScreen".
    pub raw: String,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        // Default for the primary "Capture fullscreen" hotkey. Ctrl+Shift+S
        // is the screenshot convention: two modifiers means it won't fire
        // from incidental typing, and it works on laptops without a
        // dedicated PrintScreen key. Only real conflict is apps' "Save As"
        // shortcut, which gets shadowed while GrabIt is running.
        Self { raw: "Ctrl+Shift+S".to_string() }
    }
}

impl HotkeyBinding {
    pub fn as_hotkey(&self) -> Result<HotKey> {
        parse(&self.raw)
    }
}

/// Public chord parser used by the presets UI for validation. Returns the
/// canonicalised chord string alongside the compiled `HotKey` so callers
/// can both register it and round-trip it back to TOML in a stable form.
pub fn parse_chord(s: &str) -> Result<(String, HotKey)> {
    let hk = parse(s)?;
    Ok((canonicalise(s)?, hk))
}

/// Normalise a chord string to "Ctrl+Shift+Alt+Key" order. Used so that
/// edits like "shift+ctrl+x" round-trip as "Ctrl+Shift+X".
fn canonicalise(s: &str) -> Result<String> {
    let mut has_ctrl = false;
    let mut has_shift = false;
    let mut has_alt = false;
    let mut has_super = false;
    let mut key_token: Option<String> = None;

    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => has_ctrl = true,
            "shift" => has_shift = true,
            "alt" | "menu" => has_alt = true,
            "super" | "win" | "meta" => has_super = true,
            _ => {
                // Verify the key token parses before we accept it.
                parse_code(part)?;
                key_token = Some(canonical_key_name(part));
            }
        }
    }

    let key = key_token.ok_or_else(|| anyhow!("hotkey '{s}' has no key component"))?;
    let mut out = String::new();
    if has_ctrl { out.push_str("Ctrl+"); }
    if has_shift { out.push_str("Shift+"); }
    if has_alt { out.push_str("Alt+"); }
    if has_super { out.push_str("Win+"); }
    out.push_str(&key);
    Ok(out)
}

fn canonical_key_name(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "printscreen" | "prtsc" | "prtscn" | "prntscrn" => "PrintScreen".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "space" => "Space".to_string(),
        "tab" => "Tab".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" | "del" => "Delete".to_string(),
        "insert" | "ins" => "Insert".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" | "pgup" => "PageUp".to_string(),
        "pagedown" | "pgdn" => "PageDown".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        f if f.starts_with('f') && f.len() <= 3 && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            format!("F{}", &f[1..])
        }
        other if other.len() == 1 => {
            let ch = other.chars().next().unwrap();
            ch.to_ascii_uppercase().to_string()
        }
        other => other.to_string(),
    }
}

fn parse(s: &str) -> Result<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;

    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "shift" => mods |= Modifiers::SHIFT,
            "alt" | "menu" => mods |= Modifiers::ALT,
            "super" | "win" | "meta" => mods |= Modifiers::META,
            _ => {
                code = Some(parse_code(part)?);
            }
        }
    }

    let code = code.ok_or_else(|| anyhow!("hotkey '{s}' has no key component"))?;
    Ok(HotKey::new(Some(mods), code))
}

fn parse_code(s: &str) -> Result<Code> {
    // Accept common aliases alongside the W3C `KeyboardEvent.code` names that
    // global-hotkey uses natively.
    let canon = s.to_ascii_lowercase();
    Ok(match canon.as_str() {
        "printscreen" | "prtsc" | "prtscn" | "prntscrn" => Code::PrintScreen,
        "escape" | "esc" => Code::Escape,
        "space" => Code::Space,
        "tab" => Code::Tab,
        "enter" | "return" => Code::Enter,
        "backspace" => Code::Backspace,
        "delete" | "del" => Code::Delete,
        "insert" | "ins" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" | "pgup" => Code::PageUp,
        "pagedown" | "pgdn" => Code::PageDown,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "f1" => Code::F1, "f2" => Code::F2, "f3" => Code::F3, "f4" => Code::F4,
        "f5" => Code::F5, "f6" => Code::F6, "f7" => Code::F7, "f8" => Code::F8,
        "f9" => Code::F9, "f10" => Code::F10, "f11" => Code::F11, "f12" => Code::F12,
        other if other.len() == 1 => {
            let ch = other.chars().next().unwrap();
            letter_or_digit(ch)
                .ok_or_else(|| anyhow!("unsupported key '{s}'"))?
        }
        other => return Err(anyhow!("unsupported key '{other}'")),
    })
}

fn letter_or_digit(ch: char) -> Option<Code> {
    Some(match ch.to_ascii_uppercase() {
        'A' => Code::KeyA, 'B' => Code::KeyB, 'C' => Code::KeyC, 'D' => Code::KeyD,
        'E' => Code::KeyE, 'F' => Code::KeyF, 'G' => Code::KeyG, 'H' => Code::KeyH,
        'I' => Code::KeyI, 'J' => Code::KeyJ, 'K' => Code::KeyK, 'L' => Code::KeyL,
        'M' => Code::KeyM, 'N' => Code::KeyN, 'O' => Code::KeyO, 'P' => Code::KeyP,
        'Q' => Code::KeyQ, 'R' => Code::KeyR, 'S' => Code::KeyS, 'T' => Code::KeyT,
        'U' => Code::KeyU, 'V' => Code::KeyV, 'W' => Code::KeyW, 'X' => Code::KeyX,
        'Y' => Code::KeyY, 'Z' => Code::KeyZ,
        '0' => Code::Digit0, '1' => Code::Digit1, '2' => Code::Digit2, '3' => Code::Digit3,
        '4' => Code::Digit4, '5' => Code::Digit5, '6' => Code::Digit6, '7' => Code::Digit7,
        '8' => Code::Digit8, '9' => Code::Digit9,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_printscreen() {
        parse("PrintScreen").unwrap();
        parse("PrtSc").unwrap();
    }

    #[test]
    fn parses_modified_letter() {
        parse("Ctrl+Shift+X").unwrap();
    }

    #[test]
    fn rejects_empty() {
        assert!(parse("Ctrl+Shift").is_err());
    }

    #[test]
    fn rejects_garbage_chord() {
        assert!(parse_chord("").is_err());
        assert!(parse_chord("Ctrl+").is_err());
        assert!(parse_chord("Ctrl+BogusKey").is_err());
        assert!(parse_chord("%%%").is_err());
    }

    #[test]
    fn canonicalises_out_of_order_chord() {
        let (canon, _) = parse_chord("shift+ctrl+x").unwrap();
        assert_eq!(canon, "Ctrl+Shift+X");
    }

    #[test]
    fn canonicalises_printscreen_aliases() {
        let (canon, _) = parse_chord("prtsc").unwrap();
        assert_eq!(canon, "PrintScreen");
        let (canon, _) = parse_chord("Ctrl+prtscn").unwrap();
        assert_eq!(canon, "Ctrl+PrintScreen");
    }

    #[test]
    fn roundtrip_through_canonical_form_is_stable() {
        let first = parse_chord("Ctrl+Shift+1").unwrap().0;
        let second = parse_chord(&first).unwrap().0;
        assert_eq!(first, second);
    }

    #[test]
    fn canonical_f_keys() {
        assert_eq!(parse_chord("f12").unwrap().0, "F12");
        assert_eq!(parse_chord("Ctrl+F5").unwrap().0, "Ctrl+F5");
    }
}
