mod transaction;
pub mod windows;
use crate::platform::background_windows::{
    BackgroundError, OperationGuard, atomic_write, reject_link,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};
use transaction::Checkpoint;

pub const PACKAGE_FILES: [&str; 4] = [
    "codex-zectrix-dashboard.exe",
    "codex-zectrix-companion.exe",
    "LICENSE",
    "快速入门.txt",
];
pub const DESKTOP_PACKAGE_FILES: [&str; 6] = [
    "codex-zectrix-dashboard.exe",
    "codex-zectrix-companion.exe",
    "NOTE4控制台.exe",
    "LICENSE",
    "THIRD-PARTY-NOTICES.txt",
    "快速入门.txt",
];
pub const SNAPSHOT_FILES: [&str; 5] = [
    "settings.json",
    "status-frame-state.json",
    "runtime-status.json",
    "pending-preview.png",
    "legacy-migration.json",
];
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("程序版本已变化，请打开新版控制台")]
    InstallationChanged,
    #[error("发布包不完整、版本不匹配或校验失败，请重新下载完整 ZIP")]
    InvalidPackage,
    #[error("安装记录格式不受支持，未修改现有安装")]
    InvalidState,
    #[error("旧版后台无法安全停止，保留原安装；请处理旧启动项后重试")]
    LegacyStop,
    #[error("新版后台本地检查失败，正在保留或恢复原版")]
    Health,
    #[error("自动恢复未完成，后台已阻止上传，请重新运行程序修复")]
    Recovery,
    #[error("安装文件读写失败")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Background(#[from] BackgroundError),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: u32,
    pub version: String,
    pub platform: String,
    pub files: BTreeMap<String, String>,
}
impl PackageManifest {
    pub fn allowed_files(&self) -> Result<&'static [&'static str], InstallError> {
        match self.schema_version {
            1 => Ok(&PACKAGE_FILES),
            2 => Ok(&DESKTOP_PACKAGE_FILES),
            _ => Err(InstallError::InvalidPackage),
        }
    }
}
pub struct Package {
    pub directory: PathBuf,
    pub manifest: PackageManifest,
}
pub fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 48
        && version.as_bytes()[0].is_ascii_digit()
        && !version.contains("..")
        && version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
}
impl Package {
    pub fn load(directory: &Path) -> Result<Self, InstallError> {
        reject_link(directory)?;
        reject_link(&directory.join("package.json"))?;
        let manifest: PackageManifest =
            serde_json::from_slice(&fs::read(directory.join("package.json"))?)
                .map_err(|_| InstallError::InvalidPackage)?;
        let allowed = manifest.allowed_files()?;
        if manifest.platform != "windows-x86_64"
            || !valid_version(&manifest.version)
            || manifest.files.len() != allowed.len()
        {
            return Err(InstallError::InvalidPackage);
        }
        for &name in allowed {
            let path = directory.join(name);
            reject_link(&path)?;
            if !path.is_file() || fs::metadata(&path)?.len() > 64 * 1024 * 1024 {
                return Err(InstallError::InvalidPackage);
            }
            let expected = manifest
                .files
                .get(name)
                .ok_or(InstallError::InvalidPackage)?;
            if expected.len() != 64
                || format!("{:x}", Sha256::digest(fs::read(path)?)) != expected.to_ascii_lowercase()
            {
                return Err(InstallError::InvalidPackage);
            }
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            if name != "package.json" && !allowed.iter().any(|allowed| name == *allowed) {
                return Err(InstallError::InvalidPackage);
            }
        }
        Ok(Self {
            directory: fs::canonicalize(directory)?,
            manifest,
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Installation {
    pub schema_version: u32,
    pub active_version: String,
    pub previous_version: Option<String>,
}
impl Installation {
    pub fn load(data: &Path) -> Result<Option<Self>, InstallError> {
        reject_link(&data.join("installation.json"))?;
        let Some(bytes) = read_optional(&data.join("installation.json"))? else {
            return Ok(None);
        };
        let state: Self = serde_json::from_slice(&bytes).map_err(|_| InstallError::InvalidState)?;
        if state.schema_version != 1
            || !valid_version(&state.active_version)
            || state
                .previous_version
                .as_deref()
                .is_some_and(|v| !valid_version(v))
        {
            return Err(InstallError::InvalidState);
        }
        Ok(Some(state))
    }
    pub fn directory(&self, data: &Path) -> PathBuf {
        data.join("app").join(&self.active_version)
    }
    pub fn companion(&self, data: &Path) -> PathBuf {
        self.directory(data).join("codex-zectrix-companion.exe")
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostSnapshot {
    pub startup: Option<String>,
    pub running_executable: Option<PathBuf>,
    pub legacy: Option<windows::LegacyTask>,
}
pub trait InstallHost {
    fn snapshot(&mut self) -> Result<HostSnapshot, InstallError>;
    fn verify_binaries(&mut self, directory: &Path, version: &str) -> Result<(), InstallError>;
    fn quiesce(&mut self, previous: &HostSnapshot) -> Result<(), InstallError>;
    fn activate(
        &mut self,
        executable: &Path,
        previous: &HostSnapshot,
        guard: &OperationGuard,
    ) -> Result<(), InstallError>;
    fn restore(
        &mut self,
        previous: &HostSnapshot,
        guard: &OperationGuard,
    ) -> Result<(), InstallError>;
}
#[derive(Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    AlreadyInstalled,
}
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryOutcome {
    NothingPending,
    Restored,
}
pub struct InstallManager<H> {
    data: PathBuf,
    host: H,
}
impl<H: InstallHost> InstallManager<H> {
    pub fn new(data: &Path, host: H) -> Self {
        Self {
            data: data.into(),
            host,
        }
    }
    pub fn host(&self) -> &H {
        &self.host
    }
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }
    pub fn install_or_update(&mut self, directory: &Path) -> Result<InstallOutcome, InstallError> {
        let guard = OperationGuard::acquire(&self.data)?;
        self.install_guarded(directory, &guard)
    }
    /// Compare the GUI's observed version under the transaction lock.
    pub fn install_if_current(
        &mut self,
        directory: &Path,
        expected: Option<&str>,
    ) -> Result<InstallOutcome, InstallError> {
        let guard = OperationGuard::acquire(&self.data)?;
        let current = Installation::load(&self.data)?;
        if current.as_ref().map(|i| i.active_version.as_str()) != expected {
            return Err(InstallError::InstallationChanged);
        }
        self.install_guarded(directory, &guard)
    }
    fn install_guarded(
        &mut self,
        directory: &Path,
        guard: &OperationGuard,
    ) -> Result<InstallOutcome, InstallError> {
        self.recover_guarded(guard)?;
        reject_link(&self.data.join("app"))?;
        let package = Package::load(directory)?;
        self.host
            .verify_binaries(&package.directory, &package.manifest.version)?;
        let old = Installation::load(&self.data)?;
        if let Some(old) = &old
            && old.active_version == package.manifest.version
        {
            let installed = Package::load(&old.directory(&self.data))?;
            if installed.manifest.files != package.manifest.files {
                return Err(InstallError::InvalidPackage);
            }
            return Ok(InstallOutcome::AlreadyInstalled);
        }
        let previous = self.host.snapshot()?;
        let mut checkpoint = Checkpoint::new(&self.data, previous.clone())?;
        checkpoint.save(&self.data)?; // Before changing even the legacy startup entry.
        let result = (|| {
            self.host.quiesce(&previous)?;
            checkpoint.capture_files(&self.data)?;
            checkpoint.save(&self.data)?;
            let version_dir = self.data.join("app").join(&package.manifest.version);
            reject_link(&version_dir)?;
            if version_dir.exists() {
                if Package::load(&version_dir)?.manifest.files != package.manifest.files {
                    return Err(InstallError::InvalidPackage);
                }
            } else {
                fs::create_dir_all(self.data.join("app"))?;
                let stage = self.data.join("app").join(format!(
                    ".stage-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                ));
                fs::create_dir(&stage)?;
                let copied = (|| {
                    for name in package
                        .manifest
                        .allowed_files()?
                        .iter()
                        .copied()
                        .chain(["package.json"])
                    {
                        atomic_write(&stage.join(name), &fs::read(package.directory.join(name))?)?;
                    }
                    Package::load(&stage)?;
                    fs::rename(&stage, &version_dir)?;
                    Ok::<(), InstallError>(())
                })();
                if copied.is_err() {
                    for name in package
                        .manifest
                        .allowed_files()?
                        .iter()
                        .copied()
                        .chain(["package.json"])
                    {
                        let _ = fs::remove_file(stage.join(name));
                    }
                    let _ = fs::remove_dir(&stage);
                }
                copied?;
            }
            let new = Installation {
                schema_version: 1,
                active_version: package.manifest.version,
                previous_version: old.map(|i| i.active_version),
            };
            atomic_write(
                &self.data.join("installation.json"),
                &serde_json::to_vec(&new).map_err(|_| InstallError::InvalidState)?,
            )?;
            self.host
                .activate(&new.companion(&self.data), &previous, guard)?;
            fs::remove_file(self.data.join("installation-pending.json"))?;
            Ok(InstallOutcome::Installed)
        })();
        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                if self.restore_checkpoint(&checkpoint, guard).is_err() {
                    return Err(InstallError::Recovery);
                }
                Err(error)
            }
        }
    }
    pub fn recover(&mut self) -> Result<RecoveryOutcome, InstallError> {
        let guard = OperationGuard::acquire(&self.data)?;
        self.recover_guarded(&guard)
    }
    fn recover_guarded(&mut self, guard: &OperationGuard) -> Result<RecoveryOutcome, InstallError> {
        let Some(checkpoint) = Checkpoint::load(&self.data)? else {
            return Ok(RecoveryOutcome::NothingPending);
        };
        self.restore_checkpoint(&checkpoint, guard)?;
        Ok(RecoveryOutcome::Restored)
    }
    fn restore_checkpoint(
        &mut self,
        checkpoint: &Checkpoint,
        guard: &OperationGuard,
    ) -> Result<(), InstallError> {
        // Stop a candidate before restoring data that an older executable will consume.
        if checkpoint.has_data_snapshot() {
            if let Ok(current) = self.host.snapshot() {
                self.host.quiesce(&current)?;
            } else {
                return Err(InstallError::Recovery);
            }
        }
        checkpoint.restore_files(&self.data)?;
        self.host.restore(&checkpoint.host, guard)?;
        fs::remove_file(self.data.join("installation-pending.json"))?;
        Ok(())
    }
    #[cfg(feature = "test-support")]
    pub fn prepare_recovery_checkpoint(&mut self) -> Result<(), InstallError> {
        let _guard = OperationGuard::acquire(&self.data)?;
        let snapshot = self.host.snapshot()?;
        let mut checkpoint = Checkpoint::new(&self.data, snapshot.clone())?;
        checkpoint.save(&self.data)?;
        self.host.quiesce(&snapshot)?;
        checkpoint.capture_files(&self.data)?;
        checkpoint.save(&self.data)
    }
}
pub(crate) fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    reject_link(path)?;
    match fs::read(path) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
pub(crate) fn restore_file(path: &Path, bytes: Option<&[u8]>) -> io::Result<()> {
    reject_link(path)?;
    match bytes {
        Some(bytes) => atomic_write(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        },
    }
}
