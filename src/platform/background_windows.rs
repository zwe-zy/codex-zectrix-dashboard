//! Per-data-directory coordination. Lock handles, not PID files, own the lifetime.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{ERROR_SHARING_VIOLATION, FILETIME},
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        MoveFileExW,
    },
    System::Threading::{
        CREATE_NO_WINDOW, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum BackgroundError {
    #[error("正在进行另一项操作，请稍后重试")]
    Busy,
    #[error("后台身份无法核实，未向该进程发送停止请求")]
    IdentityMismatch,
    #[error("配置中断，需要重新运行配置向导")]
    RecoveryRequired,
    #[error("后台未能在规定时间内启动或退出")]
    Timeout,
    #[error("本地后台控制不可用")]
    Io(#[from] io::Error),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundStatus {
    Stopped,
    Starting,
    Running,
}

pub(crate) fn reject_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            Err(io::Error::other("reparse point rejected"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
fn root(path: &Path) -> Result<PathBuf, BackgroundError> {
    reject_link(path)?;
    fs::create_dir_all(path)?;
    Ok(fs::canonicalize(path)?)
}
fn lock(path: &Path) -> Result<File, BackgroundError> {
    reject_link(path)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) {
                BackgroundError::Busy
            } else {
                BackgroundError::Io(e)
            }
        })
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
fn token() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    reject_link(path)?;
    let temporary = path.with_file_name(format!(".{}.tmp", token()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        let wide = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<u16>>()
        };
        let result = unsafe {
            MoveFileExW(
                wide(&temporary).as_ptr(),
                wide(path).as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), BackgroundError> {
    atomic_write(path, &serde_json::to_vec(value).map_err(io::Error::other)?)?;
    Ok(())
}

pub struct OperationGuard {
    directory: PathBuf,
    token: String,
    _lock: File,
}
impl OperationGuard {
    pub fn acquire(directory: &Path) -> Result<Self, BackgroundError> {
        let directory = root(directory)?;
        let handle = lock(&directory.join("operation.lock"))?;
        let token = token();
        atomic_write(&directory.join("operation-token"), token.as_bytes())?;
        Ok(Self {
            directory,
            token,
            _lock: handle,
        })
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("operation-token"));
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Identity {
    pid: u32,
    created_at: u64,
    executable: PathBuf,
    nonce: String,
    ready: bool,
}
pub(crate) fn process_identity(pid: u32) -> Result<(u64, PathBuf), BackgroundError> {
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(BackgroundError::IdentityMismatch);
    }
    // Own only the query handle; this module never forcibly terminates arbitrary PIDs.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let mut created: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit = created;
    let mut kernel = created;
    let mut user = created;
    if unsafe {
        GetProcessTimes(
            handle.as_raw_handle().cast(),
            &mut created,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return Err(BackgroundError::IdentityMismatch);
    }
    let mut buffer = vec![0u16; 32768];
    let mut size = buffer.len() as u32;
    if unsafe {
        QueryFullProcessImageNameW(
            handle.as_raw_handle().cast(),
            0,
            buffer.as_mut_ptr(),
            &mut size,
        )
    } == 0
    {
        return Err(BackgroundError::IdentityMismatch);
    }
    let path = PathBuf::from(
        String::from_utf16(&buffer[..size as usize])
            .map_err(|_| BackgroundError::IdentityMismatch)?,
    );
    Ok((
        ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
        fs::canonicalize(path)?,
    ))
}

pub struct InstanceGuard {
    directory: PathBuf,
    identity: Identity,
    _lock: File,
}
impl InstanceGuard {
    pub fn acquire(
        directory: &Path,
        maintenance_token: Option<&str>,
    ) -> Result<Self, BackgroundError> {
        let directory = root(directory)?;
        let operation = match lock(&directory.join("operation.lock")) {
            Ok(handle) => Some(handle),
            Err(BackgroundError::Busy)
                if maintenance_token.is_some_and(|supplied| {
                    fs::read_to_string(directory.join("operation-token"))
                        .ok()
                        .as_deref()
                        == Some(supplied)
                }) =>
            {
                None
            }
            Err(e) => return Err(e),
        };
        if directory.join("configuration-pending.json").exists()
            || (directory.join("installation-pending.json").exists() && operation.is_some())
            || directory.join("uninstall-pending.json").exists()
        {
            return Err(BackgroundError::RecoveryRequired);
        }
        let handle = lock(&directory.join("instance.lock"))?;
        let (created_at, executable) = process_identity(std::process::id())?;
        let identity = Identity {
            pid: std::process::id(),
            created_at,
            executable,
            nonce: token(),
            ready: false,
        };
        write_json(&directory.join("background.json"), &identity)?;
        let _ = fs::remove_file(directory.join("stop-request.json"));
        drop(operation);
        Ok(Self {
            directory,
            identity,
            _lock: handle,
        })
    }
    pub fn mark_ready(&self) -> Result<(), BackgroundError> {
        write_json(
            &self.directory.join("background.json"),
            &Identity {
                ready: true,
                pid: self.identity.pid,
                created_at: self.identity.created_at,
                executable: self.identity.executable.clone(),
                nonce: self.identity.nonce.clone(),
            },
        )
    }
    pub fn stop_requested(&self) -> bool {
        fs::read_to_string(self.directory.join("stop-request.json"))
            .ok()
            .as_deref()
            == Some(self.identity.nonce.as_str())
    }
    pub fn may_publish(&self) -> bool {
        !self.stop_requested()
            && !self.directory.join("configuration-pending.json").exists()
            && !self.directory.join("installation-pending.json").exists()
            && lock(&self.directory.join("operation.lock")).is_ok()
    }
}
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("background.json"));
        let _ = fs::remove_file(self.directory.join("stop-request.json"));
    }
}

pub struct BackgroundController {
    directory: PathBuf,
    executable: PathBuf,
}
impl BackgroundController {
    pub fn new(directory: &Path, executable: &Path) -> Self {
        Self {
            directory: directory.into(),
            executable: executable.into(),
        }
    }
    fn checked_identity(&self) -> Result<Identity, BackgroundError> {
        let bytes = fs::read(self.directory.join("background.json"))?;
        let identity: Identity =
            serde_json::from_slice(&bytes).map_err(|_| BackgroundError::IdentityMismatch)?;
        let (created, executable) = process_identity(identity.pid)?;
        if created != identity.created_at
            || executable != identity.executable
            || executable != fs::canonicalize(&self.executable)?
        {
            return Err(BackgroundError::IdentityMismatch);
        }
        Ok(identity)
    }
    pub fn status(&self) -> Result<BackgroundStatus, BackgroundError> {
        let directory = root(&self.directory)?;
        match lock(&directory.join("instance.lock")) {
            Ok(_) => Ok(BackgroundStatus::Stopped),
            Err(BackgroundError::Busy) => {
                if !directory.join("background.json").exists() {
                    return Ok(BackgroundStatus::Starting);
                }
                Ok(if self.checked_identity()?.ready {
                    BackgroundStatus::Running
                } else {
                    BackgroundStatus::Starting
                })
            }
            Err(e) => Err(e),
        }
    }
    pub fn start(&self) -> Result<BackgroundStatus, BackgroundError> {
        let guard = OperationGuard::acquire(&self.directory)?;
        if self.directory.join("installation-pending.json").exists()
            || self.directory.join("uninstall-pending.json").exists()
        {
            return Err(BackgroundError::RecoveryRequired);
        }
        self.start_guarded(&guard)
    }
    pub fn start_guarded(
        &self,
        guard: &OperationGuard,
    ) -> Result<BackgroundStatus, BackgroundError> {
        if fs::canonicalize(&self.directory)? != guard.directory {
            return Err(BackgroundError::IdentityMismatch);
        }
        if self.directory.join("configuration-pending.json").exists() {
            return Err(BackgroundError::RecoveryRequired);
        }
        match self.status()? {
            BackgroundStatus::Running => return Ok(BackgroundStatus::Running),
            BackgroundStatus::Starting => return Err(BackgroundError::Busy),
            BackgroundStatus::Stopped => {}
        }
        let mut child = Command::new(&self.executable)
            .env("CODEX_ZECTRIX_DATA_DIR", &self.directory)
            .env("CODEX_ZECTRIX_MAINTENANCE_TOKEN", &guard.token)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if self.status()? == BackgroundStatus::Running {
                return Ok(BackgroundStatus::Running);
            }
            if child.try_wait()?.is_some() {
                return Err(BackgroundError::Timeout);
            }
            if Instant::now() >= deadline {
                // This handle identifies only the child just spawned here, not a PID-file target.
                let _ = child.kill();
                let _ = child.wait();
                return Err(BackgroundError::Timeout);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    pub fn stop(&self) -> Result<(), BackgroundError> {
        if self.status()? == BackgroundStatus::Stopped {
            return Ok(());
        }
        let identity = self.checked_identity()?;
        atomic_write(
            &self.directory.join("stop-request.json"),
            identity.nonce.as_bytes(),
        )?;
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if self.status()? == BackgroundStatus::Stopped {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(BackgroundError::Timeout);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}
