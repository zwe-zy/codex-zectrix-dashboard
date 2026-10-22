use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::Instant;

use crate::app_server::AppServerClient;
use crate::codex_status::{CodexStatusError, CodexStatusSource};
use crate::credentials::{CredentialStore, OsCredentialStore};
use crate::freshness::{FreshValue, QUOTA_STALE_AFTER, TASK_STALE_AFTER};
use crate::platform::Shutdown;
use crate::publisher::{
    StatusFrameAttempt, StatusFrameCoordinator, StatusFramePublisherState, StatusFrameRenderError,
    StatusFrameRenderer, StatusFrameStateError, ZectrixPublisher,
};
use crate::settings::{RuntimeStatus, RuntimeStatusError, Settings};
use crate::status_board::{
    BoardLocalTime, BoardTaskState, StatusBoardClock, StatusBoardState, render_status_board,
};
use crate::{QuotaStatus, TaskState, platform};

pub const TASK_POLL_INTERVAL: Duration = Duration::from_secs(60);
pub const QUOTA_POLL_INTERVAL: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceErrorCategory {
    Unavailable,
    MethodUnavailable,
    AccountUnavailable,
    Incompatible,
}

pub trait StatusSource {
    fn read_task_state(&mut self) -> Result<TaskState, SourceErrorCategory>;
    fn read_task_observation(&mut self) -> Result<crate::TaskObservation, SourceErrorCategory> {
        self.read_task_state().map(|state| crate::TaskObservation {
            state,
            partial: state == TaskState::Unavailable,
        })
    }
    fn read_quota(&mut self) -> Result<QuotaStatus, SourceErrorCategory>;
}

impl StatusSource for CodexStatusSource {
    fn read_task_observation(&mut self) -> Result<crate::TaskObservation, SourceErrorCategory> {
        self.read_latest_task_observation()
            .map_err(map_source_error)
    }
    fn read_task_state(&mut self) -> Result<TaskState, SourceErrorCategory> {
        self.read_latest_task_state().map_err(map_source_error)
    }

    fn read_quota(&mut self) -> Result<QuotaStatus, SourceErrorCategory> {
        self.read_quota_status().map_err(map_source_error)
    }
}

struct CompanionStatusSource {
    codex: CodexStatusSource,
}

impl StatusSource for CompanionStatusSource {
    fn read_task_observation(&mut self) -> Result<crate::TaskObservation, SourceErrorCategory> {
        self.codex
            .read_latest_task_observation()
            .map_err(map_source_error)
    }
    fn read_task_state(&mut self) -> Result<TaskState, SourceErrorCategory> {
        self.codex
            .read_latest_task_state()
            .map_err(map_source_error)
    }

    fn read_quota(&mut self) -> Result<QuotaStatus, SourceErrorCategory> {
        self.codex.read_quota_status().map_err(map_source_error)
    }
}

fn map_source_error(error: CodexStatusError) -> SourceErrorCategory {
    match error {
        CodexStatusError::Unavailable => SourceErrorCategory::Unavailable,
        CodexStatusError::MethodUnavailable => SourceErrorCategory::MethodUnavailable,
        CodexStatusError::AccountUnavailable => SourceErrorCategory::AccountUnavailable,
        CodexStatusError::Incompatible => SourceErrorCategory::Incompatible,
    }
}

#[cfg(all(test, windows))]
mod task_state_source_tests {
    use super::*;

    #[test]
    fn official_active_state_wins_when_rollout_inference_cannot_describe_the_thread() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("fake-codex.ps1");
        let command = temp.path().join("fake-codex.cmd");
        fs::write(
            &script,
            r#"while (($line = [Console]::In.ReadLine()) -ne $null) {
    $request = $line | ConvertFrom-Json
    if ($request.method -eq 'initialize') {
        [Console]::Out.WriteLine('{"id":1,"result":{"userAgent":"test"}}')
    } elseif ($request.method -eq 'thread/list') {
        [Console]::Out.WriteLine('{"id":2,"result":{"data":[{"id":"still-running","parentThreadId":null,"updatedAt":2,"recencyAt":2,"status":{"type":"active","activeFlags":[]}}],"nextCursor":null}}')
    }
}"#,
        )
        .unwrap();
        fs::write(
            &command,
            format!(
                "@echo off\r\npowershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"\r\n",
                script.display()
            ),
        )
        .unwrap();
        let client = AppServerClient::new(&command);
        let mut source = CompanionStatusSource {
            codex: CodexStatusSource::new(client.clone()),
        };

        assert_eq!(source.read_task_state(), Ok(TaskState::Running));
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SchedulerPoll {
    pub state: StatusBoardState,
    pub task_polled: bool,
    pub quota_polled: bool,
    pub task_succeeded: bool,
    pub quota_succeeded: bool,
    pub source_errors: Vec<SourceErrorCategory>,
}

#[derive(Default)]
pub struct CompanionScheduler {
    task: FreshValue<crate::TaskObservation>,
    quota: FreshValue<crate::QuotaWindowStatus>,
    secondary: FreshValue<crate::QuotaWindowStatus>,
    next_task_poll: Option<Duration>,
    next_quota_poll: Option<Duration>,
}

impl CompanionScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn poll_due<S: StatusSource>(
        &mut self,
        monotonic_now: Duration,
        epoch_now: i64,
        source: &mut S,
    ) -> SchedulerPoll {
        let task_polled = self
            .next_task_poll
            .is_none_or(|deadline| monotonic_now >= deadline);
        let quota_polled = self
            .next_quota_poll
            .is_none_or(|deadline| monotonic_now >= deadline);
        let mut source_errors = Vec::new();
        let mut task_succeeded = false;
        let mut quota_succeeded = false;

        if task_polled {
            match source.read_task_observation() {
                Ok(value) => {
                    self.task.record_success(value, epoch_now);
                    task_succeeded = true;
                }
                Err(category) => source_errors.push(category),
            }
            self.next_task_poll = Some(monotonic_now.saturating_add(TASK_POLL_INTERVAL));
        }
        if quota_polled {
            match source.read_quota() {
                Ok(value) => {
                    if let Some(primary) = value.primary {
                        self.quota.record_success(primary, epoch_now);
                    } else if !value.primary_present {
                        self.quota = FreshValue::default();
                    }
                    if let Some(secondary) = value.secondary {
                        self.secondary.record_success(secondary, epoch_now);
                    } else if !value.secondary_present {
                        self.secondary = FreshValue::default();
                    }
                    quota_succeeded = true;
                }
                Err(category) => source_errors.push(category),
            }
            self.next_quota_poll = Some(monotonic_now.saturating_add(QUOTA_POLL_INTERVAL));
        }

        SchedulerPoll {
            state: self.snapshot(epoch_now),
            task_polled,
            quota_polled,
            task_succeeded,
            quota_succeeded,
            source_errors,
        }
    }

    pub fn next_deadline(&self) -> Duration {
        self.next_task_poll
            .into_iter()
            .chain(self.next_quota_poll)
            .min()
            .unwrap_or(Duration::ZERO)
    }

    fn snapshot(&self, epoch_now: i64) -> StatusBoardState {
        let quota = self.quota.value;
        StatusBoardState {
            theme: Default::default(),
            task_state: self
                .task
                .value
                .map(|observation| BoardTaskState::from(observation.state))
                .unwrap_or(BoardTaskState::Unavailable),
            task_partial: self
                .task
                .value
                .is_none_or(|observation| observation.partial),
            quota_remaining_percent: quota.map(|value| value.remaining_percent),
            quota_resets_at_epoch_seconds: quota.and_then(|value| value.resets_at_epoch_seconds),
            quota_window_duration_mins: quota.and_then(|value| value.window_duration_mins),
            quota_secondary: self.secondary.value.map(|value| {
                crate::status_board::BoardQuotaWindow {
                    remaining_percent: value.remaining_percent,
                    resets_at_epoch_seconds: value.resets_at_epoch_seconds,
                    window_duration_mins: value.window_duration_mins,
                    stale: self.secondary.is_stale(epoch_now, QUOTA_STALE_AFTER),
                }
            }),
            is_sample: false,
            quota_stale: self.quota.is_stale(epoch_now, QUOTA_STALE_AFTER),
            task_stale: self.task.is_stale(epoch_now, TASK_STALE_AFTER),
            uploaded_at_epoch_seconds: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticEvent {
    SourceUnavailable,
    SourceMethodUnavailable,
    SourceAccountUnavailable,
    SourceIncompatible,
    PublishUnavailable,
    CredentialsRejected,
    InvalidConfiguration,
    StatePersistUnavailable,
    SetupRequired,
    CredentialUnavailable,
}

impl DiagnosticEvent {
    pub const fn from_source_error(category: SourceErrorCategory) -> Self {
        match category {
            SourceErrorCategory::Unavailable => Self::SourceUnavailable,
            SourceErrorCategory::MethodUnavailable => Self::SourceMethodUnavailable,
            SourceErrorCategory::AccountUnavailable => Self::SourceAccountUnavailable,
            SourceErrorCategory::Incompatible => Self::SourceIncompatible,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "source_unavailable",
            Self::SourceMethodUnavailable => "source_method_unavailable",
            Self::SourceAccountUnavailable => "source_account_unavailable",
            Self::SourceIncompatible => "source_incompatible",
            Self::PublishUnavailable => "publish_unavailable",
            Self::CredentialsRejected => "credentials_rejected",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::StatePersistUnavailable => "state_persist_unavailable",
            Self::SetupRequired => "setup_required",
            Self::CredentialUnavailable => "credential_unavailable",
        }
    }
}

pub struct DiagnosticLog {
    path: PathBuf,
    rotated_path: PathBuf,
    max_bytes: u64,
}

impl DiagnosticLog {
    pub fn open(directory: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::with_max_bytes(directory, 64 * 1024)
    }

    pub fn with_max_bytes(directory: impl AsRef<Path>, max_bytes: u64) -> std::io::Result<Self> {
        fs::create_dir_all(directory.as_ref())?;
        Ok(Self {
            path: directory.as_ref().join("companion.log"),
            rotated_path: directory.as_ref().join("companion.log.1"),
            max_bytes: max_bytes.max(1),
        })
    }

    pub fn record(
        &mut self,
        event: DiagnosticEvent,
        _untrusted_detail: &str,
    ) -> std::io::Result<()> {
        let line = format!("{}\n", event.label());
        let current_len = fs::metadata(&self.path)
            .map(|value| value.len())
            .unwrap_or(0);
        if current_len.saturating_add(line.len() as u64) > self.max_bytes {
            match fs::remove_file(&self.rotated_path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if self.path.exists() {
                fs::rename(&self.path, &self.rotated_path)?;
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.flush()
    }
}

pub struct StatusBoardPngRenderer<C> {
    clock: C,
}

impl<C> StatusBoardPngRenderer<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }
}

impl<C: StatusBoardClock> StatusFrameRenderer for StatusBoardPngRenderer<C> {
    fn render(
        &mut self,
        state: &StatusBoardState,
        _now_epoch_seconds: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError> {
        render_status_board(state, &self.clock).map_err(|_| StatusFrameRenderError::InvalidFrame)
    }
}

pub struct SystemStatusBoardClock;

impl StatusBoardClock for SystemStatusBoardClock {
    fn now_epoch_seconds(&self) -> i64 {
        platform::now_epoch_seconds().unwrap_or(0)
    }

    fn local_time(&self, epoch_seconds: i64) -> Option<BoardLocalTime> {
        let offset = time::UtcOffset::current_local_offset().ok()?;
        let value = time::OffsetDateTime::from_unix_timestamp(epoch_seconds)
            .ok()?
            .to_offset(offset);
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

pub fn run_companion() -> std::process::ExitCode {
    std::process::ExitCode::from(run_companion_process_code())
}

pub fn run_companion_process_code() -> u8 {
    match run_companion_inner() {
        Ok(()) => 0,
        Err(CompanionExit::SetupRequired | CompanionExit::CredentialUnavailable) => 2,
        Err(CompanionExit::Unavailable) => 1,
    }
}

#[derive(Clone, Copy)]
enum CompanionExit {
    SetupRequired,
    CredentialUnavailable,
    Unavailable,
}

fn run_companion_inner() -> Result<(), CompanionExit> {
    let data_dir = platform::data_dir().map_err(|_| CompanionExit::Unavailable)?;
    #[cfg(windows)]
    let instance = {
        use crate::platform::background_windows::{BackgroundError, InstanceGuard};
        let token = std::env::var("CODEX_ZECTRIX_MAINTENANCE_TOKEN").ok();
        match InstanceGuard::acquire(&data_dir, token.as_deref()) {
            Ok(instance) => instance,
            Err(BackgroundError::Busy) => return Ok(()),
            Err(_) => return Err(CompanionExit::Unavailable),
        }
    };
    let mut diagnostics = DiagnosticLog::open(&data_dir).map_err(|_| CompanionExit::Unavailable)?;
    let persisted_path = data_dir.join("status-frame-state.json");
    let runtime_status_path = data_dir.join("runtime-status.json");
    let mut runtime_status = match RuntimeStatus::load(&runtime_status_path) {
        Ok(Some(status)) => status,
        Ok(None) => RuntimeStatus::default(),
        Err(RuntimeStatusError::Decode | RuntimeStatusError::InvalidTimestamp) => {
            let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
            return Err(CompanionExit::Unavailable);
        }
        Err(RuntimeStatusError::Read | RuntimeStatusError::Encode | RuntimeStatusError::Write) => {
            let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
            return Err(CompanionExit::Unavailable);
        }
    };
    let persisted = match StatusFramePublisherState::load(&persisted_path) {
        Ok(Some(state)) => state,
        Ok(None) => StatusFramePublisherState::default(),
        Err(StatusFrameStateError::Decode) => {
            let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
            return Err(CompanionExit::Unavailable);
        }
        Err(
            StatusFrameStateError::Read
            | StatusFrameStateError::Encode
            | StatusFrameStateError::Write,
        ) => {
            let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
            return Err(CompanionExit::Unavailable);
        }
    };
    let settings = match Settings::load(data_dir.join("settings.json")) {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            let _ = diagnostics.record(DiagnosticEvent::SetupRequired, "");
            return Err(CompanionExit::SetupRequired);
        }
        Err(_) => {
            let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
            return Err(CompanionExit::Unavailable);
        }
    };
    let inkcard_path = data_dir.join("inkcard-state.json");
    let inkcard_settings = crate::inkcards::InkcardSettings::load(data_dir.join("inkcards.json"))
        .map_err(|_| {
        let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
        CompanionExit::Unavailable
    })?;
    let mut inkcards = if inkcard_settings.uses_coordinator() {
        let saved = crate::inkcards::InkcardState::load(&inkcard_path).map_err(|_| {
            let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
            CompanionExit::Unavailable
        })?;
        Some(
            crate::inkcards::InkcardCoordinator::with_pages(
                &settings,
                &persisted,
                saved,
                &inkcard_settings
                    .resolved_pages(&settings, crate::status_board::BoardTheme::default()),
            )
            .map_err(|_| CompanionExit::Unavailable)?,
        )
    } else {
        None
    };
    let credential_store = OsCredentialStore::zectrix_api_key().map_err(|_| {
        let _ = diagnostics.record(DiagnosticEvent::CredentialUnavailable, "");
        CompanionExit::CredentialUnavailable
    })?;
    let api_key = credential_store
        .get()
        .map_err(|_| {
            let _ = diagnostics.record(DiagnosticEvent::CredentialUnavailable, "");
            CompanionExit::CredentialUnavailable
        })?
        .ok_or_else(|| {
            let _ = diagnostics.record(DiagnosticEvent::CredentialUnavailable, "");
            CompanionExit::CredentialUnavailable
        })?;
    let base_url = std::env::var("CODEX_ZECTRIX_API_BASE")
        .unwrap_or_else(|_| "https://cloud.zectrix.com".into());
    let publisher = ZectrixPublisher::new(
        api_key.as_str(),
        base_url,
        settings.device_id.clone(),
        settings.page_id,
    )
    .map_err(|_| {
        let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
        CompanionExit::Unavailable
    })?;
    drop(api_key);
    let mut publisher =
        crate::desktop_state::CachingPublisher::new(publisher, data_dir.clone(), settings.page_id);

    let client = AppServerClient::default();
    let mut source = CompanionStatusSource {
        codex: CodexStatusSource::new(client),
    };
    let shutdown = Shutdown::install().map_err(|_| CompanionExit::Unavailable)?;
    let mut coordinator = StatusFrameCoordinator::new(persisted);
    let mut scheduler = CompanionScheduler::new();
    let mut renderer = StatusBoardPngRenderer::new(SystemStatusBoardClock);
    let started = Instant::now();
    let max_cycles = std::env::var("CODEX_ZECTRIX_MAX_CYCLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let mut cycles = 0_usize;
    let mut theme = crate::status_board::BoardTheme::default();
    let mut appearance_unavailable = false;

    #[cfg(windows)]
    {
        runtime_status
            .save_atomically(&runtime_status_path)
            .map_err(|_| CompanionExit::Unavailable)?;
        instance
            .mark_ready()
            .map_err(|_| CompanionExit::Unavailable)?;
    }

    loop {
        #[cfg(windows)]
        {
            if instance.stop_requested() {
                return Ok(());
            }
            if !instance.may_publish() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        }
        let monotonic_now = started.elapsed();
        let epoch_now = platform::now_epoch_seconds().map_err(|_| CompanionExit::Unavailable)?;
        let poll = scheduler.poll_due(monotonic_now, epoch_now, &mut source);
        if poll.task_succeeded {
            runtime_status.last_successful_task_refresh_epoch_seconds = Some(epoch_now);
        }
        if poll.quota_succeeded {
            runtime_status.last_successful_quota_refresh_epoch_seconds = Some(epoch_now);
        }
        for category in poll.source_errors {
            let _ = diagnostics.record(DiagnosticEvent::from_source_error(category), "");
        }
        // Preference-only changes participate in the normal content fingerprint/throttle.
        // A damaged preference must not interrupt quota/task observation or erase a valid theme.
        match crate::AppearanceSettings::load(data_dir.join("appearance.json")) {
            Ok(appearance) => {
                theme = appearance.theme;
                appearance_unavailable = false;
            }
            Err(_) => {
                if !appearance_unavailable {
                    let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
                }
                appearance_unavailable = true;
            }
        }
        let mut state = poll.state;
        state.theme = theme;
        let snapshot = crate::desktop_state::DesktopSnapshot::new(
            &settings,
            epoch_now,
            &runtime_status,
            state.clone(),
        );
        if snapshot.save_atomically(&data_dir).is_err() {
            let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
        }
        #[cfg(windows)]
        if !instance.may_publish() {
            continue;
        }
        let attempts = if let Some(cards) = inkcards.as_mut() {
            cards.observe(state);
            let attempts = cards
                .publish_due(
                    || platform::now_epoch_seconds().ok(),
                    &mut renderer,
                    &mut publisher,
                    || {
                        #[cfg(windows)]
                        {
                            !instance.stop_requested() && instance.may_publish()
                        }
                        #[cfg(not(windows))]
                        {
                            true
                        }
                    },
                    |snapshot| snapshot.save_atomically(&inkcard_path).is_ok(),
                )
                .map_err(|error| {
                    if error == crate::inkcards::InkcardPublishError::StatePersistUnavailable {
                        let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
                    }
                    CompanionExit::Unavailable
                })?;
            runtime_status.last_successful_upload_epoch_seconds = cards.last_content_upload();
            attempts
                .into_iter()
                .map(|(_, attempt)| attempt)
                .collect::<Vec<_>>()
        } else {
            coordinator.observe(state);
            let attempt = coordinator.try_publish(epoch_now, &mut renderer, &mut publisher);
            if coordinator
                .state()
                .save_atomically(&persisted_path)
                .is_err()
            {
                let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
            }
            runtime_status.last_successful_upload_epoch_seconds =
                coordinator.state().last_successful_upload_epoch_seconds;
            vec![attempt]
        };
        if publisher.take_cache_error() {
            let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
        }
        for attempt in attempts {
            match attempt {
                StatusFrameAttempt::TransientFailure { .. } => {
                    let _ = diagnostics.record(DiagnosticEvent::PublishUnavailable, "");
                }
                StatusFrameAttempt::CredentialsRejected => {
                    let _ = diagnostics.record(DiagnosticEvent::CredentialsRejected, "");
                }
                StatusFrameAttempt::InvalidConfiguration => {
                    let _ = diagnostics.record(DiagnosticEvent::InvalidConfiguration, "");
                }
                StatusFrameAttempt::Idle
                | StatusFrameAttempt::Deferred { .. }
                | StatusFrameAttempt::Unchanged
                | StatusFrameAttempt::Published
                | StatusFrameAttempt::PublishedUpdated => {}
            }
        }
        if runtime_status
            .save_atomically(&runtime_status_path)
            .is_err()
        {
            let _ = diagnostics.record(DiagnosticEvent::StatePersistUnavailable, "");
        }

        cycles = cycles.saturating_add(1);
        if max_cycles.is_some_and(|maximum| cycles >= maximum) {
            return Ok(());
        }
        let now_elapsed = started.elapsed();
        let epoch_now = platform::now_epoch_seconds().map_err(|_| CompanionExit::Unavailable)?;
        let mut wait = scheduler.next_deadline().saturating_sub(now_elapsed);
        let retry = if let Some(cards) = inkcards.as_ref() {
            cards.next_deadline(epoch_now)
        } else {
            coordinator.state().next_allowed_attempt_epoch_seconds
        };
        if let Some(retry_epoch) = retry {
            let retry_wait_seconds = retry_epoch.saturating_sub(epoch_now).max(0) as u64;
            wait = wait.min(Duration::from_secs(retry_wait_seconds));
        }
        let deadline = Instant::now() + wait;
        loop {
            if shutdown.wait_until(deadline.min(Instant::now() + Duration::from_millis(200))) {
                return Ok(());
            }
            #[cfg(windows)]
            if instance.stop_requested() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
        }
    }
}
