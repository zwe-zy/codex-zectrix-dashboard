use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::activity_sources::parse_app_server_task_page;
use crate::platform::{OwnedProcess, ProcessError};
use crate::{
    ActivitySourceError, DashboardError, ObservedQuota, OfficialTaskMetadata, ReconnectPolicy,
    parse_app_server_quota,
};

const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const QUOTA_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct AppServerClient {
    program: PathBuf,
    locator: Option<crate::codex_discovery::CodexLocator>,
    clock: Arc<dyn Fn() -> Duration + Send + Sync>,
    state: Arc<Mutex<AppServerState>>,
}

#[derive(Default)]
struct AppServerState {
    session: Option<AppServerSession>,
    reconnect: ReconnectPolicy,
}

impl AppServerClient {
    pub fn new(program: impl AsRef<Path>) -> Self {
        let started_at = Instant::now();
        Self::new_with_clock(program, move || started_at.elapsed())
    }

    pub fn new_with_clock(
        program: impl AsRef<Path>,
        clock: impl Fn() -> Duration + Send + Sync + 'static,
    ) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            locator: None,
            clock: Arc::new(clock),
            state: Arc::new(Mutex::new(AppServerState::default())),
        }
    }

    pub fn read_quota(&self) -> Result<ObservedQuota, AppServerError> {
        let result = self.rpc_request("account/rateLimits/read", Value::Null)?;
        parse_app_server_quota(&serde_json::to_string(&result)?).map_err(AppServerError::Quota)
    }

    pub fn read_task_metadata(
        &self,
        installation_salt: &str,
    ) -> Result<Vec<OfficialTaskMetadata>, AppServerError> {
        let mut cursor = None;
        let mut seen_cursors = HashSet::new();
        let mut tasks = Vec::new();
        loop {
            let result = self.rpc_request(
                "thread/list",
                json!({
                    "cursor": cursor,
                    "limit": 100,
                    "sortKey": "updated_at",
                    "sortDirection": "desc",
                    "archived": false,
                    "sourceKinds": [
                        "cli", "vscode", "exec", "appServer", "subAgent", "subAgentReview",
                        "subAgentCompact", "subAgentThreadSpawn", "subAgentOther", "unknown"
                    ],
                    "useStateDbOnly": true
                }),
            )?;
            let page =
                parse_app_server_task_page(&serde_json::to_string(&result)?, installation_salt)?;
            tasks.extend(page.tasks);
            let Some(next_cursor) = page.next_cursor else {
                return Ok(tasks);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(AppServerError::Activity(
                    ActivitySourceError::UnsupportedTaskMetadata,
                ));
            }
            cursor = Some(next_cursor);
        }
    }

    pub fn list_hooks(&self, cwd: &Path) -> Result<Vec<HookMetadata>, AppServerError> {
        let result = self.rpc_request("hooks/list", json!({ "cwds": [cwd] }))?;
        let data = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or(AppServerError::MissingResult)?;
        let entry = data.first().ok_or(AppServerError::MissingResult)?;
        if entry
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err(AppServerError::Rpc);
        }
        serde_json::from_value(
            entry
                .get("hooks")
                .cloned()
                .ok_or(AppServerError::MissingResult)?,
        )
        .map_err(AppServerError::Json)
    }

    pub fn configure_hooks(
        &self,
        hooks: &[HookMetadata],
        enabled: bool,
    ) -> Result<(), AppServerError> {
        let value = hooks
            .iter()
            .map(|hook| {
                (
                    hook.key.clone(),
                    json!({
                        "enabled": enabled,
                        "trusted_hash": hook.current_hash,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        self.rpc_request(
            "config/batchWrite",
            json!({
                "edits": [{
                    "keyPath": "hooks.state",
                    "value": value,
                    "mergeStrategy": "upsert"
                }],
                "reloadUserConfig": true
            }),
        )?;
        Ok(())
    }

    pub(crate) fn rpc_request(&self, method: &str, params: Value) -> Result<Value, AppServerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = (self.clock)();
        if state.session.is_none() {
            if let Some(retry_after) = state.reconnect.retry_after(now) {
                return Err(AppServerError::ReconnectDeferred { retry_after });
            }
            let programs = match &self.locator {
                Some(locator) => match locator.candidates() {
                    Ok(resolved) => resolved
                        .into_iter()
                        .map(|candidate| candidate.path)
                        .collect::<Vec<_>>(),
                    Err(_) => {
                        state.reconnect.record_transport_failure((self.clock)());
                        return Err(AppServerError::Io(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "Codex executable unavailable",
                        )));
                    }
                },
                None => vec![self.program.clone()],
            };
            let mut last_error = None;
            for program in programs {
                match AppServerSession::start(&program) {
                    Ok(session) => {
                        state.reconnect.record_session_started((self.clock)());
                        state.session = Some(session);
                        break;
                    }
                    Err(error) => {
                        last_error = Some(error);
                    }
                }
            }
            if state.session.is_none() {
                state.reconnect.record_transport_failure((self.clock)());
                return Err(last_error.unwrap_or(AppServerError::MissingResult));
            }
        }
        let result = state.session.as_mut().unwrap().request(method, params);
        if result
            .as_ref()
            .is_err_and(AppServerError::invalidates_session)
        {
            drop(state.session.take());
            state.reconnect.record_transport_failure((self.clock)());
        } else {
            state.reconnect.record_transport_healthy((self.clock)());
        }
        result
    }
}

impl std::fmt::Debug for AppServerClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppServerClient")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

struct AppServerSession {
    process: OwnedProcess,
    stdin: Option<ChildStdin>,
    receiver: Receiver<std::io::Result<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
    next_request_id: u64,
}

impl AppServerSession {
    fn start(program: &Path) -> Result<Self, AppServerError> {
        let mut process = OwnedProcess::spawn_codex(program)?;
        let stdin = process.take_stdin().ok_or(AppServerError::MissingPipe)?;
        let stdout = process.take_stdout().ok_or(AppServerError::MissingPipe)?;
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        let mut session = Self {
            process,
            stdin: Some(stdin),
            receiver,
            reader: Some(reader),
            next_request_id: 2,
        };
        let result: Result<(), AppServerError> = (|| {
            write_message(
                session.stdin.as_mut().unwrap(),
                &json!({
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "clientInfo": {
                            "name": "codex-zectrix-dashboard",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }
                }),
            )?;
            read_result(&session.receiver, 1, DEFAULT_RESPONSE_TIMEOUT)?;

            write_message(
                session.stdin.as_mut().unwrap(),
                &json!({ "method": "initialized" }),
            )?;
            Ok(())
        })();
        result?;
        Ok(session)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerError> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        write_message(
            self.stdin.as_mut().ok_or(AppServerError::MissingPipe)?,
            &json!({
                "id": id,
                "method": method,
                "params": params
            }),
        )?;
        let timeout = if method == "account/rateLimits/read" {
            QUOTA_RESPONSE_TIMEOUT
        } else {
            DEFAULT_RESPONSE_TIMEOUT
        };
        read_result(&self.receiver, id, timeout)
    }
}

impl Drop for AppServerSession {
    fn drop(&mut self) {
        self.stdin.take();
        if !self.process.wait_for_exit(Duration::from_secs(3)) {
            self.process.request_termination();
            let _ = self.process.wait_for_exit(Duration::from_secs(2));
        }
        self.process.force_termination();
        self.process.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Default for AppServerClient {
    fn default() -> Self {
        let mut client = Self::new("codex");
        client.locator = Some(crate::codex_discovery::CodexLocator::from_environment());
        client
    }
}

#[cfg(windows)]
pub(crate) fn find_installed_codex_candidates(local_app_data: &Path) -> Vec<PathBuf> {
    let bin_dir = local_app_data.join("OpenAI").join("Codex").join("bin");
    let Ok(entries) = std::fs::read_dir(bin_dir) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path().join("codex.exe");
            path.is_file().then_some(path)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        let left_modified = left.metadata().and_then(|value| value.modified()).ok();
        let right_modified = right.metadata().and_then(|value| value.modified()).ok();
        right_modified
            .cmp(&left_modified)
            .then_with(|| right.cmp(left))
    });
    candidates
}

#[cfg(all(test, windows))]
mod installed_codex_tests {
    fn find_installed_codex(path: &std::path::Path) -> Option<std::path::PathBuf> {
        super::find_installed_codex_candidates(path)
            .into_iter()
            .next()
    }

    #[test]
    fn discovers_codex_inside_the_versioned_desktop_bin_directory() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp
            .path()
            .join("OpenAI")
            .join("Codex")
            .join("bin")
            .join("version-hash")
            .join("codex.exe");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, b"test executable").unwrap();

        assert_eq!(find_installed_codex(temp.path()), Some(executable));
    }

    #[test]
    fn missing_desktop_install_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(find_installed_codex(temp.path()), None);
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookMetadata {
    pub key: String,
    pub event_name: String,
    pub handler_type: String,
    pub execution_mode: Option<String>,
    pub matcher: Option<String>,
    pub command: Option<String>,
    pub timeout_sec: u64,
    pub status_message: Option<String>,
    pub additional_context_limit: Option<u64>,
    pub source_path: PathBuf,
    pub plugin_id: Option<String>,
    pub enabled: bool,
    pub is_managed: bool,
    pub current_hash: String,
    pub trust_status: String,
}

fn write_message(writer: &mut impl Write, message: &Value) -> Result<(), AppServerError> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_result(
    receiver: &Receiver<std::io::Result<String>>,
    id: u64,
    timeout: Duration,
) -> Result<Value, AppServerError> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AppServerError::Timeout);
        }
        let line = receiver
            .recv_timeout(remaining)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => AppServerError::Timeout,
                RecvTimeoutError::Disconnected => AppServerError::Closed,
            })?
            .map_err(AppServerError::Io)?;
        let message: Value = serde_json::from_str(&line)?;
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(classify_rpc_error(error));
        }
        return message
            .get("result")
            .cloned()
            .ok_or(AppServerError::ProtocolMissingResult);
    }
}

fn classify_rpc_error(error: &Value) -> AppServerError {
    if error.get("code").and_then(Value::as_i64) == Some(-32601) {
        return AppServerError::RpcMethodUnavailable;
    }
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [
        "account",
        "auth",
        "login",
        "credential",
        "subscription",
        "plan",
    ]
    .iter()
    .any(|marker| message.contains(marker))
    {
        return AppServerError::RpcAccountUnavailable;
    }
    AppServerError::Rpc
}

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error("Codex app-server 未提供标准输入输出管道")]
    MissingPipe,
    #[error("Codex app-server 未在限定时间内响应")]
    Timeout,
    #[error("Codex app-server 在返回响应前结束")]
    Closed,
    #[error("Codex app-server 输入输出失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("Codex app-server 返回了无效 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex app-server 拒绝了请求")]
    Rpc,
    #[error("Codex app-server 未提供所需方法")]
    RpcMethodUnavailable,
    #[error("Codex app-server 账户状态不可用")]
    RpcAccountUnavailable,
    #[error("Codex app-server 响应缺少结果")]
    MissingResult,
    #[error("Codex app-server 协议响应缺少结果")]
    ProtocolMissingResult,
    #[error("Codex app-server 重连延后 {retry_after:?}")]
    ReconnectDeferred { retry_after: Duration },
    #[error(transparent)]
    Quota(#[from] DashboardError),
    #[error(transparent)]
    Activity(#[from] ActivitySourceError),
}

impl AppServerError {
    pub fn is_transport_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Process(_)
                | Self::MissingPipe
                | Self::Timeout
                | Self::Closed
                | Self::Io(_)
                | Self::Json(_)
                | Self::ProtocolMissingResult
                | Self::ReconnectDeferred { .. }
        )
    }

    fn invalidates_session(&self) -> bool {
        matches!(
            self,
            Self::MissingPipe
                | Self::Timeout
                | Self::Closed
                | Self::Io(_)
                | Self::Json(_)
                | Self::ProtocolMissingResult
        )
    }
}
