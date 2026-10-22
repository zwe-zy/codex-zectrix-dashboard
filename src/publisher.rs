use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::status_board::StatusBoardState;
use crate::{
    DashboardConfig, DashboardError, DashboardOutput, NormalizedDashboardState,
    ObservedDashboardState, RadarDisplayState, ResetKind, normalize_dashboard,
    render_normalized_dashboard_with_sync,
};

pub const MIN_PUSH_INTERVAL_SECONDS: i64 = 60;
const UPDATED_FOLLOW_UP_PHASE_BIT: u32 = 1 << 31;
const MAX_CONSECUTIVE_FAILURE_COUNT: u32 = UPDATED_FOLLOW_UP_PHASE_BIT - 2;
const AUTH_REJECTED_UPDATED_FOLLOW_UP_SENTINEL: u32 = UPDATED_FOLLOW_UP_PHASE_BIT - 1;
const AUTH_REJECTED_SOURCE_CHANGE_SENTINEL: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentSource {
    Sample,
    Live,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublisherState {
    pub last_successful_sync_epoch_seconds: Option<i64>,
    pub next_allowed_push_epoch_seconds: Option<i64>,
    pub last_frame_hash: Option<String>,
    pub last_visible_state_hash: Option<String>,
    pub last_reset_at_epoch_seconds: Vec<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishAttempt {
    Idle,
    Deferred { until_epoch_seconds: i64 },
    Unchanged,
    Published,
    Failed,
    ReservationFailed,
}

pub trait FramePublisher {
    type Error;

    fn publish(&mut self, dashboard: &DashboardOutput) -> Result<(), Self::Error>;
}

pub struct ZectrixPublisher {
    client: Client,
    base_url: String,
    device_id: String,
    page_id: u8,
}

#[derive(Debug, Error)]
pub enum ZectrixPublishError {
    #[error("ZECTRIX 发布配置无效")]
    InvalidConfiguration,
    #[error("无法编码看板图像")]
    Image,
    #[error("ZECTRIX 发布暂时不可用")]
    Unavailable,
}

impl ZectrixPublisher {
    pub fn new(
        api_key: &str,
        base_url: impl Into<String>,
        device_id: impl Into<String>,
        page_id: u8,
    ) -> Result<Self, ZectrixPublishError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let device_id = device_id.into();
        let parsed_base = reqwest::Url::parse(&base_url)
            .map_err(|_| ZectrixPublishError::InvalidConfiguration)?;
        if !matches!(parsed_base.scheme(), "http" | "https")
            || device_id.trim().is_empty()
            || !(1..=5).contains(&page_id)
        {
            return Err(ZectrixPublishError::InvalidConfiguration);
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-API-Key",
            HeaderValue::from_str(api_key)
                .map_err(|_| ZectrixPublishError::InvalidConfiguration)?,
        );
        let client = Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|_| ZectrixPublishError::InvalidConfiguration)?;
        Ok(Self {
            client,
            base_url,
            device_id,
            page_id,
        })
    }
}

impl FramePublisher for ZectrixPublisher {
    type Error = ZectrixPublishError;

    fn publish(&mut self, dashboard: &DashboardOutput) -> Result<(), Self::Error> {
        let image = dashboard
            .frame
            .png_bytes()
            .map_err(|_| ZectrixPublishError::Image)?;
        if image.len() > 2 * 1024 * 1024 {
            return Err(ZectrixPublishError::Image);
        }
        let image = Part::bytes(image)
            .file_name("codex-dashboard.png")
            .mime_str("image/png")
            .map_err(|_| ZectrixPublishError::Image)?;
        let form = Form::new()
            .text("dither", "false")
            .text("pageId", self.page_id.to_string())
            .part("images", image);
        let response = self
            .client
            .post(format!(
                "{}/open/v1/devices/{}/display/image",
                self.base_url, self.device_id
            ))
            .multipart(form)
            .send()
            .map_err(|_| ZectrixPublishError::Unavailable)?;
        if !response.status().is_success() {
            return Err(ZectrixPublishError::Unavailable);
        }
        let response: ApiCodeResponse = response
            .json()
            .map_err(|_| ZectrixPublishError::Unavailable)?;
        if response.code != 0 {
            return Err(ZectrixPublishError::Unavailable);
        }
        Ok(())
    }
}

impl ZectrixPublisher {
    fn publish_png_bytes(&mut self, png: &[u8]) -> Result<(), StatusFramePublishError> {
        if png.len() > 2 * 1024 * 1024 || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(StatusFramePublishError::InvalidFrameConfiguration);
        }
        let image = Part::bytes(png.to_vec())
            .file_name("codex-status.png")
            .mime_str("image/png")
            .map_err(|_| StatusFramePublishError::InvalidFrameConfiguration)?;
        let form = Form::new()
            .text("dither", "false")
            .text("pageId", self.page_id.to_string())
            .part("images", image);
        let response = self
            .client
            .post(format!(
                "{}/open/v1/devices/{}/display/image",
                self.base_url, self.device_id
            ))
            .multipart(form)
            .send()
            .map_err(|_| StatusFramePublishError::TransientUnavailable)?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(StatusFramePublishError::CredentialsRejected);
        }
        if matches!(response.status().as_u16(), 400 | 404 | 422) {
            return Err(StatusFramePublishError::InvalidFrameConfiguration);
        }
        if !response.status().is_success() {
            return Err(StatusFramePublishError::TransientUnavailable);
        }
        let response: ApiCodeResponse = response
            .json()
            .map_err(|_| StatusFramePublishError::TransientUnavailable)?;
        if response.code != 0 {
            return Err(StatusFramePublishError::TransientUnavailable);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StatusFramePublisherState {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub content_source: Option<ContentSource>,
    #[serde(default)]
    pub last_successful_content_fingerprint: Option<String>,
    #[serde(default)]
    pub last_successful_http_upload_epoch_seconds: Option<i64>,
    pub failure_count: u32,
    pub last_successful_frame_sha256: Option<String>,
    pub last_successful_upload_epoch_seconds: Option<i64>,
    pub next_allowed_attempt_epoch_seconds: Option<i64>,
}
impl Default for StatusFramePublisherState {
    fn default() -> Self {
        Self {
            schema_version: 2,
            content_source: None,
            last_successful_content_fingerprint: None,
            last_successful_http_upload_epoch_seconds: None,
            failure_count: 0,
            last_successful_frame_sha256: None,
            last_successful_upload_epoch_seconds: None,
            next_allowed_attempt_epoch_seconds: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum StatusFrameStateError {
    #[error("unable to read status frame state")]
    Read,
    #[error("unable to decode status frame state")]
    Decode,
    #[error("unable to encode status frame state")]
    Encode,
    #[error("unable to write status frame state")]
    Write,
}

impl StatusFramePublisherState {
    pub fn credentials_rejected(&self) -> bool {
        decode_durable_phase(self.failure_count).credentials_rejected
    }
    pub fn consecutive_failure_count(&self) -> u32 {
        decode_durable_phase(self.failure_count).failure_count
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Option<Self>, StatusFrameStateError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(StatusFrameStateError::Read),
        };
        let state: Self =
            serde_json::from_slice(&bytes).map_err(|_| StatusFrameStateError::Decode)?;
        if !matches!(state.schema_version, 0 | 2) {
            return Err(StatusFrameStateError::Decode);
        }
        Ok(Some(state))
    }

    pub fn save_atomically(&self, path: impl AsRef<Path>) -> Result<(), StatusFrameStateError> {
        let path = path.as_ref();
        let bytes = serde_json::to_vec(self).map_err(|_| StatusFrameStateError::Encode)?;
        let temporary = write_state_temporary(path, &bytes)?;
        if replace_state_file_atomically(&temporary, path).is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(StatusFrameStateError::Write);
        }
        Ok(())
    }
}

static STATE_TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_state_temporary(path: &Path, bytes: &[u8]) -> Result<PathBuf, StatusFrameStateError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    for _ in 0..32 {
        let sequence = STATE_TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(
            ".status-frame-state-{}-{sequence}.tmp",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(StatusFrameStateError::Write),
        };
        if file.write_all(bytes).is_err() || file.sync_all().is_err() {
            let _ = fs::remove_file(&temporary);
            return Err(StatusFrameStateError::Write);
        }
        return Ok(temporary);
    }
    Err(StatusFrameStateError::Write)
}

#[cfg(windows)]
fn replace_state_file_atomically(temporary: &Path, path: &Path) -> std::io::Result<()> {
    match fs::metadata(path) {
        Ok(_) => replace_existing_state_file(temporary, path),
        Err(error) if error.kind() == ErrorKind::NotFound => match fs::rename(temporary, path) {
            Ok(()) => Ok(()),
            Err(rename_error) => match fs::metadata(path) {
                Ok(_) => replace_existing_state_file(temporary, path),
                Err(_) => Err(rename_error),
            },
        },
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn replace_existing_state_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
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
fn replace_state_file_atomically(temporary: &Path, path: &Path) -> std::io::Result<()> {
    fs::rename(temporary, path)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UploadCompletion {
    pub epoch_seconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum StatusFramePublishError {
    #[error("ZECTRIX credentials rejected")]
    CredentialsRejected,
    #[error("ZECTRIX service temporarily unavailable")]
    TransientUnavailable,
    #[error("status frame configuration invalid")]
    InvalidFrameConfiguration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum StatusFrameRenderError {
    #[error("status frame invalid")]
    InvalidFrame,
}

pub trait StatusFrameRenderer {
    fn render(
        &mut self,
        state: &StatusBoardState,
        now_epoch_seconds: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError>;
}

pub trait StatusFramePublisher {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError>;
}

impl StatusFramePublisher for ZectrixPublisher {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        self.publish_png_bytes(png)?;
        let epoch_seconds = crate::platform::now_epoch_seconds()
            .map_err(|_| StatusFramePublishError::InvalidFrameConfiguration)?;
        Ok(UploadCompletion { epoch_seconds })
    }
}

impl crate::inkcards::PagePublisher for ZectrixPublisher {
    fn publish_page(
        &mut self,
        page: u8,
        png: &[u8],
    ) -> Result<UploadCompletion, StatusFramePublishError> {
        if !(1..=5).contains(&page) {
            return Err(StatusFramePublishError::InvalidFrameConfiguration);
        }
        self.page_id = page;
        self.publish_png(png)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusFrameAttempt {
    Idle,
    Deferred { until_epoch_seconds: i64 },
    Unchanged,
    Published,
    PublishedUpdated,
    TransientFailure { retry_at_epoch_seconds: i64 },
    CredentialsRejected,
    InvalidConfiguration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingKind {
    SourceChange,
    UpdatedFollowUp,
    CoalescedSourceChange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurablePendingPhase {
    SourceChange,
    UpdatedFollowUp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DecodedDurablePhase {
    phase: DurablePendingPhase,
    failure_count: u32,
    credentials_rejected: bool,
}

impl PendingKind {
    const fn durable_phase(self) -> DurablePendingPhase {
        match self {
            Self::SourceChange | Self::CoalescedSourceChange => DurablePendingPhase::SourceChange,
            Self::UpdatedFollowUp => DurablePendingPhase::UpdatedFollowUp,
        }
    }
}

impl DurablePendingPhase {
    const fn pending_kind(self) -> PendingKind {
        match self {
            Self::SourceChange => PendingKind::SourceChange,
            Self::UpdatedFollowUp => PendingKind::UpdatedFollowUp,
        }
    }
}

pub struct StatusFrameCoordinator {
    state: StatusFramePublisherState,
    latest_source_state: Option<StatusBoardState>,
    pending: Option<PendingKind>,
    reconstruct_pending: Option<PendingKind>,
    credentials_rejected: bool,
    invalid_configuration: bool,
    previous_attempt_clock: Option<i64>,
    adjusted_http_deadline: Option<i64>,
}

impl Default for StatusFrameCoordinator {
    fn default() -> Self {
        Self::new(StatusFramePublisherState::default())
    }
}

impl StatusFrameCoordinator {
    pub fn new(mut state: StatusFramePublisherState) -> Self {
        state.schema_version = 2;
        // Four-field durable invariant:
        // - 0..=0x7fff_fffe: source-change phase plus decoded failure count;
        // - 0x8000_0000..=0xffff_fffe: UPDATED-follow-up phase plus decoded count;
        // - 0x7fff_ffff / 0xffff_ffff: rejected credentials for UPDATED/source;
        // - next is the real follow-up/retry deadline; no source data is persisted.
        // Every new encoded u32 value has one meaning, so auth, phase, and saturation cannot
        // collide; the one legacy four-field combination below is migrated before decoding.
        // ea7056a wrote zero failures plus a deadline for an outstanding UPDATED follow-up.
        // A source-change transient retry always had at least one failure, so this legacy shape is
        // unambiguous. Migrate only in memory; the caller's normal atomic save owns disk mutation.
        let legacy_updated_follow_up =
            state.failure_count == 0 && state.next_allowed_attempt_epoch_seconds.is_some();
        let mut decoded = decode_durable_phase(state.failure_count);
        if legacy_updated_follow_up {
            decoded.phase = DurablePendingPhase::UpdatedFollowUp;
            state.failure_count =
                encode_failure_count(DurablePendingPhase::UpdatedFollowUp, decoded.failure_count);
        }
        let reconstruct_pending = if decoded.credentials_rejected
            || state.next_allowed_attempt_epoch_seconds.is_some()
            || decoded.phase == DurablePendingPhase::UpdatedFollowUp
        {
            Some(decoded.phase.pending_kind())
        } else {
            None
        };
        Self {
            state,
            latest_source_state: None,
            pending: None,
            reconstruct_pending,
            credentials_rejected: decoded.credentials_rejected,
            invalid_configuration: false,
            previous_attempt_clock: None,
            adjusted_http_deadline: None,
        }
    }

    pub fn observe(&mut self, mut state: StatusBoardState) -> bool {
        state.uploaded_at_epoch_seconds = None;
        if self.latest_source_state.is_none() {
            let same = self.state.last_successful_content_fingerprint.as_deref()
                == Some(content_fingerprint(&state).as_str())
                && self.state.content_source == Some(content_source(&state));
            let pending = self.reconstruct_pending.take();
            self.latest_source_state = Some(state);
            self.pending = if same {
                pending
            } else {
                Some(PendingKind::SourceChange)
            };
            return self.pending.is_some();
        }
        let changed = self.latest_source_state.as_ref() != Some(&state);
        self.latest_source_state = Some(state);
        if changed {
            self.pending = Some(match self.pending {
                Some(PendingKind::UpdatedFollowUp | PendingKind::CoalescedSourceChange) => {
                    PendingKind::CoalescedSourceChange
                }
                _ => PendingKind::SourceChange,
            });
        }
        changed
    }

    pub fn try_publish<R: StatusFrameRenderer, P: StatusFramePublisher>(
        &mut self,
        now_epoch_seconds: i64,
        renderer: &mut R,
        publisher: &mut P,
    ) -> StatusFrameAttempt {
        if self.credentials_rejected {
            return StatusFrameAttempt::CredentialsRejected;
        }
        if self.invalid_configuration {
            return StatusFrameAttempt::InvalidConfiguration;
        }
        let Some(pending_kind) = self.pending else {
            return StatusFrameAttempt::Idle;
        };
        let reference = self
            .previous_attempt_clock
            .or(self.state.last_successful_http_upload_epoch_seconds);
        if reference.is_some_and(|previous| now_epoch_seconds < previous) {
            let next = now_epoch_seconds.saturating_add(MIN_PUSH_INTERVAL_SECONDS);
            self.adjusted_http_deadline = Some(next);
            if self.state.next_allowed_attempt_epoch_seconds.is_some() {
                let failures = self.state.consecutive_failure_count();
                let delay = if failures > 0 {
                    retry_delay(failures)
                } else {
                    MIN_PUSH_INTERVAL_SECONDS
                };
                self.state.next_allowed_attempt_epoch_seconds =
                    Some(now_epoch_seconds.saturating_add(delay));
            }
        }
        self.previous_attempt_clock = Some(now_epoch_seconds);
        let http_deadline = self.adjusted_http_deadline.or(self
            .state
            .last_successful_http_upload_epoch_seconds
            .map(|last| last.saturating_add(MIN_PUSH_INTERVAL_SECONDS)));
        if let Some(until_epoch_seconds) = self
            .state
            .next_allowed_attempt_epoch_seconds
            .into_iter()
            .chain(http_deadline)
            .max()
            && now_epoch_seconds < until_epoch_seconds
        {
            return StatusFrameAttempt::Deferred {
                until_epoch_seconds,
            };
        }
        let Some(mut candidate) = self.latest_source_state.clone() else {
            self.pending = None;
            return StatusFrameAttempt::Idle;
        };
        candidate.uploaded_at_epoch_seconds = self.state.last_successful_upload_epoch_seconds;
        let png = match renderer.render(&candidate, now_epoch_seconds) {
            Ok(png) => png,
            Err(_) => {
                self.invalid_configuration = true;
                return StatusFrameAttempt::InvalidConfiguration;
            }
        };
        let hash = format!("{:x}", Sha256::digest(&png));
        if self.state.last_successful_frame_sha256.as_deref() == Some(&hash) {
            self.state.last_successful_content_fingerprint = Some(content_fingerprint(&candidate));
            self.state.content_source = Some(content_source(&candidate));
            self.pending = None;
            self.state.failure_count = encode_failure_count(DurablePendingPhase::SourceChange, 0);
            self.state.next_allowed_attempt_epoch_seconds = None;
            return StatusFrameAttempt::Unchanged;
        }

        match publisher.publish_png(&png) {
            Ok(completion) => {
                self.state.last_successful_frame_sha256 = Some(hash);
                self.state.last_successful_http_upload_epoch_seconds =
                    Some(completion.epoch_seconds);
                self.adjusted_http_deadline = None;
                self.state.last_successful_content_fingerprint =
                    Some(content_fingerprint(&candidate));
                self.state.content_source = Some(content_source(&candidate));
                match pending_kind {
                    PendingKind::SourceChange | PendingKind::CoalescedSourceChange => {
                        if candidate.is_sample {
                            self.state.failure_count =
                                encode_failure_count(DurablePendingPhase::SourceChange, 0);
                            self.state.next_allowed_attempt_epoch_seconds = None;
                            self.pending = None;
                            return StatusFrameAttempt::Published;
                        }
                        self.state.last_successful_upload_epoch_seconds =
                            Some(completion.epoch_seconds);
                        self.state.failure_count =
                            encode_failure_count(DurablePendingPhase::UpdatedFollowUp, 0);
                        self.state.next_allowed_attempt_epoch_seconds = Some(
                            completion
                                .epoch_seconds
                                .saturating_add(MIN_PUSH_INTERVAL_SECONDS),
                        );
                        self.pending = Some(PendingKind::UpdatedFollowUp);
                        StatusFrameAttempt::Published
                    }
                    PendingKind::UpdatedFollowUp => {
                        // The candidate already displayed the committed completion time. Advancing
                        // it again here would create an endless timestamp-only upload chain.
                        self.state.failure_count =
                            encode_failure_count(DurablePendingPhase::SourceChange, 0);
                        self.state.next_allowed_attempt_epoch_seconds = None;
                        self.pending = None;
                        StatusFrameAttempt::PublishedUpdated
                    }
                }
            }
            Err(StatusFramePublishError::TransientUnavailable) => {
                let phase = pending_kind.durable_phase();
                let failure_count = decode_durable_phase(self.state.failure_count)
                    .failure_count
                    .saturating_add(1)
                    .min(MAX_CONSECUTIVE_FAILURE_COUNT);
                self.state.failure_count = encode_failure_count(phase, failure_count);
                let delay = retry_delay(failure_count);
                let retry_at_epoch_seconds = now_epoch_seconds.saturating_add(delay);
                self.state.next_allowed_attempt_epoch_seconds = Some(retry_at_epoch_seconds);
                StatusFrameAttempt::TransientFailure {
                    retry_at_epoch_seconds,
                }
            }
            Err(StatusFramePublishError::CredentialsRejected) => {
                self.credentials_rejected = true;
                self.state.failure_count =
                    encode_credentials_rejected(pending_kind.durable_phase());
                self.state.next_allowed_attempt_epoch_seconds = Some(i64::MAX);
                StatusFrameAttempt::CredentialsRejected
            }
            Err(StatusFramePublishError::InvalidFrameConfiguration) => {
                self.invalid_configuration = true;
                self.state.next_allowed_attempt_epoch_seconds = None;
                StatusFrameAttempt::InvalidConfiguration
            }
        }
    }

    pub fn credentials_updated(&mut self) {
        let durable_phase = self
            .pending
            .map(PendingKind::durable_phase)
            .or_else(|| self.reconstruct_pending.map(PendingKind::durable_phase))
            .unwrap_or_else(|| decode_durable_phase(self.state.failure_count).phase);
        self.credentials_rejected = false;
        self.invalid_configuration = false;
        self.state.failure_count = encode_failure_count(durable_phase, 0);
        self.state.next_allowed_attempt_epoch_seconds = None;
    }

    pub fn state(&self) -> &StatusFramePublisherState {
        &self.state
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

fn content_source(state: &StatusBoardState) -> ContentSource {
    if state.is_sample {
        ContentSource::Sample
    } else {
        ContentSource::Live
    }
}
fn content_fingerprint(state: &StatusBoardState) -> String {
    let mut semantic = state.clone();
    semantic.uploaded_at_epoch_seconds = None;
    if let Some(value) = semantic.quota_remaining_percent.as_mut() {
        *value = (*value * 10.0).round() / 10.0;
    }
    if let Some(window) = semantic.quota_secondary.as_mut() {
        window.remaining_percent = (window.remaining_percent * 10.0).round() / 10.0;
    }
    // StatusBoardState contains only public status categories/window values. Never hash credentials or task text.
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&semantic).unwrap_or_default())
    )
}

fn decode_durable_phase(encoded: u32) -> DecodedDurablePhase {
    if encoded == AUTH_REJECTED_SOURCE_CHANGE_SENTINEL {
        return DecodedDurablePhase {
            phase: DurablePendingPhase::SourceChange,
            failure_count: 0,
            credentials_rejected: true,
        };
    }
    if encoded == AUTH_REJECTED_UPDATED_FOLLOW_UP_SENTINEL {
        return DecodedDurablePhase {
            phase: DurablePendingPhase::UpdatedFollowUp,
            failure_count: 0,
            credentials_rejected: true,
        };
    }
    let phase = if encoded & UPDATED_FOLLOW_UP_PHASE_BIT == 0 {
        DurablePendingPhase::SourceChange
    } else {
        DurablePendingPhase::UpdatedFollowUp
    };
    DecodedDurablePhase {
        phase,
        failure_count: (encoded & !UPDATED_FOLLOW_UP_PHASE_BIT).min(MAX_CONSECUTIVE_FAILURE_COUNT),
        credentials_rejected: false,
    }
}

fn encode_failure_count(phase: DurablePendingPhase, failure_count: u32) -> u32 {
    let failure_count = failure_count.min(MAX_CONSECUTIVE_FAILURE_COUNT);
    match phase {
        DurablePendingPhase::SourceChange => failure_count,
        DurablePendingPhase::UpdatedFollowUp => UPDATED_FOLLOW_UP_PHASE_BIT | failure_count,
    }
}

const fn encode_credentials_rejected(phase: DurablePendingPhase) -> u32 {
    match phase {
        DurablePendingPhase::SourceChange => AUTH_REJECTED_SOURCE_CHANGE_SENTINEL,
        DurablePendingPhase::UpdatedFollowUp => AUTH_REJECTED_UPDATED_FOLLOW_UP_SENTINEL,
    }
}

fn retry_delay(failure_count: u32) -> i64 {
    match failure_count {
        0 | 1 => 60,
        2 => 120,
        3 => 300,
        _ => 900,
    }
}

#[derive(Deserialize)]
struct ApiCodeResponse {
    code: i64,
}

struct PendingDashboard {
    normalized: NormalizedDashboardState,
}

pub struct PublishCoordinator {
    config: DashboardConfig,
    state: PublisherState,
    pending: Option<PendingDashboard>,
    latest_observed: Option<NormalizedDashboardState>,
    latest_visible_hash: Option<String>,
}

impl PublishCoordinator {
    pub fn new(config: DashboardConfig, state: PublisherState) -> Self {
        Self {
            config,
            state,
            pending: None,
            latest_observed: None,
            latest_visible_hash: None,
        }
    }

    pub fn observe(&mut self, observed: ObservedDashboardState, now_epoch_seconds: i64) -> bool {
        let normalized = normalize_dashboard(observed, now_epoch_seconds, &self.config);
        let current_visible_hash = visible_state_hash(&normalized, now_epoch_seconds);
        let changed = if let Some(previous) = self.latest_observed.as_ref() {
            self.latest_visible_hash.as_deref() != Some(&current_visible_hash)
                || reset_labels_changed(previous, &normalized, now_epoch_seconds)
        } else {
            self.state.last_visible_state_hash.as_deref() != Some(&current_visible_hash)
                || persisted_reset_labels_changed(
                    &self.state.last_reset_at_epoch_seconds,
                    &normalized,
                    now_epoch_seconds,
                )
        };
        self.latest_observed = Some(normalized.clone());
        self.latest_visible_hash = Some(current_visible_hash.clone());
        if changed {
            self.pending = Some(PendingDashboard { normalized });
        } else if let Some(pending) = self.pending.as_mut() {
            pending.normalized = normalized;
        }
        changed
    }

    pub fn try_publish<P: FramePublisher>(
        &mut self,
        now_epoch_seconds: i64,
        publisher: &mut P,
    ) -> Result<PublishAttempt, DashboardError> {
        self.try_publish_with_reservation(now_epoch_seconds, publisher, |_| true)
    }

    pub fn try_publish_with_reservation<P, R>(
        &mut self,
        now_epoch_seconds: i64,
        publisher: &mut P,
        mut reserve: R,
    ) -> Result<PublishAttempt, DashboardError>
    where
        P: FramePublisher,
        R: FnMut(&PublisherState) -> bool,
    {
        let Some(pending) = self.pending.as_ref() else {
            return Ok(PublishAttempt::Idle);
        };
        let successful_interval = self
            .state
            .last_successful_sync_epoch_seconds
            .map(|last_success| last_success.saturating_add(MIN_PUSH_INTERVAL_SECONDS));
        let until_epoch_seconds = successful_interval
            .into_iter()
            .chain(self.state.next_allowed_push_epoch_seconds)
            .max();
        if let Some(until_epoch_seconds) = until_epoch_seconds
            && now_epoch_seconds < until_epoch_seconds
        {
            return Ok(PublishAttempt::Deferred {
                until_epoch_seconds,
            });
        }

        let dashboard = render_normalized_dashboard_with_sync(
            pending.normalized.clone(),
            now_epoch_seconds,
            DashboardConfig {
                previous_frame_hash: None,
                ..self.config.clone()
            },
            Some(now_epoch_seconds),
        )?;
        let published_visible_hash = visible_state_hash(&pending.normalized, now_epoch_seconds);
        if self.state.last_frame_hash.as_deref() == Some(&dashboard.frame.sha256) {
            self.state.last_visible_state_hash = Some(published_visible_hash.clone());
            self.state.last_reset_at_epoch_seconds = reset_timestamps(&pending.normalized);
            self.latest_visible_hash = Some(published_visible_hash);
            self.pending = None;
            return Ok(PublishAttempt::Unchanged);
        }
        let previous_reservation = self.state.next_allowed_push_epoch_seconds;
        self.state.next_allowed_push_epoch_seconds =
            Some(now_epoch_seconds.saturating_add(MIN_PUSH_INTERVAL_SECONDS));
        if !reserve(&self.state) {
            self.state.next_allowed_push_epoch_seconds = previous_reservation;
            return Ok(PublishAttempt::ReservationFailed);
        }
        if publisher.publish(&dashboard).is_err() {
            return Ok(PublishAttempt::Failed);
        }

        self.state.last_successful_sync_epoch_seconds = Some(now_epoch_seconds);
        self.state.next_allowed_push_epoch_seconds = None;
        self.state.last_frame_hash = Some(dashboard.frame.sha256);
        self.state.last_visible_state_hash = Some(published_visible_hash.clone());
        self.state.last_reset_at_epoch_seconds = reset_timestamps(&pending.normalized);
        self.latest_visible_hash = Some(published_visible_hash);
        self.pending = None;
        Ok(PublishAttempt::Published)
    }

    pub fn state(&self) -> &PublisherState {
        &self.state
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}

fn visible_state_hash(state: &NormalizedDashboardState, now_epoch_seconds: i64) -> String {
    let mut digest = Sha256::new();
    digest.update(b"codex-zectrix-visible-state-v6\0");
    digest.update(crate::current_date_label(now_epoch_seconds).as_bytes());
    for window in &state.quota.windows {
        digest.update(window.name.as_bytes());
        digest.update([0, window.used_percent, window.remaining_percent]);
    }
    digest.update(state.quota.reset_credits.to_be_bytes());
    match &state.radar {
        RadarDisplayState::Unavailable => digest.update([0]),
        RadarDisplayState::NoActiveWatch => digest.update([1]),
        RadarDisplayState::ActiveWatch {
            forecast_window,
            reset_chance_percent,
            stale: _,
        } => {
            digest.update([2]);
            digest.update(forecast_window.as_bytes());
            digest.update([0, reset_chance_percent.unwrap_or(u8::MAX)]);
        }
        RadarDisplayState::Confirmed { kind, stale: _ } => {
            digest.update([
                3,
                match kind {
                    ResetKind::Regular => 0,
                    ResetKind::ResetCredit => 1,
                },
            ]);
        }
    }
    digest.update([
        state.quota.stale.into(),
        state.task_activity_availability as u8,
        state.task_activity_stale.into(),
    ]);
    for task in &state.tasks {
        if let Some(title) = &task.title {
            digest.update(title.as_bytes());
        }
        digest.update([0, task.state as u8]);
    }
    digest.update(state.hidden_task_count.to_be_bytes());
    format!("{:x}", digest.finalize())
}

fn reset_labels_changed(
    previous: &NormalizedDashboardState,
    current: &NormalizedDashboardState,
    now_epoch_seconds: i64,
) -> bool {
    let previous = reset_timestamps(previous);
    let current = reset_timestamps(current);
    previous != current
        && previous
            .iter()
            .map(|timestamp| crate::quota_reset_label(*timestamp, now_epoch_seconds))
            .ne(current
                .iter()
                .map(|timestamp| crate::quota_reset_label(*timestamp, now_epoch_seconds)))
}

fn persisted_reset_labels_changed(
    previous: &[i64],
    current: &NormalizedDashboardState,
    now_epoch_seconds: i64,
) -> bool {
    let current = reset_timestamps(current);
    previous != current
        && previous
            .iter()
            .map(|timestamp| crate::quota_reset_label(*timestamp, now_epoch_seconds))
            .ne(current
                .iter()
                .map(|timestamp| crate::quota_reset_label(*timestamp, now_epoch_seconds)))
}

fn reset_timestamps(state: &NormalizedDashboardState) -> Vec<i64> {
    state
        .quota
        .windows
        .iter()
        .map(|window| window.resets_at_epoch_seconds)
        .collect()
}
