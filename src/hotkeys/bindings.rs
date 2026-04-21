//! Hotkey binding parser: serializable accelerator strings like
//! `Ctrl+Shift+X` or `PrintScreen` that round-trip through TOML and compile
//! to `global_hotkey::hotkey::HotKey`.
//!
//! M0 only handles the capture hotkey. Preset-bound hotkeys (feature #4)
//! arrive in M5 and will extend this module with a `Vec<HotkeyBinding>`.

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
        // PrintScreen is the Windows convention for "take a screenshot".
        Self { raw: "PrintScreen".to_string() }
    }
}

impl HotkeyBinding {
    pub fn as_hotkey(&self) -> Result<HotKey> {
        parse(&self.raw)
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
}
