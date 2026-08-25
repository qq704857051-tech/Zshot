use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;

use crate::error::{Error, Result};

const NAME: &str = "Local\\Zshot.SingleInstance\0";

pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        unsafe {
            let mut wide: Vec<u16> = NAME.encode_utf16().collect();
            let handle = CreateMutexW(None, true, PCWSTR(wide.as_mut_ptr()))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Err(Error::new("already running"));
            }
            Ok(Self { handle })
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}
