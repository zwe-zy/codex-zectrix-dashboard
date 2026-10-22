mod commit;
pub use commit::{PreparedSetup, SetupCommitReport, commit_prepared_setup};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::autostart::{Autostart, AutostartControl, AutostartStatus, SystemCommandRunner};
use crate::credentials::{CredentialStore, OsCredentialStore};
use crate::publisher::{
    StatusFrameAttempt, StatusFrameCoordinator, StatusFramePublishError, StatusFramePublisher,
    StatusFramePublisherState, StatusFrameRenderError, StatusFrameRenderer, UploadCompletion,
};
use crate::settings::{RuntimeStatus, Settings};
use crate::status_board::{
    BoardLocalTime, BoardTaskState, StatusBoardClock, StatusBoardState, render_status_board,
};

pub const FIRST_UPLOAD_CONFIRMATION: &str = "确认首次上传到这块屏幕？[y/N]";
pub const CLOUD_PAYLOAD_SUMMARY: &str =
    "云端仅接收生成的 400×300 PNG 及路由所需的设备 ID 和页面 ID；不包含 Codex 任务标题或内容。";
pub const CANCELLATION_LINE: &str = "已取消，未上传。";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SetupBoundaryError;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AppFile {
    Settings,
    Appearance,
    InkcardSettings,
    InkcardState,
    StatusFrameState,
    RuntimeStatus,
    PendingPreview,
    CompanionLog,
    RotatedCompanionLog,
    LegacyQuotaCache,
    LegacyActivityCache,
    LegacyRadarCache,
    LegacyPublisherState,
    LegacySourceStatus,
    LegacyHookEvents,
    LegacyHookOwners,
    LegacyLifecycle,
    DesktopSnapshot,
    DesktopFrame1,
    DesktopFrame2,
    DesktopFrame3,
    DesktopFrame4,
    DesktopFrame5,
}

impl AppFile {
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::Settings => "settings.json",
            Self::Appearance => "appearance.json",
            Self::InkcardSettings => "inkcards.json",
            Self::InkcardState => "inkcard-state.json",
            Self::StatusFrameState => "status-frame-state.json",
            Self::RuntimeStatus => "runtime-status.json",
            Self::PendingPreview => "pending-preview.png",
            Self::CompanionLog => "companion.log",
            Self::RotatedCompanionLog => "companion.log.1",
            Self::LegacyQuotaCache => "quota.json",
            Self::LegacyActivityCache => "activity.json",
            Self::LegacyRadarCache => "radar.json",
            Self::LegacyPublisherState => "publisher-state.json",
            Self::LegacySourceStatus => "source-status.json",
            Self::LegacyHookEvents => "hook-events.jsonl",
            Self::LegacyHookOwners => "hook-owner-pids",
            Self::LegacyLifecycle => "lifecycle.json",
            Self::DesktopSnapshot => "desktop-status.json",
            Self::DesktopFrame1 => "desktop-frame-1.png",
            Self::DesktopFrame2 => "desktop-frame-2.png",
            Self::DesktopFrame3 => "desktop-frame-3.png",
            Self::DesktopFrame4 => "desktop-frame-4.png",
            Self::DesktopFrame5 => "desktop-frame-5.png",
        }
    }

    pub const fn uninstall_targets() -> &'static [Self] {
        &[
            Self::Appearance,
            Self::InkcardSettings,
            Self::InkcardState,
            Self::Settings,
            Self::StatusFrameState,
            Self::RuntimeStatus,
            Self::PendingPreview,
            Self::CompanionLog,
            Self::RotatedCompanionLog,
            Self::LegacyQuotaCache,
            Self::LegacyActivityCache,
            Self::LegacyRadarCache,
            Self::LegacyPublisherState,
            Self::LegacySourceStatus,
            Self::LegacyHookEvents,
            Self::LegacyHookOwners,
            Self::LegacyLifecycle,
            Self::DesktopSnapshot,
            Self::DesktopFrame1,
            Self::DesktopFrame2,
            Self::DesktopFrame3,
            Self::DesktopFrame4,
            Self::DesktopFrame5,
        ]
    }
}

pub trait SetupFileStore {
    fn read(&self, file: AppFile) -> Result<Option<Vec<u8>>, SetupBoundaryError>;
    fn write(&mut self, file: AppFile, bytes: &[u8]) -> Result<(), SetupBoundaryError>;
    fn remove(&mut self, file: AppFile) -> Result<(), SetupBoundaryError>;
    fn path(&self, file: AppFile) -> &Path;
}

pub struct FilesystemSetupStore {
    paths: HashMap<AppFile, PathBuf>,
}

impl FilesystemSetupStore {
    pub fn new(data_directory: impl AsRef<Path>) -> Result<Self, SetupBoundaryError> {
        let data_directory = data_directory.as_ref();
        let paths = AppFile::uninstall_targets()
            .iter()
            .copied()
            .map(|file| (file, data_directory.join(file.file_name())))
            .collect();
        Ok(Self { paths })
    }

    fn file_path(&self, file: AppFile) -> &Path {
        self.paths
            .get(&file)
            .map(PathBuf::as_path)
            .expect("all app files have scoped paths")
    }
}

impl SetupFileStore for FilesystemSetupStore {
    fn read(&self, file: AppFile) -> Result<Option<Vec<u8>>, SetupBoundaryError> {
        match fs::read(self.file_path(file)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(_) => Err(SetupBoundaryError),
        }
    }

    fn write(&mut self, file: AppFile, bytes: &[u8]) -> Result<(), SetupBoundaryError> {
        write_scoped_atomically(self.file_path(file), bytes)
    }

    fn remove(&mut self, file: AppFile) -> Result<(), SetupBoundaryError> {
        match fs::remove_file(self.file_path(file)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SetupBoundaryError),
        }
    }

    fn path(&self, file: AppFile) -> &Path {
        self.file_path(file)
    }
}

static SETUP_TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn write_scoped_atomically(path: &Path, bytes: &[u8]) -> Result<(), SetupBoundaryError> {
    let directory = path.parent().ok_or(SetupBoundaryError)?;
    fs::create_dir_all(directory).map_err(|_| SetupBoundaryError)?;
    for _ in 0..32 {
        let sequence = SETUP_TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".setup-{}-{sequence}.tmp", std::process::id()));
        let mut output = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(SetupBoundaryError),
        };
        if output.write_all(bytes).is_err() || output.sync_all().is_err() {
            drop(output);
            let _ = fs::remove_file(&temporary);
            return Err(SetupBoundaryError);
        }
        // ReplaceFileW must open the replacement with access incompatible with this writer.
        drop(output);
        if replace_file(&temporary, path).is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(SetupBoundaryError);
        }
        return Ok(());
    }
    Err(SetupBoundaryError)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return fs::rename(temporary, path);
    }
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let destination = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replacement = temporary
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temporary, path)
}

pub trait SetupTerminal {
    fn read_secret(&mut self, prompt: &str) -> Result<Zeroizing<String>, SetupBoundaryError>;
    fn read_line(&mut self, prompt: &str) -> Result<Option<String>, SetupBoundaryError>;
    fn write_line(&mut self, line: &str) -> Result<(), SetupBoundaryError>;
}

pub struct ConsoleTerminal {
    output: Box<dyn Write>,
}

impl ConsoleTerminal {
    pub fn new() -> Self {
        Self::with_output(std::io::stdout())
    }

    pub fn with_output(output: impl Write + 'static) -> Self {
        Self {
            output: Box::new(output),
        }
    }

    fn write_prompt(&mut self, prompt: &str) -> Result<(), SetupBoundaryError> {
        write!(self.output, "{prompt}").map_err(|_| SetupBoundaryError)?;
        self.output.flush().map_err(|_| SetupBoundaryError)
    }
}

impl Default for ConsoleTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupTerminal for ConsoleTerminal {
    fn read_secret(&mut self, prompt: &str) -> Result<Zeroizing<String>, SetupBoundaryError> {
        self.write_prompt(prompt)?;
        let secret = if std::io::stdin().is_terminal() {
            rpassword::read_password().map_err(|_| SetupBoundaryError)?
        } else {
            let mut value = String::new();
            std::io::stdin()
                .read_line(&mut value)
                .map_err(|_| SetupBoundaryError)?;
            value.trim_end_matches(['\r', '\n']).to_owned()
        };
        Ok(Zeroizing::new(secret))
    }

    fn read_line(&mut self, prompt: &str) -> Result<Option<String>, SetupBoundaryError> {
        self.write_prompt(prompt)?;
        let mut value = String::new();
        let read = std::io::stdin()
            .read_line(&mut value)
            .map_err(|_| SetupBoundaryError)?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(value.trim().to_owned()))
    }

    fn write_line(&mut self, line: &str) -> Result<(), SetupBoundaryError> {
        writeln!(self.output, "{line}").map_err(|_| SetupBoundaryError)?;
        self.output.flush().map_err(|_| SetupBoundaryError)
    }
}

pub struct SetupDevice {
    routing_id: String,
    name: Option<String>,
}

impl SetupDevice {
    pub fn routing_id(&self) -> &str {
        &self.routing_id
    }
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or("NOTE4")
    }
    pub fn new(routing_id: impl Into<String>) -> Self {
        Self {
            routing_id: routing_id.into(),
            name: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupCloudError {
    KeyRejected,
    DiscoveryUnavailable,
    UploadRejected,
    UploadResultUnknown,
}

pub trait SetupCloud {
    fn discover(&mut self, secret: &str) -> Result<Vec<SetupDevice>, SetupCloudError>;
    fn upload_png(
        &mut self,
        secret: &str,
        device_id: &str,
        page_id: u8,
        png: &[u8],
    ) -> Result<(), SetupCloudError>;
}

pub struct ZectrixSetupCloud {
    base_url: String,
}

impl ZectrixSetupCloud {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }

    fn client(&self, secret: &str) -> Result<Client, SetupCloudError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-API-Key",
            HeaderValue::from_str(secret).map_err(|_| SetupCloudError::DiscoveryUnavailable)?,
        );
        Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| SetupCloudError::DiscoveryUnavailable)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceResponse {
    device_id: String,
    board: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    code: i64,
    data: T,
}

#[derive(Deserialize)]
struct ApiCodeResponse {
    code: i64,
}

impl SetupCloud for ZectrixSetupCloud {
    fn discover(&mut self, secret: &str) -> Result<Vec<SetupDevice>, SetupCloudError> {
        let response = self
            .client(secret)?
            .get(format!("{}/open/v1/devices", self.base_url))
            .send()
            .map_err(|_| SetupCloudError::DiscoveryUnavailable)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(SetupCloudError::KeyRejected);
        }
        if !response.status().is_success() {
            return Err(SetupCloudError::DiscoveryUnavailable);
        }
        let response = response
            .json::<ApiResponse<Vec<DeviceResponse>>>()
            .map_err(|_| SetupCloudError::DiscoveryUnavailable)?;
        if response.code != 0 {
            return Err(SetupCloudError::DiscoveryUnavailable);
        }
        Ok(response
            .data
            .into_iter()
            .filter(|device| {
                matches!(
                    device.board.as_str(),
                    "zectrix-s3-epaper-4.2" | "bread-compact-wifi"
                )
            })
            .map(|device| SetupDevice {
                routing_id: device.device_id,
                name: device.name,
            })
            .collect())
    }

    fn upload_png(
        &mut self,
        secret: &str,
        device_id: &str,
        page_id: u8,
        png: &[u8],
    ) -> Result<(), SetupCloudError> {
        if png.len() > 2 * 1024 * 1024 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(SetupCloudError::UploadRejected);
        }
        let image = Part::bytes(png.to_vec())
            .file_name("codex-status.png")
            .mime_str("image/png")
            .map_err(|_| SetupCloudError::UploadRejected)?;
        let form = Form::new()
            .text("dither", "false")
            .text("pageId", page_id.to_string())
            .part("images", image);
        let response = self
            .client(secret)
            .map_err(|_| SetupCloudError::UploadResultUnknown)?
            .post(format!(
                "{}/open/v1/devices/{device_id}/display/image",
                self.base_url
            ))
            .multipart(form)
            .send()
            .map_err(|_| SetupCloudError::UploadResultUnknown)?;
        if response.status().is_client_error() {
            return Err(SetupCloudError::UploadRejected);
        }
        if !response.status().is_success() {
            return Err(SetupCloudError::UploadResultUnknown);
        }
        let response = response
            .json::<ApiCodeResponse>()
            .map_err(|_| SetupCloudError::UploadResultUnknown)?;
        if response.code != 0 {
            return Err(SetupCloudError::UploadRejected);
        }
        Ok(())
    }
}

pub struct SetupPreview {
    state: StatusBoardState,
    png: Vec<u8>,
}

impl SetupPreview {
    pub fn png(&self) -> &[u8] {
        &self.png
    }
    pub fn new(state: StatusBoardState, png: Vec<u8>) -> Self {
        Self { state, png }
    }
}

pub trait SetupRenderer {
    fn render_sample(&mut self) -> Result<SetupPreview, SetupBoundaryError>;
}

pub trait SetupClock {
    fn now_epoch_seconds(&self) -> i64;
}

pub struct SystemSetupClock;

impl SetupClock for SystemSetupClock {
    fn now_epoch_seconds(&self) -> i64 {
        crate::platform::now_epoch_seconds().unwrap_or(0)
    }
}

pub struct DeterministicSetupRenderer;

const SAMPLE_NOW: i64 = 1_786_330_000;
const SAMPLE_RESET: i64 = 1_786_333_600;

struct SampleBoardClock;

impl StatusBoardClock for SampleBoardClock {
    fn now_epoch_seconds(&self) -> i64 {
        SAMPLE_NOW
    }

    fn local_time(&self, epoch_seconds: i64) -> Option<BoardLocalTime> {
        let value = time::OffsetDateTime::from_unix_timestamp(epoch_seconds).ok()?;
        BoardLocalTime::new(
            value.year(),
            value.month() as u8,
            value.day(),
            value.hour(),
            value.minute(),
        )
        .ok()
    }
}

impl SetupRenderer for DeterministicSetupRenderer {
    fn render_sample(&mut self) -> Result<SetupPreview, SetupBoundaryError> {
        let state = StatusBoardState {
            theme: Default::default(),
            task_partial: false,
            task_state: BoardTaskState::Empty,
            quota_remaining_percent: Some(64.0),
            quota_window_duration_mins: Some(300),
            quota_secondary: Some(crate::status_board::BoardQuotaWindow {
                remaining_percent: 80.0,
                resets_at_epoch_seconds: Some(SAMPLE_RESET + 86400 * 3),
                window_duration_mins: Some(10080),
                stale: false,
            }),
            is_sample: true,
            quota_resets_at_epoch_seconds: Some(SAMPLE_RESET),
            quota_stale: false,
            task_stale: false,
            uploaded_at_epoch_seconds: None,
        };
        let png = render_status_board(&state, &SampleBoardClock).map_err(|_| SetupBoundaryError)?;
        Ok(SetupPreview::new(state, png))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupOutcome {
    Configured,
    ConfiguredWithAutostartWarning,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetupCommitState {
    Configured,
    Restored,
    Uncertain,
}
pub trait SetupControl {
    fn begin(&mut self) -> Result<(), SetupError>;
    fn finish(&mut self, state: SetupCommitState) -> Result<(), SetupError>;
}
#[cfg(not(windows))]
struct LocalSetupControl;
#[cfg(not(windows))]
impl SetupControl for LocalSetupControl {
    fn begin(&mut self) -> Result<(), SetupError> {
        Ok(())
    }
    fn finish(&mut self, _: SetupCommitState) -> Result<(), SetupError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum SetupError {
    #[error("输入期间程序已升级，请重新打开配置向导；原配置未改动")]
    InstallationChanged,
    #[error("这个账号下没有可用的 NOTE4，请先在 ZECTRIX 中绑定设备")]
    NoDevices,
    #[error("API Key 被拒绝，请检查复制是否完整或重新生成")]
    KeyRejected,
    #[error("后台暂时无法停止，请先在菜单中停止后重试；旧配置未改动")]
    BackgroundUnavailable,
    #[error("配置已保存，但后台未能启动；请在菜单中重试启动")]
    BackgroundStartUnavailable,
    #[error("secret input unavailable")]
    SecretUnavailable,
    #[error("device discovery unavailable")]
    DiscoveryUnavailable,
    #[error("device selection invalid")]
    SelectionInvalid,
    #[error("preview unavailable")]
    PreviewUnavailable,
    #[error("local setup state unavailable")]
    LocalStateUnavailable,
    #[error("credential persistence unavailable")]
    CredentialPersistUnavailable,
    #[error("settings persistence unavailable")]
    SettingsPersistUnavailable,
    #[error("first upload rejected")]
    UploadRejected,
    #[error("first upload result unknown")]
    UploadResultUnknown,
    #[error("publisher state unavailable; screen may already have changed")]
    StatePersistFailedScreenMayHaveChanged,
    #[error(
        "publisher state unavailable; screen may already have changed; reversible local rollback may be incomplete"
    )]
    StatePersistFailedScreenMayHaveChangedRollbackIncomplete,
    #[error("autostart unavailable")]
    AutostartUnavailable,
    #[error("autostart failed; screen may already have changed")]
    AutostartFailedScreenMayHaveChanged,
    #[error(
        "autostart failed; screen may already have changed; reversible local rollback may be incomplete"
    )]
    AutostartFailedScreenMayHaveChangedRollbackIncomplete,
    #[error("setup completed; screen may already have changed; completion notice unavailable")]
    CompletionNoticeFailedScreenMayHaveChanged,
    #[error("setup rollback unavailable")]
    RollbackUnavailable,
}

struct Snapshots {
    settings: Option<Vec<u8>>,
    state: Option<Vec<u8>>,
    preview: Option<Vec<u8>>,
    credential: Option<Zeroizing<String>>,
    autostart: AutostartStatus,
}

// Keep side-effect boundaries explicit so fault-injection tests exercise the real transaction.
#[allow(clippy::too_many_arguments)]
pub fn run_setup_transaction<T, C, S, A, R, K>(
    terminal: &mut T,
    cloud: &mut C,
    credentials: &impl CredentialStore,
    files: &mut S,
    autostart: &mut A,
    renderer: &mut R,
    clock: &K,
    control: &mut impl SetupControl,
) -> Result<SetupOutcome, SetupError>
where
    T: SetupTerminal,
    C: SetupCloud,
    S: SetupFileStore,
    A: AutostartControl,
    R: SetupRenderer,
    K: SetupClock,
{
    let api_key = terminal
        .read_secret("请输入 ZECTRIX API Key（输入不会显示）：")
        .map_err(|_| SetupError::SecretUnavailable)?;
    if api_key.trim().is_empty() {
        return Err(SetupError::SecretUnavailable);
    }
    let devices = cloud.discover(api_key.as_str()).map_err(|error| {
        if error == SetupCloudError::KeyRejected {
            SetupError::KeyRejected
        } else {
            SetupError::DiscoveryUnavailable
        }
    })?;
    if devices.is_empty() {
        return Err(SetupError::NoDevices);
    }
    terminal
        .write_line("可用的 NOTE4 设备：")
        .map_err(|_| SetupError::SelectionInvalid)?;
    for (index, device) in devices.iter().enumerate() {
        let suffix = device
            .routing_id
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        let name = device
            .name
            .as_deref()
            .unwrap_or("NOTE4")
            .chars()
            .filter(|c| !c.is_control())
            .take(32)
            .collect::<String>();
        terminal
            .write_line(&format!("{}. {}（尾号 {}）", index + 1, name, suffix))
            .map_err(|_| SetupError::SelectionInvalid)?;
    }
    let device_index =
        read_bounded_selection(terminal, "选择 NOTE4 设备编号：", 1, devices.len())? - 1;
    let page_id = read_bounded_selection(terminal, "选择页面（1-5）：", 1, 5)? as u8;

    let previous_preview = files
        .read(AppFile::PendingPreview)
        .map_err(|_| SetupError::LocalStateUnavailable)?;
    let preview = renderer
        .render_sample()
        .map_err(|_| SetupError::PreviewUnavailable)?;
    if files.write(AppFile::PendingPreview, &preview.png).is_err() {
        let _ = restore_file(files, AppFile::PendingPreview, previous_preview.as_deref());
        return Err(SetupError::PreviewUnavailable);
    }
    if terminal
        .write_line(&format!(
            "预览已生成：{}",
            files.path(AppFile::PendingPreview).display()
        ))
        .is_err()
        || terminal.write_line(CLOUD_PAYLOAD_SUMMARY).is_err()
    {
        restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
            .map_err(|_| SetupError::LocalStateUnavailable)?;
        return Err(SetupError::PreviewUnavailable);
    }
    let confirmation = match terminal.read_line(FIRST_UPLOAD_CONFIRMATION) {
        Ok(value) => value,
        Err(SetupBoundaryError) => {
            restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
                .map_err(|_| SetupError::LocalStateUnavailable)?;
            return Err(SetupError::SelectionInvalid);
        }
    };
    if !confirmation
        .as_deref()
        .is_some_and(|answer| matches!(answer.trim(), "y" | "Y"))
    {
        restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
            .map_err(|_| SetupError::LocalStateUnavailable)?;
        terminal
            .write_line(CANCELLATION_LINE)
            .map_err(|_| SetupError::LocalStateUnavailable)?;
        return Ok(SetupOutcome::Cancelled);
    }

    let draft = PreparedSetup::new(
        Settings {
            device_id: devices[device_index].routing_id.clone(),
            page_id,
        },
        preview,
        api_key,
        previous_preview,
    )?;
    let report =
        commit_prepared_setup(draft, cloud, credentials, files, autostart, clock, control)?;
    if let Some(warning) = report.background_warning {
        return Err(warning);
    }
    if report.outcome == SetupOutcome::ConfiguredWithAutostartWarning {
        let _ = terminal.write_line("配置和首次上传已成功。开机启动设置失败，可在菜单中重试。");
    } else {
        terminal
            .write_line("首次状态牌已上传，自启动已配置。")
            .map_err(|_| SetupError::CompletionNoticeFailedScreenMayHaveChanged)?;
    }
    Ok(report.outcome)
}
fn read_bounded_selection(
    terminal: &mut impl SetupTerminal,
    prompt: &str,
    minimum: usize,
    maximum: usize,
) -> Result<usize, SetupError> {
    let answer = terminal
        .read_line(prompt)
        .map_err(|_| SetupError::SelectionInvalid)?
        .ok_or(SetupError::SelectionInvalid)?;
    let value = answer
        .trim()
        .parse::<usize>()
        .map_err(|_| SetupError::SelectionInvalid)?;
    (minimum..=maximum)
        .contains(&value)
        .then_some(value)
        .ok_or(SetupError::SelectionInvalid)
}

fn rollback(
    files: &mut impl SetupFileStore,
    credentials: &impl CredentialStore,
    autostart: &mut impl AutostartControl,
    snapshots: &Snapshots,
    install_was_attempted: bool,
) -> Result<(), SetupError> {
    let mut failed = false;
    failed |= restore_file(files, AppFile::Settings, snapshots.settings.as_deref()).is_err();
    failed |= restore_file(files, AppFile::StatusFrameState, snapshots.state.as_deref()).is_err();
    failed |= restore_file(files, AppFile::PendingPreview, snapshots.preview.as_deref()).is_err();
    failed |= match snapshots.credential.as_deref() {
        Some(previous) => credentials.set(previous).is_err(),
        None => credentials.delete().is_err(),
    };
    if install_was_attempted && snapshots.autostart == AutostartStatus::Absent {
        failed |= autostart.uninstall().is_err();
    }
    if failed {
        return Err(SetupError::RollbackUnavailable);
    }
    Ok(())
}

fn restore_file(
    files: &mut impl SetupFileStore,
    file: AppFile,
    previous: Option<&[u8]>,
) -> Result<(), SetupBoundaryError> {
    match previous {
        Some(bytes) => files.write(file, bytes),
        None => files.remove(file),
    }
}

struct ExactPreviewRenderer<'a> {
    png: &'a [u8],
}

impl StatusFrameRenderer for ExactPreviewRenderer<'_> {
    fn render(
        &mut self,
        _state: &StatusBoardState,
        _now_epoch_seconds: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError> {
        Ok(self.png.to_vec())
    }
}

struct FirstUploadPublisher<'a, C, K> {
    cloud: &'a mut C,
    secret: &'a str,
    device_id: &'a str,
    page_id: u8,
    clock: &'a K,
    last_error: Option<SetupCloudError>,
}

impl<C: SetupCloud, K: SetupClock> StatusFramePublisher for FirstUploadPublisher<'_, C, K> {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        match self
            .cloud
            .upload_png(self.secret, self.device_id, self.page_id, png)
        {
            Ok(()) => Ok(UploadCompletion {
                epoch_seconds: self.clock.now_epoch_seconds(),
            }),
            Err(error) => {
                self.last_error = Some(error);
                Err(StatusFramePublishError::TransientUnavailable)
            }
        }
    }
}

pub fn write_deterministic_preview(path: impl AsRef<Path>) -> Result<(), SetupError> {
    let mut renderer = DeterministicSetupRenderer;
    let preview = renderer
        .render_sample()
        .map_err(|_| SetupError::PreviewUnavailable)?;
    fs::write(path, preview.png).map_err(|_| SetupError::PreviewUnavailable)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodexReachability {
    Supported,
    Partial,
    Unavailable,
    MethodUnavailable,
    AccountUnavailable,
    Incompatible,
}

impl CodexReachability {
    const fn label(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
            Self::MethodUnavailable => "method_unavailable",
            Self::AccountUnavailable => "account_unavailable",
            Self::Incompatible => "incompatible",
        }
    }
}

pub trait StatusProbe {
    fn task_reachability(&mut self) -> CodexReachability;
    fn quota_reachability(&mut self) -> CodexReachability;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum OperatorError {
    #[error("operator command unavailable")]
    Unavailable,
}

pub fn run_operator_status(
    terminal: &mut impl SetupTerminal,
    files: &impl SetupFileStore,
    autostart: &mut impl AutostartControl,
    probe: &mut impl StatusProbe,
) -> Result<(), OperatorError> {
    let configured = files
        .read(AppFile::Settings)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .filter(|settings| settings.validate().is_ok())
        .is_some_and(|settings| (1..=5).contains(&settings.page_id));
    let mut runtime = files
        .read(AppFile::RuntimeStatus)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<RuntimeStatus>(&bytes).ok())
        .filter(|status| status.validate().is_ok())
        .unwrap_or_default();
    if runtime.last_successful_upload_epoch_seconds.is_none() {
        runtime.last_successful_upload_epoch_seconds = files
            .read(AppFile::StatusFrameState)
            .ok()
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<StatusFramePublisherState>(&bytes).ok())
            .and_then(|state| state.last_successful_upload_epoch_seconds)
            .filter(|value| crate::settings::epoch_seconds_is_valid(*value));
    }
    let autostart = match autostart.status() {
        AutostartStatus::Present => "present",
        AutostartStatus::Absent => "absent",
        AutostartStatus::Unavailable => "unavailable",
    };
    let lines = [
        format!(
            "setup={}",
            if configured {
                "configured"
            } else {
                "not_configured"
            }
        ),
        format!("companion_autostart={autostart}"),
        format!("codex_task_method={}", probe.task_reachability().label()),
        format!("codex_quota_method={}", probe.quota_reachability().label()),
        format!(
            "last_successful_task_refresh={}",
            optional_timestamp(runtime.last_successful_task_refresh_epoch_seconds)
        ),
        format!(
            "last_successful_quota_refresh={}",
            optional_timestamp(runtime.last_successful_quota_refresh_epoch_seconds)
        ),
        format!(
            "last_successful_upload={}",
            optional_timestamp(runtime.last_successful_upload_epoch_seconds)
        ),
    ];
    for line in lines {
        terminal
            .write_line(&line)
            .map_err(|_| OperatorError::Unavailable)?;
    }
    Ok(())
}

fn optional_timestamp(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unavailable".into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UninstallOutcome {
    RemovedWithCredential,
    RemovedCredentialRetained,
}

pub fn run_uninstall(
    terminal: &mut impl SetupTerminal,
    credentials: &impl CredentialStore,
    files: &mut impl SetupFileStore,
    autostart: &mut impl AutostartControl,
) -> Result<UninstallOutcome, OperatorError> {
    autostart
        .uninstall()
        .map_err(|_| OperatorError::Unavailable)?;
    for file in AppFile::uninstall_targets() {
        files
            .remove(*file)
            .map_err(|_| OperatorError::Unavailable)?;
    }
    let delete_credential = terminal
        .read_line("同时删除 Windows 凭据管理器中的 API Key？[y/N]")
        .map_err(|_| OperatorError::Unavailable)?
        .as_deref()
        .is_some_and(|answer| matches!(answer.trim(), "y" | "Y"));
    if delete_credential {
        credentials
            .delete()
            .map_err(|_| OperatorError::Unavailable)?;
        terminal
            .write_line("本地配置和 API Key 已移除。")
            .map_err(|_| OperatorError::Unavailable)?;
        Ok(UninstallOutcome::RemovedWithCredential)
    } else {
        terminal
            .write_line("本地配置已移除，API Key 已保留。")
            .map_err(|_| OperatorError::Unavailable)?;
        Ok(UninstallOutcome::RemovedCredentialRetained)
    }
}

pub fn run_setup() -> Result<SetupOutcome, SetupError> {
    let data_directory =
        crate::platform::data_dir().map_err(|_| SetupError::LocalStateUnavailable)?;
    let mut files = FilesystemSetupStore::new(&data_directory)
        .map_err(|_| SetupError::LocalStateUnavailable)?;
    let credentials = OsCredentialStore::zectrix_api_key()
        .map_err(|_| SetupError::CredentialPersistUnavailable)?;
    let base_url = std::env::var("CODEX_ZECTRIX_API_BASE")
        .unwrap_or_else(|_| "https://cloud.zectrix.com".into());
    let mut cloud = ZectrixSetupCloud::new(base_url);
    #[cfg(not(windows))]
    let current = std::env::current_exe().map_err(|_| SetupError::AutostartUnavailable)?;
    #[cfg(not(windows))]
    let companion = current
        .parent()
        .ok_or(SetupError::AutostartUnavailable)?
        .join(if cfg!(windows) {
            "codex-zectrix-companion.exe"
        } else {
            "codex-zectrix-companion"
        });
    #[cfg(windows)]
    let companion = crate::install::Installation::load(&data_directory)
        .map_err(|_| SetupError::LocalStateUnavailable)?
        .ok_or(SetupError::LocalStateUnavailable)?
        .companion(&data_directory);
    #[cfg(windows)]
    let mut control = crate::operator::WindowsSetupControl::new(&data_directory, &companion);
    #[cfg(not(windows))]
    let mut control = LocalSetupControl;
    let mut autostart = Autostart::new(SystemCommandRunner, companion);
    let mut terminal = ConsoleTerminal::new();
    let mut renderer = DeterministicSetupRenderer;
    run_setup_transaction(
        &mut terminal,
        &mut cloud,
        &credentials,
        &mut files,
        &mut autostart,
        &mut renderer,
        &SystemSetupClock,
        &mut control,
    )
}
