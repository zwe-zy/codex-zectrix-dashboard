use super::background_windows::{OperationGuard, reject_link};
use crate::install::{Installation, Package};
use serde::Deserialize;
use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

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

#[derive(Deserialize)]
struct LinkDetails {
    target: PathBuf,
    arguments: String,
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
        let details: LinkDetails = serde_json::from_slice(&powershell(&format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut({}); @{{target=$s.TargetPath;arguments=$s.Arguments;working=$s.WorkingDirectory}} | ConvertTo-Json -Compress", quote(&self.link())
        ))?).map_err(|_| ShortcutError)?;
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
        powershell(&format!(
            "$s=(New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath={}; $s.Arguments=''; $s.WorkingDirectory={}; $s.Description='NOTE4 控制台'; $s.Save()",
            quote(&self.link()),
            quote(installed_gui),
            quote(installed_gui.parent().ok_or(ShortcutError)?)
        ))?;
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
fn quote(path: &Path) -> String {
    // WScript.Shell does not accept Rust's canonical verbatim (\\?\) paths.
    // Normalize only at this COM boundary, after the ownership/path checks.
    let path = path.to_string_lossy();
    let shell_path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(&path).to_owned()
    };
    format!("'{}'", shell_path.replace('\'', "''"))
}
fn powershell(code: &str) -> Result<Vec<u8>, ShortcutError> {
    let script = format!(
        "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); {code}"
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|_| ShortcutError)?;
    if !output.status.success() {
        return Err(ShortcutError);
    }
    Ok(output.stdout)
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
