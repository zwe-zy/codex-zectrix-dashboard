use crate::autostart::{AutostartError, RunRegistry};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::*,
};

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const NAME: &str = "CodexZectrixDashboard";
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

pub struct CurrentUserRun;
impl RunRegistry for CurrentUserRun {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        let mut key = std::ptr::null_mut();
        let result = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN).as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut key,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if result != ERROR_SUCCESS {
            return Err(AutostartError::Unavailable);
        }
        let key = Key(key);
        let mut kind = 0;
        let mut length = 0;
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                wide(NAME).as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut length,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if result != ERROR_SUCCESS
            || kind != REG_SZ
            || length == 0
            || length > 32768
            || length % 2 != 0
        {
            return Err(AutostartError::Unavailable);
        }
        let mut value = vec![0u16; length as usize / 2];
        let result = unsafe {
            RegQueryValueExW(
                key.0,
                wide(NAME).as_ptr(),
                std::ptr::null(),
                &mut kind,
                value.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if result != ERROR_SUCCESS || kind != REG_SZ {
            return Err(AutostartError::Unavailable);
        }
        value.truncate(length as usize / 2);
        if value.pop() != Some(0) || value.contains(&0) {
            return Err(AutostartError::Unavailable);
        }
        String::from_utf16(&value)
            .map(Some)
            .map_err(|_| AutostartError::Unavailable)
    }
    fn write(&self, value: &str) -> Result<(), AutostartError> {
        let mut key = std::ptr::null_mut();
        let result = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN).as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if result != ERROR_SUCCESS {
            return Err(AutostartError::Unavailable);
        }
        let key = Key(key);
        let value = wide(value);
        let result = unsafe {
            RegSetValueExW(
                key.0,
                wide(NAME).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(AutostartError::Unavailable);
        }
        Ok(())
    }
    fn remove(&self) -> Result<(), AutostartError> {
        let mut key = std::ptr::null_mut();
        let result = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(RUN).as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut key,
            )
        };
        if result == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        if result != ERROR_SUCCESS {
            return Err(AutostartError::Unavailable);
        }
        let key = Key(key);
        let result = unsafe { RegDeleteValueW(key.0, wide(NAME).as_ptr()) };
        if result != ERROR_SUCCESS && result != ERROR_FILE_NOT_FOUND {
            return Err(AutostartError::Unavailable);
        }
        Ok(())
    }
}
