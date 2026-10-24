use super::background_windows::{OperationGuard, reject_link};
use crate::install::{Installation, Package};
use std::{
    ffi::OsString,
    fs,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize, IPersistFile, STGM_READ,
        },
        UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink},
    },
    core::{Interface, PCWSTR, w},
};

#[derive(Debug, thiserror::Error)]
#[error("开始菜单快捷方式操作未完成")]
pub struct ShortcutError;

#[derive(Debug, PartialEq, Eq)]
pub enum ShortcutOutcome {
    Created,
    Updated,
    Removed,
    Absent,
    PreservedForeign,
}

struct LinkDetails {
    target: PathBuf,
    arguments: OsString,
    working: PathBuf,
}

pub struct ShortcutManager {
    programs: PathBuf,
    app_root: PathBuf,
}
impl ShortcutManager {
    pub fn new(programs_dir: PathBuf, app_root: PathBuf) -> Self {
        Self {
            programs: programs_dir,
            app_root,
        }
    }
    pub fn for_current_user(data: &Path) -> Result<Self, ShortcutError> {
        let roaming = std::env::var_os("APPDATA").ok_or(ShortcutError)?;
        Ok(Self::new(
            PathBuf::from(roaming).join("Microsoft/Windows/Start Menu/Programs"),
            data.join("app"),
        ))
    }
    fn link(&self) -> PathBuf {
        self.programs.join("Codex NOTE4.lnk")
    }
    fn valid_gui(&self, gui: &Path) -> bool {
        let Some(parent) = gui.parent() else {
            return false;
        };
        let Some(root) = parent.parent() else {
            return false;
        };
        if !gui.is_absolute()
            || gui.file_name().and_then(|n| n.to_str()) != Some("NOTE4控制台.exe")
            || [&self.app_root, parent, gui]
                .iter()
                .any(|p| reject_link(p).is_err())
        {
            return false;
        }
        match (
            fs::canonicalize(root),
            fs::canonicalize(&self.app_root),
            Package::load(parent),
        ) {
            (Ok(a), Ok(b), Ok(package)) => {
                a == b
                    && package.manifest.schema_version == 2
                    && parent.file_name().and_then(|n| n.to_str())
                        == Some(package.manifest.version.as_str())
            }
            _ => false,
        }
    }
    fn owned(&self) -> Result<bool, ShortcutError> {
        let path = shell_path(&self.link())?;
        let details = with_shell_link(|link| {
            let file: IPersistFile = link.cast()?;
            let mut target = vec![0u16; 32768];
            let mut arguments = vec![0u16; 32768];
            let mut working = vec![0u16; 32768];
            // All buffers are live UTF-16 buffers; no target resolution or execution occurs.
            unsafe {
                file.Load(PCWSTR(path.as_ptr()), STGM_READ)?;
                link.GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)?;
                link.GetArguments(&mut arguments)?;
                link.GetWorkingDirectory(&mut working)?;
            }
            Ok((target, arguments, working))
        })?;
        let details = LinkDetails {
            target: PathBuf::from(read_wide(&details.0)?),
            arguments: read_wide(&details.1)?,
            working: PathBuf::from(read_wide(&details.2)?),
        };
        Ok(details.arguments.is_empty()
            && self.valid_gui(&details.target)
            && details.target.parent().is_some_and(|parent| {
                match (fs::canonicalize(parent), fs::canonicalize(&details.working)) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                }
            }))
    }
    fn validate_link(&self) -> Result<(), ShortcutError> {
        reject_link(&self.programs).map_err(|_| ShortcutError)?;
        reject_link(&self.link()).map_err(|_| ShortcutError)?;
        Ok(())
    }
    pub fn ensure(&self, installed_gui: &Path) -> Result<ShortcutOutcome, ShortcutError> {
        self.validate_link()?;
        if !self.valid_gui(installed_gui) {
            return Err(ShortcutError);
        }
        let exists = self.link().exists();
        if exists && !self.owned()? {
            return Ok(ShortcutOutcome::PreservedForeign);
        }
        fs::create_dir_all(&self.programs).map_err(|_| ShortcutError)?;
        let path = shell_path(&self.link())?;
        let target = shell_path(installed_gui)?;
        let working = shell_path(installed_gui.parent().ok_or(ShortcutError)?)?;
        with_shell_link(|link| {
            let file: IPersistFile = link.cast()?;
            // Use the Unicode interface: WScript.Shell rejects Chinese targets on
            // some Windows locales. The validated buffers live through each call.
            unsafe {
                link.SetPath(PCWSTR(target.as_ptr()))?;
                link.SetArguments(w!(""))?;
                link.SetWorkingDirectory(PCWSTR(working.as_ptr()))?;
                link.SetDescription(w!("NOTE4 控制台"))?;
                file.Save(PCWSTR(path.as_ptr()), true)
            }
        })?;
        Ok(if exists {
            ShortcutOutcome::Updated
        } else {
            ShortcutOutcome::Created
        })
    }
    pub fn remove_owned(&self) -> Result<ShortcutOutcome, ShortcutError> {
        self.validate_link()?;
        if !self.link().exists() {
            return Ok(ShortcutOutcome::Absent);
        }
        if !self.owned()? {
            return Ok(ShortcutOutcome::PreservedForeign);
        }
        fs::remove_file(self.link()).map_err(|_| ShortcutError)?;
        Ok(ShortcutOutcome::Removed)
    }
}
fn shell_path(path: &Path) -> Result<Vec<u16>, ShortcutError> {
    // Normalize canonical verbatim prefixes only at the Shell boundary. Keep
    // UTF-16 intact, including paths not representable in the system code page.
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    if wide.starts_with(&unc) {
        wide.splice(..unc.len(), [b'\\' as u16, b'\\' as u16]);
    } else if wide.starts_with(&prefix) {
        wide.drain(..prefix.len());
    }
    if wide.contains(&0) {
        return Err(ShortcutError);
    }
    wide.push(0);
    Ok(wide)
}
fn read_wide(buffer: &[u16]) -> Result<OsString, ShortcutError> {
    let end = buffer.iter().position(|&c| c == 0).ok_or(ShortcutError)?;
    if end == buffer.len() - 1 {
        // Refuse potentially truncated ownership data.
        return Err(ShortcutError);
    }
    Ok(OsString::from_wide(&buffer[..end]))
}
fn with_shell_link<T>(
    operation: impl FnOnce(&IShellLinkW) -> windows::core::Result<T>,
) -> Result<T, ShortcutError> {
    struct Apartment(bool);
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                // Balance only our successful CoInitializeEx, on this same thread.
                unsafe { CoUninitialize() };
            }
        }
    }
    // Existing GUI threads may already be in an STA; keep their apartment.
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
        return Err(ShortcutError);
    }
    let _apartment = Apartment(initialized.is_ok());
    // COM is initialized and the interface remains on this thread. Declaration
    // order drops the link (including on errors) before the apartment guard.
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }
        .map_err(|_| ShortcutError)?;
    operation(&link).map_err(|_| ShortcutError)
}
/// Shortcut failure does not roll back a successfully committed installation.
pub fn ensure_installed_shortcut(data: &Path) -> Option<String> {
    let result = (|| {
        let _guard = OperationGuard::acquire(data).map_err(|_| ShortcutError)?;
        let installation = Installation::load(data)
            .map_err(|_| ShortcutError)?
            .ok_or(ShortcutError)?;
        if installation.active_version != env!("CARGO_PKG_VERSION") {
            return Err(ShortcutError);
        }
        let package = Package::load(&installation.directory(data)).map_err(|_| ShortcutError)?;
        if package.manifest.schema_version == 1 {
            return Ok(ShortcutOutcome::Absent);
        }
        ShortcutManager::for_current_user(data)?
            .ensure(&installation.directory(data).join("NOTE4控制台.exe"))
    })();
    match result {
        Ok(ShortcutOutcome::PreservedForeign) => Some("安装成功；同名开始菜单快捷方式已被修改，已保留。可直接打开安装目录中的控制台。".into()),
        Ok(_) => None,
        Err(_) => Some("安装成功，但开始菜单快捷方式未更新。可再次点击安装重试，或直接打开安装目录中的控制台。".into()),
    }
}
