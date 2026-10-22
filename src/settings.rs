use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

/// Separate from device settings so older binaries can still read their routing configuration.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppearanceSettings {
    pub theme: crate::status_board::BoardTheme,
}
impl AppearanceSettings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let path = path.as_ref();
        #[cfg(windows)]
        crate::platform::background_windows::reject_link(path).map_err(|_| SettingsError::Read)?;
        match fs::metadata(path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Ok(meta) if meta.is_file() && meta.len() <= 4096 => {}
            _ => return Err(SettingsError::Read),
        }
        serde_json::from_slice(&fs::read(path).map_err(|_| SettingsError::Read)?)
            .map_err(|_| SettingsError::Decode)
    }
    pub fn save_atomically(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        save_json_atomically(self, path.as_ref())
    }
}

pub(crate) fn save_json_atomically(
    value: &impl serde::Serialize,
    path: &Path,
) -> Result<(), SettingsError> {
    #[cfg(windows)]
    crate::platform::background_windows::reject_link(path).map_err(|_| SettingsError::Write)?;
    let bytes = serde_json::to_vec(value).map_err(|_| SettingsError::Encode)?;
    let temporary = write_temporary(path, &bytes)?;
    if replace_file_atomically(&temporary, path).is_err() {
        let _ = fs::remove_file(temporary);
        return Err(SettingsError::Write);
    }
    Ok(())
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub device_id: String,
    pub page_id: u8,
}

impl fmt::Debug for Settings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Settings")
            .field("device_id", &"[redacted]")
            .field("page_id", &self.page_id)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeStatus {
    pub last_successful_task_refresh_epoch_seconds: Option<i64>,
    pub last_successful_quota_refresh_epoch_seconds: Option<i64>,
    pub last_successful_upload_epoch_seconds: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum RuntimeStatusError {
    #[error("unable to read runtime status")]
    Read,
    #[error("unable to decode runtime status")]
    Decode,
    #[error("unable to encode runtime status")]
    Encode,
    #[error("unable to write runtime status")]
    Write,
    #[error("runtime status timestamp is invalid")]
    InvalidTimestamp,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("settings device id is invalid")]
    InvalidDeviceId,
    #[error("settings page id must be between 1 and 5")]
    InvalidPageId,
    #[error("unable to read settings")]
    Read,
    #[error("unable to decode settings")]
    Decode,
    #[error("unable to encode settings")]
    Encode,
    #[error("unable to write settings")]
    Write,
    #[error("unable to remove settings")]
    Remove,
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Option<Self>, SettingsError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SettingsError::Read),
        };
        let settings = serde_json::from_slice::<Self>(&bytes).map_err(|_| SettingsError::Decode)?;
        settings.validate()?;
        Ok(Some(settings))
    }

    pub fn save_atomically(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        self.validate()?;
        let path = path.as_ref();
        let serialized = serde_json::to_vec_pretty(self).map_err(|_| SettingsError::Encode)?;
        let temporary = write_temporary(path, &serialized)?;
        if replace_file_atomically(&temporary, path).is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(SettingsError::Write);
        }
        Ok(())
    }

    pub fn remove(path: impl AsRef<Path>) -> Result<(), SettingsError> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SettingsError::Remove),
        }
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if !routing_id_is_safe(&self.device_id) {
            return Err(SettingsError::InvalidDeviceId);
        }
        if !(1..=5).contains(&self.page_id) {
            return Err(SettingsError::InvalidPageId);
        }
        Ok(())
    }
}

impl RuntimeStatus {
    pub fn load(path: impl AsRef<Path>) -> Result<Option<Self>, RuntimeStatusError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RuntimeStatusError::Read),
        };
        let status =
            serde_json::from_slice::<Self>(&bytes).map_err(|_| RuntimeStatusError::Decode)?;
        status.validate()?;
        Ok(Some(status))
    }

    pub fn save_atomically(&self, path: impl AsRef<Path>) -> Result<(), RuntimeStatusError> {
        self.validate()?;
        let path = path.as_ref();
        let serialized = serde_json::to_vec(self).map_err(|_| RuntimeStatusError::Encode)?;
        let temporary =
            write_temporary(path, &serialized).map_err(|_| RuntimeStatusError::Write)?;
        if replace_file_atomically(&temporary, path).is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(RuntimeStatusError::Write);
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), RuntimeStatusError> {
        let valid = [
            self.last_successful_task_refresh_epoch_seconds,
            self.last_successful_quota_refresh_epoch_seconds,
            self.last_successful_upload_epoch_seconds,
        ]
        .into_iter()
        .flatten()
        .all(epoch_seconds_is_valid);
        valid
            .then_some(())
            .ok_or(RuntimeStatusError::InvalidTimestamp)
    }
}

fn routing_id_is_safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

pub(crate) const fn epoch_seconds_is_valid(value: i64) -> bool {
    value >= 0 && value <= 253_402_300_799
}

static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_temporary(path: &Path, contents: &[u8]) -> Result<PathBuf, SettingsError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    for _ in 0..32 {
        let sequence = TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".settings-{}-{sequence}.tmp", std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(SettingsError::Write),
        };
        if file.write_all(contents).is_err() || file.sync_all().is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(SettingsError::Write);
        }
        return Ok(temporary);
    }
    Err(SettingsError::Write)
}

#[cfg(windows)]
fn replace_file_atomically(temporary: &Path, path: &Path) -> std::io::Result<()> {
    match fs::metadata(path) {
        Ok(_) => replace_existing_file(temporary, path),
        Err(error) if error.kind() == ErrorKind::NotFound => match fs::rename(temporary, path) {
            Ok(()) => Ok(()),
            Err(rename_error) => match fs::metadata(path) {
                Ok(_) => replace_existing_file(temporary, path),
                Err(_) => Err(rename_error),
            },
        },
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn replace_existing_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let destination: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let replaced = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if replaced == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file_atomically(temporary: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temporary, path)
}
