#[cfg(unix)]
#[rustfmt::skip]
mod unix_tests {
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use codex_zectrix_dashboard::{AppServerClient, CorrelationKey};

const SUPPORTED_USER_AGENT: &str = "codex-zectrix-dashboard/0.146.1 (Mac OS 26.5; arm64) dumb";

#[test]
fn standalone_client_initializes_then_reads_quota_without_other_requests() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"{SUPPORTED_USER_AGENT}","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}}}'
read -r initialized
printf '%s\n' "$initialized" >> '{}'
read -r quota
printf '%s\n' "$quota" >> '{}'
printf '%s\n' '{{"id":2,"result":{{"rateLimits":{{"primary":{{"usedPercent":37,"windowDurationMins":300,"resetsAt":1786337200}},"secondary":null}},"rateLimitResetCredits":{{"availableCount":0}},"account":{{"email":"SECRET_ACCOUNT_MARKER"}},"accessToken":"SECRET_TOKEN_MARKER"}}}}'
"#,
        requests.display(),
        requests.display(),
        requests.display()
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let quota = AppServerClient::new(&server).read_quota().unwrap();

    assert_eq!(quota.windows.len(), 1);
    assert_eq!(quota.windows[0].used_percent, 37);
    let messages: Vec<serde_json::Value> = fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["method"], "initialize");
    assert_eq!(
        messages[0]["params"]["clientInfo"]["name"],
        "codex-zectrix-dashboard"
    );
    assert_eq!(messages[1]["method"], "initialized");
    assert_eq!(messages[2]["method"], "account/rateLimits/read");
    assert!(messages[2]["params"].is_null());
}

#[test]
fn slow_quota_response_keeps_the_persistent_session() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let invocation_count = temp.path().join("invocations");
    let script = r#"#!/bin/sh
count=0
[ ! -f "__COUNT__" ] || count=$(cat "__COUNT__")
count=$((count + 1))
printf '%s' "$count" > "__COUNT__"
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
read -r quota
request_id=$(printf '%s\n' "$quota" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
sleep 6
printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
read -r quota
request_id=$(printf '%s\n' "$quota" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":41,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
"#
    .replace("__COUNT__", &invocation_count.to_string_lossy());
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();
    let client = AppServerClient::new(&server);

    let first = client.read_quota().unwrap();
    let second = client.read_quota().unwrap();

    assert_eq!(first.windows[0].used_percent, 37);
    assert_eq!(second.windows[0].used_percent, 41);
    assert_eq!(fs::read_to_string(invocation_count).unwrap(), "1");
}

#[test]
fn unavailable_app_server_returns_an_error_without_synthesizing_quota() {
    let error = AppServerClient::new("/path/that/does/not/exist")
        .read_quota()
        .unwrap_err();

    assert!(error.to_string().contains("启动 Codex app-server"));
}

#[test]
fn harmless_app_server_version_change_keeps_recognized_quota_available() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let fixture = format!(
        "{}/fixtures/app-server-0.148-additive.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let script = format!(
        r#"#!/bin/sh
read -r initialize
sed -n '1p' '{}'
read -r initialized
read -r request
sed -n '2p' '{}'
"#,
        fixture, fixture
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let quota = AppServerClient::new(&server).read_quota().unwrap();

    assert_eq!(quota.windows.len(), 1);
    assert_eq!(quota.windows[0].used_percent, 37);
}

#[test]
fn rpc_error_details_are_not_exposed_by_the_client_error() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    fs::write(
        &server,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"error":{"message":"SECRET_TOKEN_MARKER","accessToken":"SECRET_TOKEN_MARKER"}}'
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let error = AppServerClient::new(&server).read_quota().unwrap_err();

    assert!(!error.to_string().contains("SECRET_TOKEN_MARKER"));
}

#[test]
fn unusable_protocol_stream_is_replaced_after_the_reconnect_delay() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let invocation_count = temp.path().join("invocations");
    let script = r#"#!/bin/sh
count=0
[ ! -f "__COUNT__" ] || count=$(cat "__COUNT__")
count=$((count + 1))
printf '%s' "$count" > "__COUNT__"
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
read -r request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
if [ "$count" -eq 1 ]; then
  printf '%s\n' "{\"id\":$request_id}"
  while read -r request; do
    request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
    printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":99,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
  done
else
  printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":41,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
fi
"#
    .replace("__COUNT__", &invocation_count.to_string_lossy());
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();
    let now = Arc::new(AtomicU64::new(0));
    let clock = Arc::clone(&now);
    let client = AppServerClient::new_with_clock(&server, move || {
        Duration::from_secs(clock.load(Ordering::Relaxed))
    });

    assert!(client.read_quota().is_err());
    assert!(client.read_quota().is_err());
    assert_eq!(fs::read_to_string(&invocation_count).unwrap(), "1");
    now.store(5, Ordering::Relaxed);
    let quota = client.read_quota().unwrap();

    assert_eq!(quota.windows[0].used_percent, 41);
    assert_eq!(fs::read_to_string(invocation_count).unwrap(), "2");
}

#[test]
fn delayed_response_from_a_timed_out_session_cannot_satisfy_a_later_request() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let invocation_count = temp.path().join("timeout-invocations");
    let script = r#"#!/bin/sh
count=0
[ ! -f "__COUNT__" ] || count=$(cat "__COUNT__")
count=$((count + 1))
printf '%s' "$count" > "__COUNT__"
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
read -r request
request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
if [ "$count" -eq 1 ]; then
  notifications=0
  while [ "$notifications" -lt 6 ]; do
    printf '%s\n' '{"method":"account/updated"}'
    notifications=$((notifications + 1))
    sleep 1
  done
  printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":null}}"
else
  printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":41,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
fi
"#
    .replace("__COUNT__", &invocation_count.to_string_lossy());
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();
    let now = Arc::new(AtomicU64::new(0));
    let clock = Arc::clone(&now);
    let client = AppServerClient::new_with_clock(&server, move || {
        Duration::from_secs(clock.load(Ordering::Relaxed))
    });

    let error = client.read_task_metadata("test-installation").unwrap_err();
    assert!(error.to_string().contains("限定时间内响应"));
    assert!(client.read_task_metadata("test-installation").is_err());
    assert_eq!(fs::read_to_string(&invocation_count).unwrap(), "1");
    now.store(5, Ordering::Relaxed);
    let quota = client.read_quota().unwrap();

    assert_eq!(quota.windows[0].used_percent, 41);
    assert_eq!(fs::read_to_string(invocation_count).unwrap(), "2");
}

#[test]
fn standalone_client_reads_official_titles_without_rollout_scan_or_repair() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"{SUPPORTED_USER_AGENT}"}}}}'
read -r initialized
printf '%s\n' "$initialized" >> '{}'
read -r threads
printf '%s\n' "$threads" >> '{}'
printf '%s\n' '{{"id":2,"result":{{"data":[{{"id":"different-thread-id","sessionId":"task-1","name":"官方任务标题","parentThreadId":null,"source":"appServer","preview":"SECRET_PROMPT","cwd":"SECRET_PATH"}}],"nextCursor":null}}}}'
"#,
        requests.display(),
        requests.display(),
        requests.display()
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let tasks = AppServerClient::new(&server)
        .read_task_metadata("test-installation")
        .unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].title, "官方任务标题");
    assert_eq!(
        tasks[0].correlation,
        CorrelationKey::derive("different-thread-id", "test-installation")
    );
    let messages: Vec<serde_json::Value> = fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["method"], "thread/list");
    assert_eq!(messages[2]["params"]["useStateDbOnly"], true);
    assert_eq!(messages[2]["params"]["sortKey"], "updated_at");
    assert_eq!(messages[2]["params"]["sortDirection"], "desc");
    assert_eq!(messages[2]["params"]["archived"], false);
    assert!(
        messages[2]["params"]["sourceKinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == "subAgent")
    );
    assert!(!format!("{tasks:?}").contains("SECRET_"));
}

#[test]
fn standalone_client_follows_every_read_only_task_metadata_page() {
    let temp = tempfile::tempdir().unwrap();
    let server = temp.path().join("fake-codex");
    let requests = temp.path().join("requests.jsonl");
    let script = format!(
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{{"id":1,"result":{{"userAgent":"{SUPPORTED_USER_AGENT}"}}}}'
read -r initialized
page=0
while read -r threads; do
  printf '%s\n' "$threads" >> '{}'
  request_id=$(printf '%s\n' "$threads" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  page=$((page + 1))
  if [ "$page" -eq 1 ]; then
    printf '%s\n' "{{\"id\":$request_id,\"result\":{{\"data\":[{{\"id\":\"thread-1\",\"sessionId\":\"session-1\",\"name\":\"第一页\",\"parentThreadId\":null,\"source\":\"appServer\"}}],\"nextCursor\":\"cursor-2\"}}}}"
  else
    printf '%s\n' "{{\"id\":$request_id,\"result\":{{\"data\":[{{\"id\":\"thread-2\",\"sessionId\":\"session-2\",\"name\":\"第二页\",\"parentThreadId\":null,\"source\":\"appServer\"}}],\"nextCursor\":null}}}}"
  fi
done
"#,
        requests.display()
    );
    fs::write(&server, script).unwrap();
    let mut permissions = fs::metadata(&server).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&server, permissions).unwrap();

    let tasks = AppServerClient::new(&server)
        .read_task_metadata("test-installation")
        .unwrap();

    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].title, "第一页");
    assert_eq!(tasks[1].title, "第二页");
    let requests: Vec<serde_json::Value> = fs::read_to_string(requests)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["params"]["cursor"].is_null());
    assert_eq!(requests[1]["params"]["cursor"], "cursor-2");
    assert!(
        requests
            .iter()
            .all(|request| { request["params"]["useStateDbOnly"] == true })
    );
}
}

#[cfg(feature = "test-support")]
#[test]
fn dropping_the_client_reaps_the_owned_app_server() {
    use codex_zectrix_dashboard::AppServerClient;

    let temp = tempfile::tempdir().unwrap();
    let (program, process_record, _) = copied_fake_app_server(temp.path(), "owned");

    {
        let client = AppServerClient::new(&program);
        assert_eq!(client.read_quota().unwrap().windows[0].used_percent, 24);
    }

    assert!(wait_until_process_record_is_gone(&process_record).unwrap());
}

#[cfg(all(feature = "test-support", windows))]
#[test]
fn owned_process_starts_in_the_job_and_drop_reaps_without_an_external_child_owner() {
    use std::io::{BufRead, BufReader, Write};
    use std::time::Duration;

    use codex_zectrix_dashboard::platform::OwnedProcess;

    let temp = tempfile::tempdir().unwrap();
    let (program, process_record, started_in_job) =
        copied_fake_app_server(temp.path(), "direct-owner");
    let mut process = OwnedProcess::spawn_codex(&program).unwrap();
    let mut stdin = process.take_stdin().unwrap();
    let mut stdout = BufReader::new(process.take_stdout().unwrap());

    writeln!(stdin, r#"{{"id":1,"method":"initialize","params":null}}"#).unwrap();
    stdin.flush().unwrap();
    let mut response = String::new();
    stdout.read_line(&mut response).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response).unwrap()["id"],
        1
    );
    assert_eq!(std::fs::read_to_string(started_in_job).unwrap(), "true");

    drop(stdin);
    assert!(process.wait_for_exit(Duration::from_secs(3)));
    drop(process);

    assert!(wait_until_process_record_is_gone(&process_record).unwrap());
}

#[cfg(all(feature = "test-support", windows))]
#[test]
fn process_observation_errors_are_not_classified_as_death() {
    let access_denied = std::io::Error::from_raw_os_error(5);

    let error = classify_process_observation(Err(access_denied)).unwrap_err();

    assert_eq!(error.raw_os_error(), Some(5));
}

#[cfg(all(feature = "test-support", windows))]
fn classify_process_observation(observation: std::io::Result<bool>) -> std::io::Result<bool> {
    observation
}

#[cfg(feature = "test-support")]
fn copied_fake_app_server(
    directory: &std::path::Path,
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    use std::fs;

    let process_record = directory.join(format!("{name}.process-record"));
    fs::write(&process_record, []).unwrap();
    let program = process_record.with_extension(std::env::consts::EXE_EXTENSION);
    fs::copy(
        env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"),
        &program,
    )
    .unwrap();
    let started_in_job = program.with_extension("started-in-job");
    fs::write(&started_in_job, []).unwrap();
    (program, process_record, started_in_job)
}

#[cfg(feature = "test-support")]
fn wait_until_process_record_is_gone(path: &std::path::Path) -> std::io::Result<bool> {
    use std::fs;
    use std::time::Duration;

    let process_ids: Vec<u32> = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect();
    assert_eq!(
        process_ids.len(),
        2,
        "fake app-server did not record its descendant"
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let all_dead = process_ids
            .iter()
            .copied()
            .map(process_is_alive)
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .all(|is_alive| !is_alive);
        if all_dead {
            fs::remove_file(path).unwrap();
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(all(feature = "test-support", unix))]
fn process_is_alive(process_id: u32) -> std::io::Result<bool> {
    let process_id = process_id.try_into().unwrap();
    if unsafe { libc::kill(process_id, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(all(feature = "test-support", windows))]
fn process_is_alive(process_id: u32) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // OpenProcess either returns null or a new owned handle for this test to close.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(false);
        }
        return classify_process_observation(Err(error));
    }
    // The non-null handle was freshly returned and is wrapped exactly once.
    let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle.cast()) };
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) };
    if queried == 0 {
        return classify_process_observation(Err(std::io::Error::last_os_error()));
    }
    classify_process_observation(Ok(exit_code == STILL_ACTIVE as u32))
}
