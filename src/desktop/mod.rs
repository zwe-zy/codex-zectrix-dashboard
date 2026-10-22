//! UI-independent desktop commands and state. The companion remains the sole observer.
mod card_editor;
mod jobs;
mod model;
pub use card_editor::*;
pub use model::*;
#[cfg(feature = "desktop")]
pub mod app;
#[cfg(feature = "desktop")]
mod pages;
#[cfg(windows)]
mod service;
pub use jobs::*;
#[cfg(windows)]
pub use service::WindowsDesktopBackend;

use crate::{
    AutostartStatus, StatusFramePublisherState, desktop_state::*, inkcards::InkcardMode,
    status_board::BoardTheme,
};
use std::path::PathBuf;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobId(pub u64);
pub enum CredentialChoice {
    Saved,
    New(Zeroizing<String>),
}
impl std::fmt::Debug for CredentialChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialChoice([redacted])")
    }
}
#[derive(Debug)]
pub enum DesktopCommand {
    Refresh,
    CheckCodex,
    Discover {
        draft_id: DraftId,
        credential: CredentialChoice,
    },
    Prepare {
        draft_id: DraftId,
        device_index: usize,
        page_id: u8,
    },
    Commit {
        draft_id: DraftId,
    },
    Start,
    Stop,
    SetTheme(BoardTheme),
    SetMode(InkcardMode),
    SetCards(crate::inkcards::InkcardSettings),
    SetStartup(bool),
    Install {
        package_dir: PathBuf,
    },
    Uninstall {
        delete_data: bool,
        delete_key: bool,
    },
    Diagnose,
}
impl DesktopCommand {
    pub fn draft_id(&self) -> Option<DraftId> {
        match self {
            Self::Discover { draft_id, .. }
            | Self::Prepare { draft_id, .. }
            | Self::Commit { draft_id } => Some(*draft_id),
            _ => None,
        }
    }
    pub fn is_transaction(&self) -> bool {
        !matches!(
            self,
            Self::Refresh
                | Self::CheckCodex
                | Self::Discover { .. }
                | Self::Prepare { .. }
                | Self::Diagnose
        )
    }
}
#[derive(Debug)]
pub enum DesktopReply {
    Status(Box<DesktopStatus>),
    CodexChecked {
        warning: Option<String>,
    },
    Devices {
        draft_id: DraftId,
        labels: Vec<String>,
    },
    Preview {
        draft_id: DraftId,
        png: Vec<u8>,
    },
    Completed(OperationReport),
    Diagnostics(Vec<String>),
    RefreshFailed(DesktopError),
    Failed(DesktopError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopBackground {
    Stopped,
    Starting,
    Running,
    Unavailable,
}
#[derive(Debug)]
pub struct PageStatus {
    pub page: u8,
    pub theme: BoardTheme,
    pub record: StatusFramePublisherState,
    pub uploaded_png: Option<Vec<u8>>,
}
#[derive(Debug)]
pub struct DesktopStatus {
    pub installed_version: Option<String>,
    pub configured: bool,
    pub needs_recovery: bool,
    pub uninstall_pending: bool,
    pub read_only: bool,
    pub background: DesktopBackground,
    pub mode: InkcardMode,
    pub theme: BoardTheme,
    pub configured_page: u8,
    pub device_label: String,
    pub snapshot: Option<DesktopSnapshot>,
    pub freshness: DesktopFreshness,
    pub pages: Vec<PageStatus>,
    pub startup: AutostartStatus,
    pub credentials_rejected: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Installed,
    Configured,
    Updated,
    CardsSaved,
    UninstallPending,
}
#[derive(Debug)]
pub struct OperationReport {
    pub kind: OperationKind,
    pub warning: Option<String>,
}
impl OperationReport {
    pub fn updated() -> Self {
        Self {
            kind: OperationKind::Updated,
            warning: None,
        }
    }
}
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum DesktopError {
    #[error("已取消，未提交配置。")]
    Cancelled,
    #[error("程序版本已变化，请关闭本窗口并打开新版控制台。")]
    InstallationChanged,
    #[error("请先安装当前版本，再配置或控制设备。")]
    NotInstalled,
    #[error("上次操作未完成，请重新安装或配置以恢复。")]
    RecoveryRequired,
    #[error("本地设置无法读取，请检查目录权限或使用安装恢复。")]
    LocalUnavailable,
    #[error("Windows 凭据管理器不可用，请重试或重新配置 Key。")]
    CredentialUnavailable,
    #[error("草稿已失效，请重新获取设备列表。")]
    DraftExpired,
    #[error("后台操作未能完成，请重试；必要时查看诊断。")]
    WorkerUnavailable,
    #[error("{0}")]
    Operation(String),
}
