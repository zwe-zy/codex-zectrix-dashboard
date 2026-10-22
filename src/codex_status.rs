use std::collections::HashSet;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::AppServerError;
use crate::app_server::AppServerClient;

// 64 pages permit observing up to 6,400 summaries while preventing an endless cursor chain.
const MAX_THREAD_LIST_PAGES: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct QuotaWindowStatus {
    pub remaining_percent: f64,
    pub resets_at_epoch_seconds: Option<i64>,
    pub window_duration_mins: Option<u32>,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct QuotaSnapshot {
    pub bucket_id: &'static str,
    pub primary: Option<QuotaWindowStatus>,
    pub secondary: Option<QuotaWindowStatus>,
    pub primary_present: bool,
    pub secondary_present: bool,
}
pub type QuotaStatus = QuotaSnapshot;
impl QuotaSnapshot {
    pub fn single(
        remaining_percent: f64,
        resets_at_epoch_seconds: Option<i64>,
        window_duration_mins: Option<u32>,
    ) -> Self {
        Self {
            bucket_id: "codex",
            primary: Some(QuotaWindowStatus {
                remaining_percent,
                resets_at_epoch_seconds,
                window_duration_mins,
            }),
            secondary: None,
            primary_present: true,
            secondary_present: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Running,
    Idle,
    Completed,
    Failed,
    Interrupted,
    Empty,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct TaskObservation {
    pub state: TaskState,
    pub partial: bool,
}

#[derive(Debug, Error)]
pub enum CodexStatusError {
    #[error("Codex status observation unavailable")]
    Unavailable,
    #[error("Codex status method unavailable")]
    MethodUnavailable,
    #[error("Codex status account unavailable")]
    AccountUnavailable,
    #[error("Codex status response incompatible")]
    Incompatible,
}

pub struct CodexStatusSource {
    client: AppServerClient,
    history_directory: Option<std::path::PathBuf>,
}

impl CodexStatusSource {
    pub fn new(client: AppServerClient) -> Self {
        Self {
            client,
            history_directory: None,
        }
    }

    pub fn with_history_directory(mut self, directory: impl Into<std::path::PathBuf>) -> Self {
        self.history_directory = Some(directory.into());
        self
    }

    pub fn read_quota_status(&self) -> Result<QuotaStatus, CodexStatusError> {
        let response = self
            .client
            .rpc_request("account/rateLimits/read", Value::Null)
            .map_err(map_app_server_error)?;
        parse_quota_status_value(response)
    }

    pub fn read_latest_task_state(&self) -> Result<TaskState, CodexStatusError> {
        self.read_latest_task_observation()
            .map(|observation| observation.state)
    }

    pub fn read_latest_task_observation(&self) -> Result<TaskObservation, CodexStatusError> {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let mut aggregate = TaskState::Empty;
        let mut partial = false;
        let mut recent_idle = Vec::new();
        let now = crate::platform::now_epoch_seconds().unwrap_or(0);
        let history = match &self.history_directory {
            Some(directory) => crate::local_task_history::LocalTaskHistory::open(directory),
            None => crate::local_task_history::LocalTaskHistory::from_environment(),
        }
        .ok();
        let mut reached_end = false;
        for _ in 0..MAX_THREAD_LIST_PAGES {
            let mut params = json!({
                "limit": 100,
                "sortKey": "recency_at",
                "sortDirection": "desc",
                "archived": false,
                "sourceKinds": ["cli", "vscode", "appServer"],
                "useStateDbOnly": true
            });
            if let Some(cursor) = cursor.as_ref() {
                params["cursor"] = Value::String(cursor.clone());
            }
            let response = self
                .client
                .rpc_request("thread/list", params)
                .map_err(map_app_server_error)?;
            let page = parse_thread_list_page_value(response)?;
            let mut unresolved = Vec::new();
            for thread in page.data {
                if matches!(thread.parent_thread_id, ParentThreadId::Root)
                    && !matches!(thread.status, ThreadStatus::Active { .. })
                    && let Some(state) = history.as_ref().and_then(|history| {
                        history
                            .read_state(
                                &thread.id,
                                crate::platform::now_epoch_seconds().unwrap_or(now),
                            )
                            .ok()
                            .flatten()
                    })
                {
                    aggregate = prioritize(aggregate, state);
                    continue;
                }
                partial |= matches!(thread.parent_thread_id, ParentThreadId::Missing)
                    || (matches!(thread.parent_thread_id, ParentThreadId::Root)
                        && matches!(
                            thread.status,
                            ThreadStatus::NotLoaded | ThreadStatus::SystemError
                        ));
                if matches!(thread.parent_thread_id, ParentThreadId::Root)
                    && matches!(thread.status, ThreadStatus::Idle)
                    && thread.updated_at <= now
                    && now.saturating_sub(thread.updated_at) < 86400
                    && recent_idle.len() < 8
                {
                    recent_idle.push(thread.id.clone());
                }
                unresolved.push(thread);
            }
            aggregate = aggregate_top_level_state(aggregate, unresolved);
            let Some(next_cursor) = page.next_cursor else {
                reached_end = true;
                break;
            };
            if next_cursor.is_empty() || !seen_cursors.insert(next_cursor.clone()) {
                return Err(CodexStatusError::Incompatible);
            }
            cursor = Some(next_cursor);
        }
        if !reached_end {
            return Err(CodexStatusError::Incompatible);
        }
        if aggregate != TaskState::Running {
            for id in recent_idle {
                match self
                    .client
                    .rpc_request("thread/read", json!({"threadId":id,"includeTurns":true}))
                    .map_err(map_app_server_error)
                    .and_then(|value| parse_task_state_at_value(value, now))
                {
                    Ok(state) => {
                        partial |= state == TaskState::Unavailable;
                        aggregate = prioritize(aggregate, state);
                    }
                    Err(_) => partial = true,
                }
            }
        }
        Ok(TaskObservation {
            state: aggregate,
            partial,
        })
    }
}

fn map_app_server_error(error: AppServerError) -> CodexStatusError {
    match error {
        AppServerError::RpcMethodUnavailable => CodexStatusError::MethodUnavailable,
        AppServerError::RpcAccountUnavailable => CodexStatusError::AccountUnavailable,
        _ => CodexStatusError::Unavailable,
    }
}

pub fn parse_quota_status(response: &str) -> Result<QuotaStatus, CodexStatusError> {
    let value = serde_json::from_str(response).map_err(|_| CodexStatusError::Incompatible)?;
    parse_quota_status_value(value)
}

pub fn parse_task_state(response: &str) -> Result<TaskState, CodexStatusError> {
    let value = serde_json::from_str(response).map_err(|_| CodexStatusError::Incompatible)?;
    parse_task_state_value(value)
}

pub fn render_status_diagnostic(quota: Option<&QuotaStatus>, task: TaskState) -> String {
    let quota = if quota.is_some() {
        "available"
    } else {
        "unavailable"
    };
    format!(
        "codex_quota_status={quota}\ncodex_task_status={}",
        task_state_label(task)
    )
}

fn task_state_label(state: TaskState) -> &'static str {
    match state {
        TaskState::Running => "running",
        TaskState::Idle => "idle",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Interrupted => "interrupted",
        TaskState::Empty => "empty",
        TaskState::Unavailable => "unavailable",
    }
}

fn parse_quota_status_value(response: Value) -> Result<QuotaStatus, CodexStatusError> {
    // Select raw data before parsing: a malformed unrelated group cannot erase Codex quota.
    let map = response
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object);
    let bucket = if let Some(codex) = map.and_then(|map| map.get("codex")) {
        codex
    } else {
        let legacy = response
            .get("rateLimits")
            .filter(|v| v.is_object())
            .ok_or(CodexStatusError::Incompatible)?;
        let legacy_id = legacy.get("limitId").and_then(Value::as_str);
        if legacy_id != Some("codex") && !(legacy_id.is_none() && map.is_none_or(|m| m.is_empty()))
        {
            return Err(CodexStatusError::Incompatible);
        }
        legacy
    };
    let primary = bucket.get("primary").and_then(parse_window);
    let secondary = bucket.get("secondary").and_then(parse_window);
    let primary_present = bucket.get("primary").is_some_and(|value| !value.is_null());
    let secondary_present = bucket
        .get("secondary")
        .is_some_and(|value| !value.is_null());
    if !bucket.is_object()
        || (primary.is_none() && secondary.is_none() && (primary_present || secondary_present))
    {
        return Err(CodexStatusError::Incompatible);
    }
    Ok(QuotaSnapshot {
        bucket_id: "codex",
        primary,
        secondary,
        primary_present,
        secondary_present,
    })
}
fn parse_window(value: &Value) -> Option<QuotaWindowStatus> {
    let used = value.get("usedPercent")?.as_f64()?;
    if !used.is_finite() || !(0.0..=100.0).contains(&used) {
        return None;
    }
    Some(QuotaWindowStatus {
        remaining_percent: 100.0 - used,
        resets_at_epoch_seconds: value
            .get("resetsAt")
            .and_then(Value::as_i64)
            .filter(|v| *v >= 0 && time::OffsetDateTime::from_unix_timestamp(*v).is_ok()),
        window_duration_mins: value
            .get("windowDurationMins")
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v > 0),
    })
}

pub fn parse_task_state_at(response: &str, now: i64) -> Result<TaskState, CodexStatusError> {
    parse_task_state_at_value(
        serde_json::from_str(response).map_err(|_| CodexStatusError::Incompatible)?,
        now,
    )
}
fn parse_task_state_at_value(value: Value, now: i64) -> Result<TaskState, CodexStatusError> {
    let thread = &value["thread"];
    if thread["status"]["type"] == "notLoaded" {
        return Ok(TaskState::Unavailable);
    }
    if thread["status"]["type"] == "active" {
        return parse_task_state_value(value);
    }
    let Some(turn) = thread["turns"].as_array().and_then(|turns| turns.last()) else {
        return Ok(TaskState::Idle);
    };
    let Some(ended) = turn["completedAt"]
        .as_i64()
        .filter(|time| *time >= 0 && *time <= now && now.saturating_sub(*time) < 86400)
    else {
        return Ok(TaskState::Unavailable);
    };
    let _ = ended;
    parse_task_state_value(value)
}

fn parse_task_state_value(response: Value) -> Result<TaskState, CodexStatusError> {
    let response: ThreadReadResponse =
        serde_json::from_value(response).map_err(|_| CodexStatusError::Incompatible)?;
    match response.thread.status {
        ThreadStatus::Active { active_flags } => {
            let _ = active_flags;
            return Ok(TaskState::Running);
        }
        ThreadStatus::SystemError => return Ok(TaskState::Failed),
        ThreadStatus::NotLoaded => return Ok(TaskState::Unavailable),
        ThreadStatus::Idle => {}
    }
    match response.thread.turns.last().map(|turn| {
        let _ = &turn.id;
        let _ = &turn.items;
        turn.status
    }) {
        Some(TurnStatus::InProgress) => Ok(TaskState::Running),
        Some(TurnStatus::Completed) => Ok(TaskState::Completed),
        Some(TurnStatus::Failed) => Ok(TaskState::Failed),
        Some(TurnStatus::Interrupted) => Ok(TaskState::Interrupted),
        None => Ok(TaskState::Empty),
    }
}

fn parse_thread_list_page_value(response: Value) -> Result<ThreadListPage, CodexStatusError> {
    let response: ThreadListResponse =
        serde_json::from_value(response).map_err(|_| CodexStatusError::Incompatible)?;
    Ok(ThreadListPage {
        data: response.data,
        next_cursor: response.next_cursor,
    })
}

fn aggregate_top_level_state(mut aggregate: TaskState, threads: Vec<ThreadSummary>) -> TaskState {
    for thread in threads {
        match thread.parent_thread_id {
            ParentThreadId::Root => {}
            ParentThreadId::Missing => {
                aggregate = prioritize(aggregate, TaskState::Unavailable);
                continue;
            }
            ParentThreadId::Child(parent) => {
                let _ = parent;
                continue;
            }
        }
        let _ = (&thread.id, thread.recency_at.unwrap_or(thread.updated_at));
        match thread.status {
            ThreadStatus::SystemError => aggregate = prioritize(aggregate, TaskState::Unavailable),
            ThreadStatus::Active { active_flags } => {
                let _ = active_flags;
                aggregate = TaskState::Running;
            }
            ThreadStatus::Idle => aggregate = prioritize(aggregate, TaskState::Idle),
            ThreadStatus::NotLoaded => aggregate = prioritize(aggregate, TaskState::Unavailable),
        }
    }
    aggregate
}
fn prioritize(a: TaskState, b: TaskState) -> TaskState {
    let rank = |s| match s {
        TaskState::Running => 6,
        TaskState::Failed => 5,
        TaskState::Interrupted => 4,
        TaskState::Completed => 3,
        TaskState::Unavailable => 2,
        TaskState::Idle => 1,
        TaskState::Empty => 0,
    };
    if rank(b) > rank(a) { b } else { a }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListResponse {
    data: Vec<ThreadSummary>,
    next_cursor: Option<String>,
}

struct ThreadListPage {
    data: Vec<ThreadSummary>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadSummary {
    id: String,
    #[serde(default, deserialize_with = "deserialize_parent_thread_id")]
    parent_thread_id: ParentThreadId,
    updated_at: i64,
    recency_at: Option<i64>,
    status: ThreadStatus,
}

#[derive(Default)]
enum ParentThreadId {
    #[default]
    Missing,
    Root,
    Child(String),
}

fn deserialize_parent_thread_id<'de, D>(deserializer: D) -> Result<ParentThreadId, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|parent| match parent {
        Some(parent) => ParentThreadId::Child(parent),
        None => ParentThreadId::Root,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
enum ThreadStatus {
    #[serde(rename = "active")]
    Active {
        #[serde(rename = "activeFlags")]
        active_flags: Vec<IgnoredAny>,
    },
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "systemError")]
    SystemError,
    #[serde(rename = "notLoaded")]
    NotLoaded,
}

#[derive(Deserialize)]
struct ThreadReadResponse {
    thread: ThreadDetails,
}

#[derive(Deserialize)]
struct ThreadDetails {
    status: ThreadStatus,
    turns: Vec<TurnSummary>,
}

#[derive(Deserialize)]
struct TurnSummary {
    #[serde(deserialize_with = "deserialize_ignored_string")]
    id: IgnoredAny,
    #[serde(deserialize_with = "deserialize_ignored_array")]
    items: IgnoredAny,
    status: TurnStatus,
}

fn deserialize_ignored_string<'de, D>(deserializer: D) -> Result<IgnoredAny, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let _ = String::deserialize(deserializer)?;
    Ok(IgnoredAny)
}

fn deserialize_ignored_array<'de, D>(deserializer: D) -> Result<IgnoredAny, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let _ = Vec::<IgnoredAny>::deserialize(deserializer)?;
    Ok(IgnoredAny)
}

#[derive(Clone, Copy, Deserialize)]
enum TurnStatus {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "interrupted")]
    Interrupted,
}
