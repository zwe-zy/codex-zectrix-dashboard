//! Configurable live pages, each retaining its own deduplication, retry and timestamp state.
use crate::status_board::{BoardTheme, StatusBoardState};
use crate::{
    Settings, SettingsError, StatusFrameAttempt, StatusFrameCoordinator, StatusFramePublishError,
    StatusFramePublisher, StatusFramePublisherState, StatusFrameRenderer, UploadCompletion,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub const PAGE_THEMES: [BoardTheme; 5] = [
    BoardTheme::PopCut,
    BoardTheme::Eva,
    BoardTheme::PixelConsole,
    BoardTheme::SpaceGauge,
    BoardTheme::CyberHud,
];

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InkcardMode {
    #[default]
    Single,
    All,
}
impl std::str::FromStr for InkcardMode {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "single" => Ok(Self::Single),
            "all" => Ok(Self::All),
            _ => Err("墨卡模式必须为 all 或 single"),
        }
    }
}
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InkcardPage {
    pub page: u8,
    pub theme: BoardTheme,
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InkcardSettings {
    pub mode: InkcardMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<InkcardPage>>,
}
impl InkcardSettings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let settings: Self = read_json(path.as_ref(), 4096)?.unwrap_or_default();
        settings.validate()?;
        Ok(settings)
    }
    pub fn save_atomically(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        self.validate()?;
        crate::settings::save_json_atomically(self, path.as_ref())
    }
    pub fn from_pages(pages: Vec<InkcardPage>) -> Result<Self, SettingsError> {
        let settings = Self {
            mode: if pages.len() == 1 {
                InkcardMode::Single
            } else {
                InkcardMode::All
            },
            pages: Some(pages),
        };
        settings.validate()?;
        Ok(settings)
    }
    pub fn validate(&self) -> Result<(), SettingsError> {
        if let Some(pages) = &self.pages {
            validate_pages(pages)?;
            if (pages.len() == 1) != (self.mode == InkcardMode::Single) {
                return Err(SettingsError::Decode);
            }
        }
        Ok(())
    }
    pub fn uses_coordinator(&self) -> bool {
        self.pages.is_some() || self.mode == InkcardMode::All
    }
    pub fn resolved_pages(&self, settings: &Settings, theme: BoardTheme) -> Vec<InkcardPage> {
        self.pages.clone().unwrap_or_else(|| {
            if self.mode == InkcardMode::All {
                PAGE_THEMES
                    .into_iter()
                    .enumerate()
                    .map(|(i, theme)| InkcardPage {
                        page: i as u8 + 1,
                        theme,
                    })
                    .collect()
            } else {
                vec![InkcardPage {
                    page: settings.page_id,
                    theme,
                }]
            }
        })
    }
}
fn validate_pages(pages: &[InkcardPage]) -> Result<(), SettingsError> {
    let mut seen = [false; 5];
    if pages.is_empty() || pages.len() > 5 {
        return Err(SettingsError::Decode);
    }
    for page in pages {
        if !(1..=5).contains(&page.page)
            || std::mem::replace(&mut seen[(page.page - 1) as usize], true)
        {
            return Err(SettingsError::Decode);
        }
    }
    Ok(())
}
fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    limit: u64,
) -> Result<Option<T>, SettingsError> {
    #[cfg(windows)]
    crate::platform::background_windows::reject_link(path).map_err(|_| SettingsError::Read)?;
    match fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && meta.len() <= limit => {}
        _ => return Err(SettingsError::Read),
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| SettingsError::Read)?)
        .map(Some)
        .map_err(|_| SettingsError::Decode)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InkcardState {
    schema_version: u32,
    configuration_fingerprint: String,
    pub credentials_rejected: bool,
    pub pages: [StatusFramePublisherState; 5],
}
impl InkcardState {
    pub fn load(path: impl AsRef<Path>) -> Result<Option<Self>, SettingsError> {
        let value: Option<Self> = read_json(path.as_ref(), 32768)?;
        if value.as_ref().is_some_and(|v| {
            v.schema_version != 1
                || v.configuration_fingerprint.len() != 64
                || !v
                    .configuration_fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                || v.pages.iter().any(|p| !matches!(p.schema_version, 0 | 2))
        }) {
            return Err(SettingsError::Decode);
        }
        Ok(value)
    }
    pub fn save_atomically(&self, path: impl AsRef<Path>) -> Result<(), SettingsError> {
        crate::settings::save_json_atomically(self, path.as_ref())
    }
    pub fn latest_http_upload(&self) -> Option<i64> {
        self.pages
            .iter()
            .filter_map(|p| p.last_successful_http_upload_epoch_seconds)
            .max()
    }
}

pub trait PagePublisher {
    fn publish_page(
        &mut self,
        page: u8,
        png: &[u8],
    ) -> Result<UploadCompletion, StatusFramePublishError>;
}
struct PageTarget<'a, P> {
    page: u8,
    publisher: &'a mut P,
}
impl<P: PagePublisher> StatusFramePublisher for PageTarget<'_, P> {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        self.publisher.publish_page(self.page, png)
    }
}
pub struct InkcardCoordinator {
    configuration_fingerprint: String,
    credentials_rejected: bool,
    pages: [StatusFrameCoordinator; 5],
    themes: [Option<BoardTheme>; 5],
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InkcardPublishError {
    ClockUnavailable,
    StatePersistUnavailable,
}
impl InkcardCoordinator {
    pub fn new(
        settings: &Settings,
        seed: &StatusFramePublisherState,
        saved: Option<InkcardState>,
    ) -> Result<Self, SettingsError> {
        let pages = InkcardSettings {
            mode: InkcardMode::All,
            pages: None,
        }
        .resolved_pages(settings, BoardTheme::default());
        Self::with_pages(settings, seed, saved, &pages)
    }
    pub fn with_pages(
        settings: &Settings,
        seed: &StatusFramePublisherState,
        saved: Option<InkcardState>,
        layout: &[InkcardPage],
    ) -> Result<Self, SettingsError> {
        validate_pages(layout)?;
        let mut themes = [None; 5];
        for page in layout {
            themes[usize::from(page.page - 1)] = Some(page.theme);
        }
        // Single-page state is unchanged during all-page operation. A successful setup
        // replaces that seed, invalidating old per-page equality/auth state even on the same device.
        let bytes = serde_json::to_vec(&(settings, seed)).map_err(|_| SettingsError::Encode)?;
        let fingerprint = format!("{:x}", Sha256::digest(bytes));
        let saved = saved.filter(|s| s.configuration_fingerprint == fingerprint);
        let rejected = saved.as_ref().is_some_and(|s| s.credentials_rejected);
        let states = saved.map(|s| s.pages).unwrap_or_else(|| {
            std::array::from_fn(|_| StatusFramePublisherState {
                last_successful_http_upload_epoch_seconds: seed
                    .last_successful_http_upload_epoch_seconds,
                next_allowed_attempt_epoch_seconds: seed.next_allowed_attempt_epoch_seconds,
                ..Default::default()
            })
        });
        Ok(Self {
            configuration_fingerprint: fingerprint,
            credentials_rejected: rejected,
            pages: states.map(StatusFrameCoordinator::new),
            themes,
        })
    }
    pub fn observe(&mut self, state: StatusBoardState) {
        for (page, theme) in self.pages.iter_mut().zip(self.themes) {
            let Some(theme) = theme else {
                continue;
            };
            let mut state = state.clone();
            state.theme = theme;
            page.observe(state);
        }
    }
    pub fn publish_due<R: StatusFrameRenderer, P: PagePublisher>(
        &mut self,
        mut now: impl FnMut() -> Option<i64>,
        renderer: &mut R,
        publisher: &mut P,
        mut may_publish: impl FnMut() -> bool,
        mut checkpoint: impl FnMut(&InkcardState) -> bool,
    ) -> Result<Vec<(u8, StatusFrameAttempt)>, InkcardPublishError> {
        if self.credentials_rejected {
            return Ok(Vec::new());
        }
        let mut attempts = Vec::new();
        for index in 0..self.pages.len() {
            if self.themes[index].is_none() {
                continue;
            }
            if !may_publish() {
                break;
            }
            let page = index as u8 + 1;
            // Earlier HTTP requests may have taken minutes. Each page owns its clock.
            let now = now().ok_or(InkcardPublishError::ClockUnavailable)?;
            let result =
                self.pages[index].try_publish(now, renderer, &mut PageTarget { page, publisher });
            attempts.push((page, result));
            if result == StatusFrameAttempt::CredentialsRejected {
                self.credentials_rejected = true;
            }
            // Preserve an acknowledged upload before starting another network request.
            if !checkpoint(&self.snapshot()) {
                return Err(InkcardPublishError::StatePersistUnavailable);
            }
            if self.credentials_rejected {
                break;
            }
        }
        Ok(attempts)
    }
    pub fn snapshot(&self) -> InkcardState {
        InkcardState {
            schema_version: 1,
            configuration_fingerprint: self.configuration_fingerprint.clone(),
            credentials_rejected: self.credentials_rejected,
            pages: std::array::from_fn(|i| self.pages[i].state().clone()),
        }
    }
    pub fn last_content_upload(&self) -> Option<i64> {
        self.pages
            .iter()
            .enumerate()
            .filter(|(i, _)| self.themes[*i].is_some())
            .map(|(_, p)| p)
            .map(|p| p.state().last_successful_upload_epoch_seconds)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .min()
    }
    pub fn next_deadline(&self, now: i64) -> Option<i64> {
        if self.credentials_rejected {
            return None;
        }
        self.pages
            .iter()
            .enumerate()
            .filter(|(i, _)| self.themes[*i].is_some())
            .map(|(_, p)| p)
            .filter(|p| p.has_pending())
            .filter_map(|p| p.state().next_allowed_attempt_epoch_seconds)
            .filter(|t| *t > now)
            .min()
    }
}
