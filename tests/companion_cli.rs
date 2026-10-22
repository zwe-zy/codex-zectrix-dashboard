#[cfg(unix)]
#[rustfmt::skip]
mod unix_tests {
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    ActivityEvent, ActivityEventKind, ActivityState, CorrelationKey, DashboardConfig,
    ObservedDashboardState, ObservedQuota, ObservedQuotaWindow, ObservedTask, RadarDisplayState,
    ResetKind, TaskActivityAvailability, normalize_dashboard,
    render_normalized_dashboard_with_sync,
};

mod common {
    pub fn dashboard_binary() -> std::path::PathBuf {
        std::env::var_os("CODEX_ZECTRIX_TEST_BINARY")
            .map(Into::into)
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_codex-zectrix-dashboard").into())
    }
}

const PROCESS_START_TIMEOUT: Duration = Duration::from_secs(15);
const PROCESS_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn companion_publishes_current_quota_with_a_compatibility_frame_instead_of_cached_activity() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    fs::write(
        data_dir.join("activity.json"),
        r#"[{"title":"SECRET_TASK_TITLE","state":"running","activity_at_epoch_seconds":4102444800}]"#,
    )
    .unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let codex_log = temp.path().join("codex.log");
    let codex = fake_codex_command(temp.path());
    let security = fake_security_command(temp.path());
    let started_at = current_epoch_seconds();
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env("CODEX_ZECTRIX_CODEX_BIN", &codex)
        .env("CODEX_ZECTRIX_SECURITY_BIN", &security)
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "2")
        .env("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS", "0")
        .output()
        .unwrap();
    let finished_at = current_epoch_seconds();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = request.recv().unwrap();
    assert!(
        request
            .0
            .contains("content-type: multipart/form-data; boundary=")
    );
    let png = extract_png(&request.1);
    assert!(!contains(&request.1, secret.as_bytes()));
    assert!(!request.0.lines().next().unwrap().contains(secret));
    let image = image::load_from_memory(png).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    let expected_frame = (started_at..=finished_at).any(|render_time| {
        let observed = ObservedDashboardState {
            quota: ObservedQuota {
                windows: vec![ObservedQuotaWindow {
                    name: "5 小时".into(),
                    used_percent: 37,
                    resets_at_epoch_seconds: 4_102_444_800,
                }],
                reset_credits: 0,
                stale: false,
            },
            radar: Default::default(),
            task_activity_availability: TaskActivityAvailability::Unavailable,
            task_activity_stale: false,
            tasks: vec![ObservedTask::new(
                "SECRET_TASK_TITLE",
                ActivityState::Running,
                4_102_444_800,
            )],
            prompt: None,
            response: None,
            reasoning: None,
            project_path: None,
            tool: None,
            error_text: None,
            plan: None,
        };
        let normalized = normalize_dashboard(observed, render_time, &DashboardConfig::default());
        render_normalized_dashboard_with_sync(
            normalized,
            render_time,
            DashboardConfig::default(),
            Some(render_time),
        )
        .is_ok_and(|dashboard| dashboard.frame.png_bytes().is_ok_and(|bytes| bytes == png))
    });
    assert!(
        expected_frame,
        "published PNG was not the compatibility frame"
    );
    assert!(data_dir.join("publisher-state.json").is_file());
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "unavailable");

    let codex_requests = fs::read_to_string(codex_log).unwrap();
    assert_eq!(
        codex_requests.matches(r#""method":"initialize""#).count(),
        1
    );
    assert_eq!(
        codex_requests
            .matches(r#""method":"account/rateLimits/read""#)
            .count(),
        2
    );
    assert_eq!(
        codex_requests.matches(r#""method":"thread/list""#).count(),
        2
    );
    for forbidden in [
        "turn/start",
        "turn/interrupt",
        "thread/archive",
        "review/start",
    ] {
        assert!(!codex_requests.contains(forbidden));
    }
    let diagnostics = [output.stdout, output.stderr].concat();
    for secret in [
        "SECRET_API_KEY",
        "SECRET_DEVICE_ID",
        "SECRET_TASK_TITLE",
        "SECRET_PROMPT",
        "SECRET_PATH",
    ] {
        assert!(!contains(&diagnostics, secret.as_bytes()));
    }
}

#[test]
fn companion_keeps_verified_hook_activity_when_rollout_storage_is_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    let now = current_epoch_seconds();
    let hook = ActivityEvent {
        correlation: CorrelationKey::derive("session-1", "codex-zectrix-dashboard-v1"),
        kind: ActivityEventKind::UserSubmission,
        observed_at_epoch_seconds: now - now.rem_euclid(60),
    };
    fs::write(
        data_dir.join("hook-events.jsonl"),
        format!("{}\n", serde_json::to_string(&hook).unwrap()),
    )
    .unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let started_at = current_epoch_seconds();
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();
    let finished_at = current_epoch_seconds();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = request.recv().unwrap();
    let png = extract_png(&request.1);
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "inferred");

    let expected_frame = (started_at..=finished_at).any(|render_time| {
        let expected = ObservedDashboardState {
            quota: ObservedQuota {
                windows: vec![ObservedQuotaWindow {
                    name: "5 小时".into(),
                    used_percent: 37,
                    resets_at_epoch_seconds: 4_102_444_800,
                }],
                reset_credits: 0,
                stale: false,
            },
            radar: Default::default(),
            task_activity_availability: TaskActivityAvailability::Available,
            task_activity_stale: false,
            tasks: vec![ObservedTask::new(
                "HOOK_TASK",
                ActivityState::Running,
                now - now.rem_euclid(60),
            )],
            prompt: None,
            response: None,
            reasoning: None,
            project_path: None,
            tool: None,
            error_text: None,
            plan: None,
        };
        let normalized = normalize_dashboard(expected, render_time, &DashboardConfig::default());
        render_normalized_dashboard_with_sync(
            normalized,
            render_time,
            DashboardConfig::default(),
            Some(render_time),
        )
        .is_ok_and(|dashboard| dashboard.frame.png_bytes().is_ok_and(|bytes| bytes == png))
    });
    assert!(
        expected_frame,
        "published PNG used an unexpected render time"
    );
}

#[test]
fn companion_treats_only_expired_running_activity_as_a_current_empty_observation() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(codex_home.join("sessions")).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    let hook = ActivityEvent {
        correlation: CorrelationKey::derive("session-1", "codex-zectrix-dashboard-v1"),
        kind: ActivityEventKind::UserSubmission,
        observed_at_epoch_seconds: current_epoch_seconds() - 121,
    };
    fs::write(
        data_dir.join("hook-events.jsonl"),
        format!("{}\n", serde_json::to_string(&hook).unwrap()),
    )
    .unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_HOME", codex_home)
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    request.recv().unwrap();
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "inferred");
    let activity: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("activity.json")).unwrap()).unwrap();
    assert_eq!(activity, serde_json::json!([]));
}

#[test]
fn companion_does_not_treat_an_empty_hook_file_as_verified_activity() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
    )
    .unwrap();
    fs::write(data_dir.join("hook-events.jsonl"), []).unwrap();

    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(output.status.success());
    request.recv().unwrap();
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "unavailable");
}

fn current_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .try_into()
        .unwrap()
}

#[test]
fn companion_reads_keychain_only_once_across_repeated_publish_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    fs::write(
        data_dir.join("activity.json"),
        r#"[{"title":"SECRET_TASK_TITLE","state":"running","activity_at_epoch_seconds":4102444800}]"#,
    )
    .unwrap();
    let keychain_log = temp.path().join("keychain.log");
    let security = fake_missing_security_command(temp.path());
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_ZECTRIX_API_BASE", "http://127.0.0.1:1")
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env("CODEX_ZECTRIX_CODEX_BIN", fake_codex_command(temp.path()))
        .env("CODEX_ZECTRIX_SECURITY_BIN", security)
        .env("TEST_KEYCHAIN_LOG", &keychain_log)
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "3")
        .env("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS", "0")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(fs::read_to_string(keychain_log).unwrap().lines().count(), 1);
}

#[test]
fn companion_reuses_one_app_server_session_across_polling_cycles_and_task_pages() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let codex_log = temp.path().join("persistent-codex.log");
    let keychain_log = temp.path().join("keychain.log");
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", keychain_log)
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "3")
        .env("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS", "0")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = fs::read_to_string(codex_log).unwrap();
    assert_eq!(
        messages.lines().filter(|line| *line == "process").count(),
        1
    );
    assert_eq!(messages.matches(r#""method":"initialize""#).count(), 1);
    assert_eq!(messages.matches(r#""method":"initialized""#).count(), 1);
    assert_eq!(
        messages
            .matches(r#""method":"account/rateLimits/read""#)
            .count(),
        3
    );
    assert_eq!(messages.matches(r#""method":"thread/list""#).count(), 6);
    assert_eq!(messages.matches(r#""cursor":null"#).count(), 3);
    assert_eq!(messages.matches(r#""cursor":"cursor-2""#).count(), 3);
    let request_ids: Vec<u64> = messages
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|message| message.get("id").and_then(serde_json::Value::as_u64))
        .collect();
    assert_eq!(request_ids, (1..=10).collect::<Vec<_>>());
    let diagnostics = [output.stdout, output.stderr].concat();
    assert!(
        !diagnostics
            .windows(b"SECRET_NOTIFICATION".len())
            .any(|bytes| { bytes == b"SECRET_NOTIFICATION" })
    );
}

#[test]
fn companion_observes_task_activity_and_quota_on_independent_deadlines() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let codex_log = temp.path().join("deadline-codex.log");
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "5")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "20")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "70")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = fs::read_to_string(codex_log).unwrap();
    assert_eq!(
        messages.lines().filter(|line| *line == "process").count(),
        1
    );
    assert_eq!(
        messages
            .matches(r#""method":"account/rateLimits/read""#)
            .count(),
        2
    );
    let task_requests = messages.matches(r#""method":"thread/list""#).count();
    assert!(task_requests > 4);
    assert_eq!(task_requests % 2, 0);
}

#[test]
fn companion_coalesces_overdue_task_activity_after_a_delayed_quota_read() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let codex_log = temp.path().join("delayed-codex.log");
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_slow_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "2")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "20")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "1000")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = fs::read_to_string(codex_log).unwrap();
    assert_eq!(
        messages
            .matches(r#""method":"account/rateLimits/read""#)
            .count(),
        1
    );
    assert_eq!(messages.matches(r#""method":"thread/list""#).count(), 4);
}

#[test]
fn companion_keeps_semantically_unchanged_cache_files_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(codex_home.join("sessions")).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let quota_path = data_dir.join("quota.json");
    let activity_path = data_dir.join("activity.json");
    fs::write(
        &quota_path,
        r#"{
            "stale": false,
            "reset_credits": 0,
            "windows": [{
                "resets_at_epoch_seconds": 4102444800,
                "used_percent": 37,
                "name": "5 小时"
            }]
        }"#,
    )
    .unwrap();
    fs::write(&activity_path, "[\n]").unwrap();
    let hook = ActivityEvent {
        correlation: CorrelationKey::derive("session-1", "codex-zectrix-dashboard-v1"),
        kind: ActivityEventKind::UserSubmission,
        observed_at_epoch_seconds: current_epoch_seconds() - 121,
    };
    fs::write(
        data_dir.join("hook-events.jsonl"),
        format!("{}\n", serde_json::to_string(&hook).unwrap()),
    )
    .unwrap();
    let quota_inode = fs::metadata(&quota_path).unwrap().ino();
    let activity_inode = fs::metadata(&activity_path).unwrap().ino();

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_HOME", codex_home)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::metadata(quota_path).unwrap().ino(), quota_inode);
    assert_eq!(fs::metadata(activity_path).unwrap().ino(), activity_inode);
}

#[test]
fn companion_fetches_radar_on_its_own_deadline_without_rewriting_or_duplicate_uploading() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let radar_path = data_dir.join("radar.json");
    fs::write(
        &radar_path,
        "{\n  \"latestReset\": null,\n  \"activeWatch\": null\n}\n",
    )
    .unwrap();
    let radar_inode = fs::metadata(&radar_path).unwrap().ino();
    let (radar_url, radar_requests) = fake_radar_service(3);
    let secret = "SECRET_API_KEY";
    let (zectrix_url, upload_count) = fake_counting_zectrix_service(secret);
    let codex_log = temp.path().join("radar-codex.log");

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_API_BASE", zectrix_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "3")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "100")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "100")
        .env("CODEX_ZECTRIX_RADAR_POLL_INTERVAL_MILLIS", "10")
        .env("CODEX_ZECTRIX_TEST_RADAR_URL", radar_url)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let radar_requests = radar_requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(radar_requests.len(), 3);
    assert!(!radar_requests[0].contains("if-none-match"));
    assert!(radar_requests[1].contains("if-none-match: radar-v1"));
    assert!(radar_requests[2].contains("if-none-match: radar-v1"));
    assert_eq!(fs::metadata(&radar_path).unwrap().ino(), radar_inode);
    assert_eq!(
        upload_count.recv_timeout(Duration::from_secs(12)).unwrap(),
        1
    );
    let messages = fs::read_to_string(codex_log).unwrap();
    assert_eq!(
        messages
            .matches(r#""method":"account/rateLimits/read""#)
            .count(),
        1
    );
    assert_eq!(messages.matches(r#""method":"thread/list""#).count(), 2);
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["radar"], "current");
    assert_eq!(source_status["radarReason"], "none");

    let diagnostics = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .output()
        .unwrap();
    let diagnostics = String::from_utf8(diagnostics.stdout).unwrap();
    assert!(diagnostics.contains("radar_source=current\n"));
    assert!(diagnostics.contains("radar_reason=none\n"));
    for forbidden in [
        "SECRET_PROVIDER_TEXT",
        "SECRET_SOURCE_URL",
        "radar-v1",
        "/api/v1/status",
    ] {
        assert!(!diagnostics.contains(forbidden));
    }
}

#[test]
fn companion_rate_limit_uses_the_later_retry_after_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let responses = vec![
        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 2\r\nContent-Length: 17\r\nConnection: close\r\n\r\nSECRET_RATE_LIMIT",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 15\r\nConnection: close\r\n\r\nSECRET_UPSTREAM",
    ];
    let (radar_url, request_times) = fake_scripted_radar_service(responses);

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "2")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_RADAR_POLL_INTERVAL_MILLIS", "1200")
        .env("CODEX_ZECTRIX_TEST_RADAR_URL", radar_url)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request_times = request_times.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(request_times.len(), 2);
    assert!(
        request_times[1].duration_since(request_times[0]) >= Duration::from_millis(1900),
        "rate-limited radar request retried before Retry-After"
    );
    let diagnostics = [output.stdout, output.stderr].concat();
    for forbidden in ["SECRET_RATE_LIMIT", "SECRET_UPSTREAM", "/api/v1/status"] {
        assert!(!contains(&diagnostics, forbidden.as_bytes()));
    }
}

#[test]
fn companion_reports_the_next_request_time_from_the_completed_failure() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let (radar_url, response_at) = fake_delayed_radar_failure(Duration::from_millis(1200));

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_RADAR_POLL_INTERVAL_MILLIS", "2000")
        .env("CODEX_ZECTRIX_TEST_RADAR_URL", radar_url)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response_at = response_at.recv_timeout(Duration::from_secs(1)).unwrap();
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert!(
        source_status["radarNextRequestAtEpochSeconds"]
            .as_i64()
            .is_some_and(|next_request_at| next_request_at >= response_at + 2)
    );
}

#[test]
fn companion_upstream_failures_wait_without_cache_writes_or_duplicate_uploads() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    let codex_home = temp.path().join("codex-home");
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(codex_home.join("sessions")).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let now = current_epoch_seconds();
    let hook = ActivityEvent {
        correlation: CorrelationKey::derive("session-1", "codex-zectrix-dashboard-v1"),
        kind: ActivityEventKind::UserSubmission,
        observed_at_epoch_seconds: now,
    };
    fs::write(
        data_dir.join("hook-events.jsonl"),
        format!("{}\n", serde_json::to_string(&hook).unwrap()),
    )
    .unwrap();
    let radar_path = data_dir.join("radar.json");
    fs::write(
        &radar_path,
        serde_json::to_vec(&serde_json::json!({
            "latestReset": null,
            "activeWatch": {
                "resetChancePercent": 70,
                "forecastWindow": "SECRET_WINDOW",
                "observedAtEpochSeconds": now - 60,
                "expiresAtEpochSeconds": now + 60 * 60,
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let radar_before = fs::read(&radar_path).unwrap();
    let radar_inode = fs::metadata(&radar_path).unwrap().ino();
    let responses = vec![
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 16\r\nConnection: close\r\n\r\nSECRET_FAILURE_1",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 16\r\nConnection: close\r\n\r\nSECRET_FAILURE_2",
    ];
    let (radar_url, request_times) = fake_scripted_radar_service(responses);
    let secret = "SECRET_API_KEY";
    let (zectrix_url, upload_count) = fake_counting_zectrix_service(secret);
    let codex_log = temp.path().join("codex.log");

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_HOME", codex_home)
        .env("CODEX_ZECTRIX_API_BASE", zectrix_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_task_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "2")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_RADAR_POLL_INTERVAL_MILLIS", "250")
        .env("CODEX_ZECTRIX_TEST_RADAR_URL", radar_url)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request_times = request_times.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(request_times.len(), 2);
    assert!(
        request_times[1].duration_since(request_times[0]) >= Duration::from_millis(200),
        "upstream failure triggered a tight radar retry"
    );
    assert_eq!(fs::metadata(&radar_path).unwrap().ino(), radar_inode);
    assert_eq!(fs::read(&radar_path).unwrap(), radar_before);
    assert_eq!(
        upload_count.recv_timeout(Duration::from_secs(12)).unwrap(),
        1
    );
    let codex_messages = fs::read_to_string(codex_log).unwrap();
    assert_eq!(
        codex_messages
            .matches(r#""method":"account/rateLimits/read""#)
            .count(),
        1
    );
    assert_eq!(
        codex_messages.matches(r#""method":"initialize""#).count(),
        1
    );
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "current");
    assert_eq!(source_status["taskActivity"], "inferred");
    assert_eq!(source_status["radar"], "stale");
    assert_eq!(source_status["radarReason"], "upstream_unavailable");
    assert!(source_status["radarNextRequestAtEpochSeconds"].is_number());
    let diagnostics = [output.stdout, output.stderr].concat();
    for forbidden in ["SECRET_FAILURE_1", "SECRET_FAILURE_2", "SECRET_WINDOW"] {
        assert!(!contains(&diagnostics, forbidden.as_bytes()));
    }
}

#[test]
fn companion_diagnostics_become_unavailable_when_stale_radar_evidence_expires() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let now = current_epoch_seconds();
    fs::write(
        data_dir.join("radar.json"),
        serde_json::to_vec(&serde_json::json!({
            "latestReset": null,
            "activeWatch": {
                "resetChancePercent": 70,
                "forecastWindow": "soon",
                "observedAtEpochSeconds": now - 60,
                "expiresAtEpochSeconds": now + 3,
            },
        }))
        .unwrap(),
    )
    .unwrap();
    let (radar_url, request_times) = fake_scripted_radar_service(vec![
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ]);

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_persistent_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
        .env("CODEX_ZECTRIX_MAX_CYCLES", "2")
        .env("CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS", "3500")
        .env("CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS", "3500")
        .env("CODEX_ZECTRIX_RADAR_POLL_INTERVAL_MILLIS", "5000")
        .env("CODEX_ZECTRIX_TEST_RADAR_URL", radar_url)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        request_times
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .len(),
        1
    );
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["radar"], "unavailable");
    assert_eq!(source_status["radarReason"], "upstream_unavailable");
}

#[test]
fn offline_restart_recovers_only_intrinsically_eligible_persisted_radar_evidence() {
    let now = current_epoch_seconds();
    let cases = [
        (
            "confirmation",
            serde_json::json!({
                "latestReset": {
                    "kind": "regular",
                    "announcedAtEpochSeconds": now - 60 * 60,
                },
                "activeWatch": null,
            }),
            RadarDisplayState::Confirmed {
                kind: ResetKind::Regular,
                stale: true,
            },
            "stale",
        ),
        (
            "watch",
            serde_json::json!({
                "latestReset": null,
                "activeWatch": {
                    "resetChancePercent": 70,
                    "forecastWindow": "today 12:00-15:00 UTC",
                    "observedAtEpochSeconds": now - 60 * 60,
                    "expiresAtEpochSeconds": now + 2 * 60 * 60,
                },
            }),
            RadarDisplayState::ActiveWatch {
                forecast_window: "today 12:00-15:00 UTC".into(),
                reset_chance_percent: Some(70),
                stale: true,
            },
            "stale",
        ),
        (
            "no_watch",
            serde_json::json!({
                "latestReset": null,
                "activeWatch": null,
            }),
            RadarDisplayState::Unavailable,
            "unavailable",
        ),
    ];

    for (case, persisted_radar, expected_radar, expected_source) in cases {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(
            data_dir.join("settings.json"),
            r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":false}"#,
        )
        .unwrap();
        let radar_path = data_dir.join("radar.json");
        fs::write(&radar_path, serde_json::to_vec(&persisted_radar).unwrap()).unwrap();
        let radar_before = fs::read(&radar_path).unwrap();
        let secret = "SECRET_API_KEY";
        let (zectrix_url, request) = fake_zectrix_service(secret);
        let started_at = current_epoch_seconds();

        let output = Command::new(common::dashboard_binary())
            .arg("companion")
            .env("CODEX_ZECTRIX_API_BASE", zectrix_url)
            .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
            .env("CODEX_ZECTRIX_CODEX_BIN", fake_codex_command(temp.path()))
            .env(
                "CODEX_ZECTRIX_SECURITY_BIN",
                fake_security_command(temp.path()),
            )
            .env("TEST_KEYCHAIN_SECRET", secret)
            .env("TEST_CODEX_LOG", temp.path().join("codex.log"))
            .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
            .output()
            .unwrap();
        let finished_at = current_epoch_seconds();

        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let request = request.recv().unwrap();
        let png = extract_png(&request.1);
        let expected_frame = (started_at..=finished_at).any(|render_time| {
            let observed = ObservedDashboardState {
                quota: ObservedQuota {
                    windows: vec![ObservedQuotaWindow {
                        name: "5 小时".into(),
                        used_percent: 37,
                        resets_at_epoch_seconds: 4_102_444_800,
                    }],
                    reset_credits: 0,
                    stale: false,
                },
                radar: expected_radar.clone(),
                task_activity_availability: TaskActivityAvailability::Unavailable,
                task_activity_stale: false,
                tasks: vec![],
                prompt: None,
                response: None,
                reasoning: None,
                project_path: None,
                tool: None,
                error_text: None,
                plan: None,
            };
            let normalized =
                normalize_dashboard(observed, render_time, &DashboardConfig::default());
            render_normalized_dashboard_with_sync(
                normalized,
                render_time,
                DashboardConfig::default(),
                Some(render_time),
            )
            .is_ok_and(|dashboard| dashboard.frame.png_bytes().is_ok_and(|bytes| bytes == png))
        });
        assert!(expected_frame, "{case}: unexpected published radar frame");
        assert_eq!(fs::read(&radar_path).unwrap(), radar_before, "{case}");
        let source_status: serde_json::Value =
            serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap())
                .unwrap();
        assert_eq!(source_status["radar"], expected_source, "{case}");
        assert_eq!(source_status["radarReason"], "network", "{case}");
    }
}

#[test]
fn companion_transport_failures_cannot_start_an_immediate_restart_loop() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let cached_quota = r#"{"windows":[{"name":"5 小时","used_percent":37,"resets_at_epoch_seconds":4102444800}],"reset_credits":0,"stale":false}"#;
    let cached_activity =
        r#"[{"title":"LAST_KNOWN_TASK","state":"running","activity_at_epoch_seconds":4102444800}]"#;
    fs::write(data_dir.join("quota.json"), cached_quota).unwrap();
    fs::write(data_dir.join("activity.json"), cached_activity).unwrap();
    let codex_log = temp.path().join("failing-codex.log");
    let secret = "SECRET_API_KEY";
    let (base_url, request) = fake_zectrix_service(secret);
    let started_at = current_epoch_seconds();

    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_ZECTRIX_API_BASE", base_url)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_failing_codex_command(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_SECRET", secret)
        .env("TEST_CODEX_LOG", &codex_log)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "20")
        .env("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS", "0")
        .output()
        .unwrap();
    let finished_at = current_epoch_seconds();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(codex_log)
            .unwrap()
            .lines()
            .filter(|line| *line == "process")
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(data_dir.join("quota.json")).unwrap(),
        cached_quota
    );
    assert_eq!(
        fs::read_to_string(data_dir.join("activity.json")).unwrap(),
        cached_activity
    );
    let source_status: serde_json::Value =
        serde_json::from_slice(&fs::read(data_dir.join("source-status.json")).unwrap()).unwrap();
    assert_eq!(source_status["quota"], "stale");
    assert_eq!(source_status["taskActivity"], "stale");
    let request = request.recv().unwrap();
    let png = extract_png(&request.1);
    let expected_frame = (started_at..=finished_at).any(|render_time| {
        let observed = ObservedDashboardState {
            quota: ObservedQuota {
                windows: vec![ObservedQuotaWindow {
                    name: "5 小时".into(),
                    used_percent: 37,
                    resets_at_epoch_seconds: 4_102_444_800,
                }],
                reset_credits: 0,
                stale: true,
            },
            radar: Default::default(),
            task_activity_availability: TaskActivityAvailability::Available,
            task_activity_stale: true,
            tasks: vec![ObservedTask::new(
                "LAST_KNOWN_TASK",
                ActivityState::Running,
                4_102_444_800,
            )],
            prompt: None,
            response: None,
            reasoning: None,
            project_path: None,
            tool: None,
            error_text: None,
            plan: None,
        };
        let config = DashboardConfig {
            privacy_mode: true,
            previous_frame_hash: None,
        };
        let normalized = normalize_dashboard(observed, render_time, &config);
        render_normalized_dashboard_with_sync(normalized, render_time, config, Some(render_time))
            .is_ok_and(|dashboard| dashboard.frame.png_bytes().is_ok_and(|bytes| bytes == png))
    });
    assert!(
        expected_frame,
        "published PNG did not retain stale task state"
    );
}

#[test]
fn companion_shutdown_reaps_the_app_server_process_tree() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let sentinel_pid = temp.path().join("sentinel.pid");
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_sentinel_descendant(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_SENTINEL_PID", &sentinel_pid)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pid = fs::read_to_string(sentinel_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(wait_until_process_exits(pid, Duration::from_secs(1)));
}

#[test]
fn companion_shutdown_reaps_a_descendant_in_an_independent_session() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let sentinel_pid = temp.path().join("escaped-sentinel.pid");
    let output = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_escaped_sentinel_descendant(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_SENTINEL_PID", &sentinel_pid)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pid = fs::read_to_string(sentinel_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let descendant_exited = wait_until_process_exits(pid, Duration::from_secs(1));
    if !descendant_exited {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        assert!(wait_until_process_exits(pid, Duration::from_secs(1)));
    }
    assert!(descendant_exited);
}

#[test]
fn companion_transport_failure_forcibly_reaps_a_hung_process_tree() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let sentinel_pid = temp.path().join("hung-sentinel.pid");
    let transport_failed = temp.path().join("transport-failed");
    let mut companion = Command::new(common::dashboard_binary())
        .arg("companion")
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_hung_codex_with_sentinel_descendant(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_SENTINEL_PID", &sentinel_pid)
        .env("TEST_TRANSPORT_FAILURE_MARKER", &transport_failed)
        .env("CODEX_ZECTRIX_MAX_CYCLES", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    assert!(wait_until_file_exists(
        &transport_failed,
        PROCESS_START_TIMEOUT
    ));
    let status = wait_for_child(&mut companion, PROCESS_CLEANUP_TIMEOUT).unwrap();
    assert!(status.success());
    let pid = fs::read_to_string(sentinel_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(wait_until_process_exits(pid, Duration::from_secs(1)));
}

#[test]
fn companion_sigterm_reaps_the_app_server_process_tree() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("settings.json"),
        r#"{"deviceId":"SECRET_DEVICE_ID","pageId":3,"privacyMode":true}"#,
    )
    .unwrap();
    let sentinel_pid = temp.path().join("sigterm-sentinel.pid");
    let mut companion = Command::new(common::dashboard_binary())
        .arg("companion")
        .env(
            "CODEX_HOME",
            temp.path().join("codex-home-without-rollouts"),
        )
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            fake_codex_with_sentinel_descendant(temp.path()),
        )
        .env(
            "CODEX_ZECTRIX_SECURITY_BIN",
            fake_missing_security_command(temp.path()),
        )
        .env("TEST_KEYCHAIN_LOG", temp.path().join("keychain.log"))
        .env("TEST_SENTINEL_PID", &sentinel_pid)
        .env("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS", "10")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    if !wait_until_file_exists(&sentinel_pid, PROCESS_START_TIMEOUT) {
        let _ = companion.kill();
        let status = companion.wait().unwrap();
        panic!("sentinel descendant did not start; companion status: {status}");
    }

    unsafe {
        libc::kill(companion.id().try_into().unwrap(), libc::SIGTERM);
    }
    let status = wait_for_child(&mut companion, PROCESS_CLEANUP_TIMEOUT).unwrap();
    let pid = fs::read_to_string(sentinel_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let descendant_exited = wait_until_process_exits(pid, Duration::from_secs(1));
    if !descendant_exited {
        unsafe {
            libc::kill(-libc::getpgid(pid), libc::SIGKILL);
        }
    }

    assert!(status.success());
    assert!(descendant_exited);
}

fn fake_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> "$TEST_CODEX_LOG"
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/0.146.1 (test)","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
printf '%s\n' "$initialized" >> "$TEST_CODEX_LOG"
while read -r request; do
  printf '%s\n' "$request" >> "$TEST_CODEX_LOG"
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *account/rateLimits/read*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}" ;;
    *) printf '%s\n' "{\"id\":$request_id,\"error\":{\"code\":-1,\"message\":\"SECRET_PROMPT SECRET_PATH\"}}" ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_codex_with_task_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex-with-task");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' "$initialize" >> "$TEST_CODEX_LOG"
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
printf '%s\n' "$initialized" >> "$TEST_CODEX_LOG"
while read -r request; do
  printf '%s\n' "$request" >> "$TEST_CODEX_LOG"
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *account/rateLimits/read*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}" ;;
    *thread/list*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[{\"id\":\"task-1\",\"sessionId\":\"session-1\",\"name\":\"HOOK_TASK\",\"parentThreadId\":null,\"source\":\"appServer\"}],\"nextCursor\":null}}" ;;
    *) printf '%s\n' "{\"id\":$request_id,\"error\":{\"code\":-1,\"message\":\"unsupported\"}}" ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_persistent_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("persistent-codex");
    fs::write(
        &path,
        r#"#!/bin/sh
printf '%s\n' process >> "$TEST_CODEX_LOG"
read -r initialize
printf '%s\n' "$initialize" >> "$TEST_CODEX_LOG"
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
printf '%s\n' "$initialized" >> "$TEST_CODEX_LOG"
while read -r request; do
  printf '%s\n' "$request" >> "$TEST_CODEX_LOG"
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '%s\n' '{"method":"account/updated","params":{"detail":"SECRET_NOTIFICATION"}}'
  case "$request" in
    *account/rateLimits/read*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
      ;;
    *'"cursor":null'*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":\"cursor-2\"}}"
      ;;
    *'"cursor":"cursor-2"'*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":null}}"
      ;;
    *)
      printf '%s\n' "{\"id\":$request_id,\"error\":{\"code\":-1,\"message\":\"unsupported\"}}"
      ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_slow_persistent_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("slow-persistent-codex");
    fs::write(
        &path,
        r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
while read -r request; do
  printf '%s\n' "$request" >> "$TEST_CODEX_LOG"
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *account/rateLimits/read*)
      sleep 0.12
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
      ;;
    *'"cursor":null'*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":\"cursor-2\"}}"
      ;;
    *'"cursor":"cursor-2"'*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":null}}"
      ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_failing_codex_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("failing-codex");
    fs::write(
        &path,
        r#"#!/bin/sh
printf '%s\n' process >> "$TEST_CODEX_LOG"
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
read -r request
printf '%s\n' 'not-json'
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_codex_with_sentinel_descendant(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex-with-sentinel");
    fs::write(
        &path,
        r#"#!/bin/sh
sh -c 'trap "exit 0" TERM; printf "%s\n" "$$" > "$TEST_SENTINEL_PID"; while :; do sleep 1; done' &
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
while read -r request; do
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *account/rateLimits/read*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
      ;;
    *thread/list*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":null}}" ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_hung_codex_with_sentinel_descendant(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("hung-codex-with-sentinel");
    fs::write(
        &path,
        r#"#!/bin/sh
trap '' TERM
sh -c 'trap "" TERM; printf "%s\n" "$$" > "$TEST_SENTINEL_PID"; while :; do sleep 1; done' &
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
read -r request
printf 'failed\n' > "$TEST_TRANSPORT_FAILURE_MARKER"
printf '%s\n' 'not-json'
while :; do sleep 1; done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_codex_with_escaped_sentinel_descendant(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("codex-with-escaped-sentinel");
    fs::write(
        &path,
        r#"#!/bin/sh
/usr/bin/perl -MPOSIX -e 'POSIX::setsid(); open my $pid_file, ">", $ENV{TEST_SENTINEL_PID}; print {$pid_file} "$$\n"; close $pid_file; $SIG{TERM} = sub { exit 0 }; sleep 1 while 1' </dev/null >/dev/null 2>&1 &
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/future (test)"}}'
read -r initialized
while read -r request; do
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *account/rateLimits/read*)
      printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}"
      ;;
    *thread/list*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[],\"nextCursor\":null}}" ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn wait_until_process_exits(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_until_file_exists(path: &std::path::Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

fn wait_for_child(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn fake_security_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("security");
    fs::write(
        &path,
        "#!/bin/sh\n[ \"$1\" = find-generic-password ] || exit 2\nprintf '%s' \"$TEST_KEYCHAIN_SECRET\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_missing_security_command(temp: &std::path::Path) -> std::path::PathBuf {
    let path = temp.join("missing-security");
    fs::write(
        &path,
        "#!/bin/sh\nprintf 'read\\n' >> \"$TEST_KEYCHAIN_LOG\"\nexit 44\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn fake_zectrix_service(secret: &str) -> (String, mpsc::Receiver<(String, Vec<u8>)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let expected_secret = secret.to_ascii_lowercase();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        assert!(request.0.contains(&format!("x-api-key: {expected_secret}")));
        let body = r#"{"code":0}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
        sender.send(request).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn fake_radar_service(request_count: usize) -> (String, mpsc::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..request_count {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request_head(&mut stream);
            requests.push(request);
            if index == 0 {
                let body = r#"{"data":{"latest_reset":null,"active_watch":null,"stats":{"text":"SECRET_PROVIDER_TEXT","source":"SECRET_SOURCE_URL"}},"meta":{"generated_at":"SECRET_GENERATED"}}"#;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nETag: radar-v1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
            } else {
                write!(
                    stream,
                    "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}/api/v1/status"), receiver)
}

fn fake_scripted_radar_service(
    responses: Vec<&'static str>,
) -> (String, mpsc::Receiver<Vec<Instant>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut request_times = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().unwrap();
            request_times.push(Instant::now());
            let _ = read_request_head(&mut stream);
            stream.write_all(response.as_bytes()).unwrap();
        }
        sender.send(request_times).unwrap();
    });
    (format!("http://{address}/api/v1/status"), receiver)
}

fn fake_delayed_radar_failure(delay: Duration) -> (String, mpsc::Receiver<i64>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = read_request_head(&mut stream);
        thread::sleep(delay);
        stream
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        sender.send(current_epoch_seconds()).unwrap();
    });
    (format!("http://{address}/api/v1/status"), receiver)
}

fn fake_counting_zectrix_service(secret: &str) -> (String, mpsc::Receiver<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let expected_secret = secret.to_ascii_lowercase();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut deadline = Instant::now() + Duration::from_secs(10);
        let mut count = 0;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request = read_request(&mut stream);
                    assert!(request.0.contains(&format!("x-api-key: {expected_secret}")));
                    count += 1;
                    deadline = Instant::now() + Duration::from_millis(300);
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{{\"code\":0}}"
                    )
                    .unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        }
        sender.send(count).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn read_request(stream: &mut impl Read) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec())
        .unwrap()
        .to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap()
        .trim()
        .parse::<usize>()
        .unwrap();
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    }
    (
        head,
        bytes[header_end..header_end + content_length].to_vec(),
    )
}

fn read_request_head(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes).unwrap().to_ascii_lowercase();
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn extract_png(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .windows(8)
        .position(|window| window == b"\x89PNG\r\n\x1a\n")
        .unwrap();
    let iend = bytes[start..]
        .windows(4)
        .position(|window| window == b"IEND")
        .unwrap()
        + start;
    &bytes[start..iend + 8]
}
}
