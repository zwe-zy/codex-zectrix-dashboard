use std::collections::VecDeque;
use std::time::Duration;

use codex_zectrix_dashboard::companion::{
    CompanionScheduler, DiagnosticEvent, DiagnosticLog, SourceErrorCategory, StatusSource,
};
use codex_zectrix_dashboard::freshness::{FreshValue, QUOTA_STALE_AFTER, TASK_STALE_AFTER};
use codex_zectrix_dashboard::status_board::BoardTaskState;
use codex_zectrix_dashboard::{QuotaStatus, TaskState};
use tempfile::tempdir;

#[cfg(feature = "test-support")]
use codex_zectrix_dashboard::{AppServerClient, CodexStatusSource};

#[derive(Default)]
struct FakeSource {
    task_results: VecDeque<Result<TaskState, SourceErrorCategory>>,
    quota_results: VecDeque<Result<QuotaStatus, SourceErrorCategory>>,
    task_reads: usize,
    quota_reads: usize,
}

impl StatusSource for FakeSource {
    fn read_task_state(&mut self) -> Result<TaskState, SourceErrorCategory> {
        self.task_reads += 1;
        self.task_results
            .pop_front()
            .unwrap_or(Err(SourceErrorCategory::Unavailable))
    }

    fn read_quota(&mut self) -> Result<QuotaStatus, SourceErrorCategory> {
        self.quota_reads += 1;
        self.quota_results
            .pop_front()
            .unwrap_or(Err(SourceErrorCategory::Unavailable))
    }
}

fn quota(percent: u8, reset: i64) -> QuotaStatus {
    QuotaStatus::single(percent as f64, Some(reset), Some(300))
}
#[test]
fn explicit_absent_windows_clear_cached_values() {
    let both = codex_zectrix_dashboard::parse_quota_status(
        r#"{"rateLimits":{"primary":{"usedPercent":20},"secondary":{"usedPercent":40}}}"#,
    )
    .unwrap();
    let single = codex_zectrix_dashboard::parse_quota_status(
        r#"{"rateLimits":{"primary":{"usedPercent":25},"secondary":null}}"#,
    )
    .unwrap();
    let mut source = FakeSource {
        quota_results: VecDeque::from([Ok(both), Ok(single)]),
        ..Default::default()
    };
    let mut scheduler = CompanionScheduler::new();
    scheduler.poll_due(Duration::ZERO, 1000, &mut source);
    let state = scheduler
        .poll_due(Duration::from_secs(600), 1600, &mut source)
        .state;
    assert!(state.quota_secondary.is_none());
    assert_eq!(state.quota_remaining_percent, Some(75.0));
}

#[test]
fn both_explicitly_absent_windows_clear_the_previous_account_quota() {
    let absent = codex_zectrix_dashboard::parse_quota_status(
        r#"{"rateLimitsByLimitId":{"codex":{"primary":null,"secondary":null}}}"#,
    )
    .unwrap();
    let mut source = FakeSource {
        quota_results: VecDeque::from([Ok(quota(80, 9000)), Ok(absent)]),
        ..Default::default()
    };
    let mut scheduler = CompanionScheduler::new();
    scheduler.poll_due(Duration::ZERO, 1000, &mut source);
    let state = scheduler
        .poll_due(Duration::from_secs(600), 1600, &mut source)
        .state;
    assert!(state.quota_remaining_percent.is_none() && state.quota_secondary.is_none());
}

#[test]
fn a_backward_clock_marks_cached_data_stale() {
    let value = FreshValue::with_success(70, 1000);
    assert!(value.is_stale(900, QUOTA_STALE_AFTER));
}

#[test]
fn quota_windows_age_independently_when_one_window_is_invalid() {
    let both = codex_zectrix_dashboard::parse_quota_status(
        r#"{"rateLimits":{"primary":{"usedPercent":20},"secondary":{"usedPercent":40}}}"#,
    )
    .unwrap();
    let secondary = codex_zectrix_dashboard::parse_quota_status(
        r#"{"rateLimits":{"primary":{"usedPercent":-1},"secondary":{"usedPercent":50}}}"#,
    )
    .unwrap();
    let mut source = FakeSource {
        quota_results: VecDeque::from([Ok(both), Ok(secondary), Ok(secondary), Ok(secondary)]),
        ..Default::default()
    };
    let mut scheduler = CompanionScheduler::new();
    scheduler.poll_due(Duration::ZERO, 1000, &mut source);
    scheduler.poll_due(Duration::from_secs(600), 1600, &mut source);
    scheduler.poll_due(Duration::from_secs(1200), 2200, &mut source);
    let state = scheduler
        .poll_due(Duration::from_secs(1800), 2800, &mut source)
        .state;
    assert_eq!(state.quota_remaining_percent, Some(80.0));
    assert!(state.quota_stale);
    let weekly = state.quota_secondary.unwrap();
    assert_eq!(weekly.remaining_percent, 50.0);
    assert!(!weekly.stale);
}

#[test]
fn polls_both_sources_immediately_then_at_independent_rates_without_catch_up_bursts() {
    // Catches advancing a deadline from its old value, which would burst after a long pause.
    let mut source = FakeSource {
        task_results: VecDeque::from([
            Ok(TaskState::Running),
            Ok(TaskState::Completed),
            Ok(TaskState::Failed),
        ]),
        quota_results: VecDeque::from([Ok(quota(80, 9_000)), Ok(quota(70, 10_000))]),
        ..FakeSource::default()
    };
    let mut scheduler = CompanionScheduler::new();

    let first = scheduler.poll_due(Duration::ZERO, 1_000, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (1, 1));
    assert_eq!(first.state.task_state, BoardTaskState::Running);
    assert_eq!(first.state.quota_remaining_percent, Some(80.0));
    assert!(first.task_polled && first.quota_polled);
    assert!(first.task_succeeded && first.quota_succeeded);

    let early = scheduler.poll_due(Duration::from_secs(59), 1_059, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (1, 1));
    assert!(!early.task_polled && !early.quota_polled);
    assert!(!early.task_succeeded && !early.quota_succeeded);

    scheduler.poll_due(Duration::from_secs(60), 1_060, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (2, 1));
    scheduler.poll_due(Duration::from_secs(599), 1_599, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (3, 1));
    let quota_tick = scheduler.poll_due(Duration::from_secs(600), 1_600, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (3, 2));
    assert_eq!(quota_tick.state.task_state, BoardTaskState::Failed);
    assert_eq!(quota_tick.state.quota_remaining_percent, Some(70.0));

    scheduler.poll_due(Duration::from_secs(5_000), 6_000, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (4, 3));
    scheduler.poll_due(Duration::from_secs(5_000), 6_000, &mut source);
    assert_eq!((source.task_reads, source.quota_reads), (4, 3));
}

#[test]
fn failed_refresh_preserves_value_and_staleness_begins_at_exact_threshold() {
    // Catches clearing cached data on error or using > instead of >= at the threshold.
    let task = FreshValue::with_success(TaskState::Running, 100);
    let quota_value = FreshValue::with_success(quota(42, 5_000), 100);
    assert!(!task.is_stale(279, TASK_STALE_AFTER));
    assert!(task.is_stale(280, TASK_STALE_AFTER));
    assert!(!quota_value.is_stale(1_899, QUOTA_STALE_AFTER));
    assert!(quota_value.is_stale(1_900, QUOTA_STALE_AFTER));
    assert_eq!(task.value, Some(TaskState::Running));
    assert_eq!(quota_value.value, Some(quota(42, 5_000)));

    // A backward wall-clock observation must not make fresh data stale.
    assert!(task.is_stale(i64::MIN, TASK_STALE_AFTER));
}

#[test]
fn initial_failures_render_unavailable_task_and_missing_quota_without_fabrication() {
    // Catches substituting Empty/0% for data that was never observed successfully.
    let mut scheduler = CompanionScheduler::new();
    let mut source = FakeSource::default();
    let report = scheduler.poll_due(Duration::ZERO, i64::MAX, &mut source);

    assert_eq!(report.state.task_state, BoardTaskState::Unavailable);
    assert_eq!(report.state.quota_remaining_percent, None);
    assert_eq!(report.state.quota_resets_at_epoch_seconds, None);
    assert!(!report.state.task_stale);
    assert!(!report.state.quota_stale);
    assert_eq!(
        report.source_errors,
        vec![
            SourceErrorCategory::Unavailable,
            SourceErrorCategory::Unavailable
        ]
    );
}

#[test]
fn source_failures_keep_last_success_and_mark_each_cache_stale_at_its_own_threshold() {
    // Catches replacing a successful value on error or sharing one freshness deadline.
    let mut source = FakeSource {
        task_results: VecDeque::from([
            Ok(TaskState::Running),
            Err(SourceErrorCategory::Unavailable),
            Err(SourceErrorCategory::Unavailable),
        ]),
        quota_results: VecDeque::from([
            Ok(quota(64, 9_000)),
            Err(SourceErrorCategory::Unavailable),
            Err(SourceErrorCategory::Unavailable),
        ]),
        ..FakeSource::default()
    };
    let mut scheduler = CompanionScheduler::new();
    scheduler.poll_due(Duration::ZERO, 1_000, &mut source);

    let before_task_stale = scheduler.poll_due(Duration::from_secs(179), 1_179, &mut source);
    assert_eq!(before_task_stale.state.task_state, BoardTaskState::Running);
    assert!(!before_task_stale.state.task_stale);
    assert_eq!(before_task_stale.state.quota_remaining_percent, Some(64.0));

    let task_stale = scheduler.poll_due(Duration::from_secs(180), 1_180, &mut source);
    assert_eq!(task_stale.state.task_state, BoardTaskState::Running);
    assert!(task_stale.state.task_stale);
    assert!(!task_stale.state.quota_stale);

    let quota_stale = scheduler.poll_due(Duration::from_secs(1_800), 2_800, &mut source);
    assert_eq!(quota_stale.state.quota_remaining_percent, Some(64.0));
    assert!(quota_stale.state.quota_stale);
}

#[cfg(feature = "test-support")]
#[test]
fn real_codex_adapter_protocol_error_preserves_last_task_until_exact_stale_threshold() {
    // Catches CodexStatusSource turning incompatible payloads into a successful Unavailable value.
    let mut scheduler = CompanionScheduler::new();
    let mut seed = FakeSource {
        task_results: VecDeque::from([Ok(TaskState::Running)]),
        quota_results: VecDeque::from([Ok(quota(64, 9_000))]),
        ..FakeSource::default()
    };
    scheduler.poll_due(Duration::ZERO, 1_000, &mut seed);

    let temp = tempfile::tempdir().unwrap();
    let program = temp.path().join(format!(
        "status-malformed-list{}",
        std::env::consts::EXE_SUFFIX
    ));
    std::fs::copy(
        env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"),
        &program,
    )
    .unwrap();
    let mut source = CodexStatusSource::new(AppServerClient::new(program));

    let before_stale = scheduler.poll_due(Duration::from_secs(60), 1_060, &mut source);
    assert_eq!(before_stale.state.task_state, BoardTaskState::Running);
    assert!(!before_stale.state.task_stale);
    assert_eq!(
        before_stale.source_errors,
        vec![SourceErrorCategory::Incompatible]
    );

    let stale = scheduler.poll_due(Duration::from_secs(180), 1_180, &mut source);
    assert_eq!(stale.state.task_state, BoardTaskState::Running);
    assert!(stale.state.task_stale);
    assert_eq!(stale.source_errors, vec![SourceErrorCategory::Incompatible]);
}

#[test]
fn bounded_diagnostics_rotate_and_discard_untrusted_details() {
    // Catches accidentally formatting source/cloud strings into persistent diagnostics.
    let directory = tempdir().unwrap();
    let sentinel = "secret-key raw-json thread-title C:\\private\\project";
    let mut log = DiagnosticLog::with_max_bytes(directory.path(), 80).unwrap();
    for _ in 0..20 {
        log.record(DiagnosticEvent::SourceUnavailable, sentinel)
            .unwrap();
    }
    log.record(DiagnosticEvent::CredentialsRejected, sentinel)
        .unwrap();
    log.record(
        DiagnosticEvent::from_source_error(SourceErrorCategory::Incompatible),
        sentinel,
    )
    .unwrap();

    let current = std::fs::read_to_string(directory.path().join("companion.log")).unwrap();
    let rotated = std::fs::read_to_string(directory.path().join("companion.log.1")).unwrap();
    assert!(!current.contains(sentinel));
    assert!(!rotated.contains(sentinel));
    assert!(current.contains("credentials_rejected"));
    assert!(current.contains("source_incompatible"));
    assert!(
        std::fs::metadata(directory.path().join("companion.log"))
            .unwrap()
            .len()
            <= 80
    );
    assert!(
        std::fs::metadata(directory.path().join("companion.log.1"))
            .unwrap()
            .len()
            <= 80
    );
}

#[test]
fn hidden_companion_without_setup_exits_quickly_with_only_a_fixed_diagnostic() {
    // Catches a startup panic, credential access before settings, or accidental secret output.
    let directory = tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-zectrix-companion"))
        .env("CODEX_ZECTRIX_DATA_DIR", directory.path())
        .env("SECRET_SENTINEL", "never-print-this")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let diagnostics = std::fs::read_to_string(directory.path().join("companion.log")).unwrap();
    assert_eq!(diagnostics, "setup_required\n");
    assert!(!diagnostics.contains("never-print-this"));
}

#[test]
fn corrupt_publisher_state_fails_closed_before_setup_and_preserves_original_bytes() {
    // Catches silently defaulting and later overwriting a state file that failed JSON decoding.
    let directory = tempdir().unwrap();
    let state_path = directory.path().join("status-frame-state.json");
    let original = b"{corrupt-json SECRET_SENTINEL";
    std::fs::write(&state_path, original).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-zectrix-companion"))
        .env("CODEX_ZECTRIX_DATA_DIR", directory.path())
        .env("SECRET_SENTINEL", "never-print-this")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read(&state_path).unwrap(), original);
    let diagnostics = std::fs::read_to_string(directory.path().join("companion.log")).unwrap();
    assert_eq!(diagnostics, "invalid_configuration\n");
    assert!(!diagnostics.contains("corrupt-json"));
    assert!(!diagnostics.contains("SECRET_SENTINEL"));
}

#[test]
fn corrupt_runtime_status_fails_closed_and_preserves_original_bytes() {
    // Catches silently defaulting and overwriting privacy-safe diagnostic timestamps.
    let directory = tempdir().unwrap();
    let status_path = directory.path().join("runtime-status.json");
    let original = b"{corrupt-runtime SECRET_SENTINEL";
    std::fs::write(&status_path, original).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-zectrix-companion"))
        .env("CODEX_ZECTRIX_DATA_DIR", directory.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert_eq!(std::fs::read(&status_path).unwrap(), original);
    let diagnostics = std::fs::read_to_string(directory.path().join("companion.log")).unwrap();
    assert_eq!(diagnostics, "invalid_configuration\n");
    assert!(!diagnostics.contains("SECRET_SENTINEL"));
}

#[test]
fn unreadable_publisher_state_path_fails_closed_without_replacing_it() {
    // Catches treating a state read failure as no prior state and writing over that path.
    let directory = tempdir().unwrap();
    let state_path = directory.path().join("status-frame-state.json");
    std::fs::create_dir(&state_path).unwrap();
    let marker_path = state_path.join("preserve.bin");
    let original = b"preserve-state-sentinel";
    std::fs::write(&marker_path, original).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-zectrix-companion"))
        .env("CODEX_ZECTRIX_DATA_DIR", directory.path())
        .env("SECRET_SENTINEL", "never-print-this")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    assert!(state_path.is_dir());
    assert_eq!(std::fs::read(&marker_path).unwrap(), original);
    assert_eq!(std::fs::read_dir(&state_path).unwrap().count(), 1);
    let diagnostics = std::fs::read_to_string(directory.path().join("companion.log")).unwrap();
    assert_eq!(diagnostics, "state_persist_unavailable\n");
    assert!(!diagnostics.contains("preserve-state-sentinel"));
}
