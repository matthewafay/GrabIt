//! Named-mutex single-instance guard.
//!
//! A second `grabit.exe` that finds the mutex already held exits immediately.
//! M0 intentionally does not implement `WM_COPYDATA` relay to the running
//! instance — that polish lands with the settings window in M2, at which
//! point a second launch should forward "open settings" / "capture now" to
//! the existing process instead of silently exiting.

use log::debug;
use thiserror::Error;

const MUTEX_NAME: &str = "Global\\GrabIt-Singleton-38a7";

#[derive(Debug, Error)]
pub enum Error {
    #[error("another GrabIt instance is already running")]
    AlreadyRunning,
    #[error("Win32 error creating mutex: {0}")]
    Win32(String),
}

pub struct Guard {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(windows)]
        unsafe {
            if !self.handle.is_invalid() {
                let _ = windows::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
pub fn acquire() -> Result<Guard, Error> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    let name = HSTRING::from(MUTEX_NAME);
    let handle = unsafe { CreateMutexW(None, true, &name) }
        .map_err(|e| Error::Win32(e.to_string()))?;

    // CreateMutexW returns a valid handle even if the mutex already exists;
    // we detect contention via GetLastError immediately after the call.
    let last = unsafe { GetLastError() };
    if last == ERROR_ALREADY_EXISTS {
        unsafe { let _ = windows::Win32::Foundation::CloseHandle(handle); }
        return Err(Error::AlreadyRunning);
    }

    debug!("acquired singleton mutex");
    Ok(Guard { handle })
}

#[cfg(not(windows))]
pub fn acquire() -> Result<Guard, Error> {
    Ok(Guard {})
}
