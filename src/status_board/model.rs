use serde::Serialize;
use thiserror::Error;

use crate::{QuotaStatus, TaskState};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoardTheme {
    #[default]
    PopCut,
    Eva,
    PixelConsole,
    SpaceGauge,
    CyberHud,
}
impl BoardTheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PopCut => "pop-cut",
            Self::Eva => "eva",
            Self::PixelConsole => "pixel-console",
            Self::SpaceGauge => "space-gauge",
            Self::CyberHud => "cyber-hud",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::PopCut => "怪盗剪贴风",
            Self::Eva => "EVA 机库终端风",
            Self::PixelConsole => "像素掌机风",
            Self::SpaceGauge => "航天仪表风",
            Self::CyberHud => "赛博 HUD 风",
        }
    }
}
impl std::str::FromStr for BoardTheme {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pop-cut" => Ok(Self::PopCut),
            "eva" => Ok(Self::Eva),
            "pixel-console" => Ok(Self::PixelConsole),
            "space-gauge" => Ok(Self::SpaceGauge),
            "cyber-hud" => Ok(Self::CyberHud),
            _ => Err("主题必须为 pop-cut、eva、pixel-console、space-gauge 或 cyber-hud"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum BoardTaskState {
    Running,
    Idle,
    Completed,
    Failed,
    Interrupted,
    Empty,
    #[default]
    Unavailable,
}

impl BoardTaskState {
    pub const fn large_label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Idle => "空闲",
            Self::Completed => "本轮完成",
            Self::Failed => "失败",
            Self::Interrupted => "已中断",
            Self::Empty => "无任务",
            Self::Unavailable => "状态未知",
        }
    }

    pub const fn small_code(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Idle => "IDLE",
            Self::Completed => "DONE",
            Self::Failed => "FAILED",
            Self::Interrupted => "STOPPED",
            Self::Empty => "EMPTY",
            Self::Unavailable => "OFFLINE",
        }
    }
}

impl From<TaskState> for BoardTaskState {
    fn from(state: TaskState) -> Self {
        match state {
            TaskState::Running => Self::Running,
            TaskState::Idle => Self::Idle,
            TaskState::Completed => Self::Completed,
            TaskState::Failed => Self::Failed,
            TaskState::Interrupted => Self::Interrupted,
            TaskState::Empty => Self::Empty,
            TaskState::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardQuotaWindow {
    pub remaining_percent: f64,
    pub resets_at_epoch_seconds: Option<i64>,
    pub window_duration_mins: Option<u32>,
    pub stale: bool,
}
#[derive(Debug, Default, Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusBoardState {
    pub theme: BoardTheme,
    pub task_state: BoardTaskState,
    pub task_partial: bool,
    pub quota_remaining_percent: Option<f64>,
    pub quota_window_duration_mins: Option<u32>,
    pub quota_secondary: Option<BoardQuotaWindow>,
    pub is_sample: bool,
    pub quota_resets_at_epoch_seconds: Option<i64>,
    pub quota_stale: bool,
    pub task_stale: bool,
    pub uploaded_at_epoch_seconds: Option<i64>,
}

impl StatusBoardState {
    pub fn from_codex_status(
        task_state: TaskState,
        quota: Option<QuotaStatus>,
        quota_stale: bool,
        task_stale: bool,
        uploaded_at_epoch_seconds: Option<i64>,
    ) -> Result<Self, StatusBoardModelError> {
        if quota.is_some_and(|quota| {
            [quota.primary, quota.secondary]
                .into_iter()
                .flatten()
                .any(|window| {
                    !window.remaining_percent.is_finite()
                        || !(0.0..=100.0).contains(&window.remaining_percent)
                })
        }) {
            return Err(StatusBoardModelError::QuotaOutOfRange);
        }

        Ok(Self {
            theme: BoardTheme::default(),
            task_state: task_state.into(),
            task_partial: task_state == TaskState::Unavailable,
            quota_remaining_percent: quota
                .and_then(|quota| quota.primary)
                .map(|window| window.remaining_percent),
            quota_resets_at_epoch_seconds: quota
                .and_then(|quota| quota.primary)
                .and_then(|window| window.resets_at_epoch_seconds),
            quota_window_duration_mins: quota
                .and_then(|quota| quota.primary)
                .and_then(|window| window.window_duration_mins),
            quota_secondary: quota.and_then(|quota| quota.secondary).map(|window| {
                BoardQuotaWindow {
                    remaining_percent: window.remaining_percent,
                    resets_at_epoch_seconds: window.resets_at_epoch_seconds,
                    window_duration_mins: window.window_duration_mins,
                    stale: quota_stale,
                }
            }),
            is_sample: false,
            quota_stale,
            task_stale,
            uploaded_at_epoch_seconds,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum StatusBoardModelError {
    #[error("quota percentage outside display range")]
    QuotaOutOfRange,
}
