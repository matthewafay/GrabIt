//! HKCU Run-key autostart integration.
//!
//! Writing under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`
//! needs no elevation and survives roaming-profile sync. The value stored is
//! the current process's executable path, optionally quoted.

use anyhow::{Context, Result};
use log::debug;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "GrabIt";

/// Align the Run-key state with the user's preference.
pub fn sync(enabled: &bool) -> Result<()> {
    if *enabled {
        enable()
    } else {
        disable()
    }
}

pub fn enable() -> Result<()> {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        use winreg::RegKey;

        let exe = std::env::current_exe().context("resolve current exe path")?;
        // Quote to survive paths containing spaces.
        let mut value = String::from("\"");
        value.push_str(&exe.to_string_lossy());
        value.push('"');

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
            .map(|k| (k, false))
            .or_else(|_| hkcu.create_subkey(RUN_KEY).map(|(k, _)| (k, true)))
            .context("open HKCU Run key")?;
        key.set_value(VALUE_NAME, &value)
            .context("write Run value")?;
        debug!("autostart enabled: {value}");
    }
    Ok(())
}

pub fn disable() -> Result<()> {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        use winreg::RegKey;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) = hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
            // Ignore "value not found" — being absent is the goal.
            let _ = key.delete_value(VALUE_NAME);
            debug!("autostart disabled");
        }
    }
    Ok(())
}

#[allow(dead_code)] // consumed by the settings window in M2+.
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(key) = hkcu.open_subkey(RUN_KEY) {
            return key.get_value::<String, _>(VALUE_NAME).is_ok();
        }
    }
    false
}
