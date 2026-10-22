//! Derived, bounded display data. No credentials, task titles or raw source responses.
use crate::inkcards::PagePublisher;
use crate::status_board::{BoardTaskState, StatusBoardState};
use crate::{
    RuntimeStatus, Settings, StatusFramePublishError, StatusFramePublisher, UploadCompletion,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
#[error("本地界面缓存不可用")]
pub struct DesktopStateError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Live,
    LastKnown,
    Stale,
    Unavailable,
}
impl Freshness {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "最新数据",
            Self::LastKnown => "上次数据",
            Self::Stale => "数据已过期",
            Self::Unavailable => "暂不可用",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct DesktopFreshness {
    pub quota: Freshness,
    pub secondary_quota: Freshness,
    pub task: Freshness,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopSnapshot {
    schema_version: u32,
    pub app_version: String,
    settings_fingerprint: String,
    pub written_at: i64,
    pub quota_observed_at: Option<i64>,
    pub task_observed_at: Option<i64>,
    pub board: StatusBoardState,
}
impl DesktopSnapshot {
    pub fn new(
        settings: &Settings,
        now: i64,
        runtime: &RuntimeStatus,
        board: StatusBoardState,
    ) -> Self {
        Self {
            schema_version: 1,
            app_version: env!("CARGO_PKG_VERSION").into(),
            settings_fingerprint: configuration_fingerprint(settings),
            written_at: now,
            quota_observed_at: runtime.last_successful_quota_refresh_epoch_seconds,
            task_observed_at: runtime.last_successful_task_refresh_epoch_seconds,
            board,
        }
    }
    pub fn load(
        data: &Path,
        settings: &Settings,
        now: i64,
    ) -> Result<Option<Self>, DesktopStateError> {
        let Some(bytes) = read_bounded(&data.join("desktop-status.json"), 65536)? else {
            return Ok(None);
        };
        let value: Self = serde_json::from_slice(&bytes).map_err(|_| DesktopStateError)?;
        let valid_time = |time: i64| {
            crate::settings::epoch_seconds_is_valid(time) && time <= now.saturating_add(60)
        };
        let valid_percent = |value: f64| value.is_finite() && (0.0..=100.0).contains(&value);
        if value.schema_version != 1
            || value.app_version != env!("CARGO_PKG_VERSION")
            || value.settings_fingerprint != configuration_fingerprint(settings)
            || !valid_time(value.written_at)
            || value.board.is_sample
            || [value.task_observed_at, value.quota_observed_at]
                .into_iter()
                .flatten()
                .any(|t| !valid_time(t) || t > value.written_at.saturating_add(60))
            || value
                .board
                .quota_remaining_percent
                .is_some_and(|v| !valid_percent(v))
            || value
                .board
                .quota_secondary
                .as_ref()
                .is_some_and(|v| !valid_percent(v.remaining_percent))
        {
            return Ok(None);
        }
        Ok(Some(value))
    }
    pub fn save_atomically(&self, data: &Path) -> Result<(), DesktopStateError> {
        let bytes = serde_json::to_vec(self).map_err(|_| DesktopStateError)?;
        if bytes.len() > 65536 {
            return Err(DesktopStateError);
        }
        write_cache(&data.join("desktop-status.json"), &bytes)
    }
    pub fn assess_freshness(&self, now: i64, background_running: bool) -> DesktopFreshness {
        let assess = |observed: Option<i64>, available: bool, stale: bool, ttl: i64| {
            let Some(time) = observed else {
                return Freshness::Unavailable;
            };
            if !available || time > now.saturating_add(60) {
                Freshness::Unavailable
            } else if stale || now.saturating_sub(time) > ttl {
                Freshness::Stale
            } else if !background_running {
                Freshness::LastKnown
            } else {
                Freshness::Live
            }
        };
        DesktopFreshness {
            quota: assess(
                self.quota_observed_at,
                self.board.quota_remaining_percent.is_some(),
                self.board.quota_stale,
                1800,
            ),
            secondary_quota: assess(
                self.quota_observed_at,
                self.board.quota_secondary.is_some(),
                self.board.quota_secondary.as_ref().is_some_and(|q| q.stale),
                1800,
            ),
            task: assess(
                self.task_observed_at,
                self.board.task_state != BoardTaskState::Unavailable && !self.board.task_partial,
                self.board.task_stale,
                180,
            ),
        }
    }
}
fn configuration_fingerprint(settings: &Settings) -> String {
    // Both settings fields are bounded/validated by Settings; serialization cannot fail.
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(settings).unwrap_or_default())
    )
}

pub(crate) fn read_bounded(
    path: &Path,
    limit: usize,
) -> Result<Option<Vec<u8>>, DesktopStateError> {
    check_path(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(DesktopStateError),
    };
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(DesktopStateError);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| DesktopStateError)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DesktopStateError)?;
    if bytes.len() > limit {
        return Err(DesktopStateError);
    }
    Ok(Some(bytes))
}
fn check_path(path: &Path) -> Result<(), DesktopStateError> {
    for candidate in [Some(path), path.parent()].into_iter().flatten() {
        #[cfg(windows)]
        crate::platform::background_windows::reject_link(candidate)
            .map_err(|_| DesktopStateError)?;
        #[cfg(not(windows))]
        if fs::symlink_metadata(candidate).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(DesktopStateError);
        }
    }
    Ok(())
}
fn write_cache(path: &Path, bytes: &[u8]) -> Result<(), DesktopStateError> {
    check_path(path)?;
    crate::setup::write_scoped_atomically(path, bytes).map_err(|_| DesktopStateError)
}
fn validate_png(bytes: &[u8]) -> Result<(), DesktopStateError> {
    if bytes.len() > 131072 {
        return Err(DesktopStateError);
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().map_err(|_| DesktopStateError)?;
    if reader.info().width != 400 || reader.info().height != 300 {
        return Err(DesktopStateError);
    }
    image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .map_err(|_| DesktopStateError)?;
    Ok(())
}
pub fn load_uploaded_frame(
    data: &Path,
    page: u8,
    expected_sha256: &str,
) -> Result<Option<Vec<u8>>, DesktopStateError> {
    if !(1..=5).contains(&page)
        || expected_sha256.len() != 64
        || !expected_sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(DesktopStateError);
    }
    let Some(bytes) = read_bounded(&data.join(format!("desktop-frame-{page}.png")), 131072)? else {
        return Ok(None);
    };
    if format!("{:x}", Sha256::digest(&bytes)) != expected_sha256 {
        return Ok(None);
    }
    validate_png(&bytes)?;
    Ok(Some(bytes))
}
pub struct CachingPublisher<P> {
    inner: P,
    data: PathBuf,
    default_page: u8,
    cache_error: bool,
}
impl<P> CachingPublisher<P> {
    pub fn new(inner: P, data: PathBuf, default_page: u8) -> Self {
        Self {
            inner,
            data,
            default_page,
            cache_error: false,
        }
    }
    pub fn take_cache_error(&mut self) -> bool {
        std::mem::take(&mut self.cache_error)
    }
    fn cache(&mut self, page: u8, png: &[u8]) {
        self.cache_error |= !(1..=5).contains(&page)
            || validate_png(png).is_err()
            || write_cache(&self.data.join(format!("desktop-frame-{page}.png")), png).is_err();
    }
}
impl<P: StatusFramePublisher> StatusFramePublisher for CachingPublisher<P> {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        let completion = self.inner.publish_png(png)?;
        self.cache(self.default_page, png);
        Ok(completion)
    }
}
impl<P: PagePublisher> PagePublisher for CachingPublisher<P> {
    fn publish_page(
        &mut self,
        page: u8,
        png: &[u8],
    ) -> Result<UploadCompletion, StatusFramePublishError> {
        let completion = self.inner.publish_page(page, png)?;
        self.cache(page, png);
        Ok(completion)
    }
}
