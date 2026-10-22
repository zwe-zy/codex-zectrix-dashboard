use std::env;
use std::path::{Path, PathBuf};

use super::PlatformError;

pub fn application_data_dir(
    override_dir: Option<&Path>,
    local_app_data: Option<&Path>,
    home: Option<&Path>,
) -> Result<PathBuf, PlatformError> {
    if let Some(path) = override_dir {
        return Ok(path.to_owned());
    }
    #[cfg(windows)]
    if let Some(path) = local_app_data {
        return Ok(path.join("CodexZectrixDashboard"));
    }
    #[cfg(target_os = "macos")]
    if let Some(path) = home {
        return Ok(path.join("Library/Application Support/codex-zectrix-dashboard"));
    }
    let _ = (local_app_data, home);
    Err(PlatformError::MissingDataDirectory)
}

pub fn data_dir() -> Result<PathBuf, PlatformError> {
    application_data_dir(
        env::var_os("CODEX_ZECTRIX_DATA_DIR")
            .as_deref()
            .map(Path::new),
        env::var_os("LOCALAPPDATA").as_deref().map(Path::new),
        env::var_os("HOME").as_deref().map(Path::new),
    )
}
