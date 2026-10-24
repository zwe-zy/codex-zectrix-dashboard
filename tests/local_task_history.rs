#![cfg(feature = "test-support")]
use codex_zectrix_dashboard::{TaskState, local_task_history::LocalTaskHistory};
use rusqlite::{Connection, params};
use std::process::Command;

const LOGICAL: &str = "10000000-0000-7000-8000-000000000001";
const EXECUTION: &str = "20000000-0000-7000-8000-000000000002";

#[test]
fn unavailable_history_never_reads_the_superseded_private_server_turn() {
    for (newer, malformed) in [(false, false), (true, false), (false, true)] {
        let home = tempfile::tempdir().unwrap();
        if newer {
            drop(fixture(home.path(), "interrupted", 10000));
            std::fs::write(
                home.path().join("thread_history_2.sqlite"),
                b"future schema",
            )
            .unwrap();
        }
        let fake = home.path().join(format!(
            "status-continuation.{}",
            std::env::consts::EXE_EXTENSION
        ));
        std::fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &fake).unwrap();
        let suffix = if malformed { "unknown" } else { EXECUTION };
        let rollout = home.path().join(format!(
            "rollout-2026-10-24T14-00-00-{LOGICAL}_{suffix}.jsonl"
        ));
        std::fs::write(
            fake.with_extension("rollout-path"),
            rollout.to_str().unwrap(),
        )
        .unwrap();
        std::fs::write(fake.with_extension("idle"), b"idle").unwrap();
        let source = codex_zectrix_dashboard::CodexStatusSource::new(
            codex_zectrix_dashboard::AppServerClient::new(&fake),
        )
        .with_history_directory(home.path());
        let observed = source.read_latest_task_observation().unwrap();
        assert_eq!(
            observed.state,
            TaskState::Unavailable,
            "newer={newer}, malformed={malformed}"
        );
        assert!(observed.partial);
        assert!(
            !fake.with_extension("old-read").exists(),
            "must not ask for superseded turn"
        );
    }
}

// Same shape as Desktop: thread/list returns the logical ID while the current
// rollout and history projection have a distinct execution ID.
#[test]
fn continued_desktop_execution_replaces_the_old_interrupted_turn() {
    let home = tempfile::tempdir().unwrap();
    let now = codex_zectrix_dashboard::platform::now_epoch_seconds().unwrap();
    let db = fixture(home.path(), "interrupted", now);
    db.execute(
        "UPDATE thread_turns SET thread_id=?1,completed_at=?2",
        params![LOGICAL, now - 300],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_turns VALUES (?1,'continued',1,'inProgress',?2,NULL)",
        params![EXECUTION, now - 30],
    )
    .unwrap();
    let fake = home.path().join(format!(
        "status-continuation.{}",
        std::env::consts::EXE_EXTENSION
    ));
    std::fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &fake).unwrap();
    let rollout = home.path().join(format!(
        "rollout-2026-10-24T14-00-00-{LOGICAL}_{EXECUTION}.jsonl"
    ));
    std::fs::write(
        fake.with_extension("rollout-path"),
        rollout.to_str().unwrap(),
    )
    .unwrap();
    let source = codex_zectrix_dashboard::CodexStatusSource::new(
        codex_zectrix_dashboard::AppServerClient::new(&fake),
    )
    .with_history_directory(home.path());
    for (status, expected) in [
        ("inProgress", TaskState::Running),
        ("completed", TaskState::Completed),
        ("failed", TaskState::Failed),
        ("interrupted", TaskState::Interrupted),
    ] {
        db.execute(
            "UPDATE thread_turns SET status=?1,completed_at=?2 WHERE thread_id=?3",
            params![
                status,
                if status == "inProgress" {
                    None
                } else {
                    Some(now)
                },
                EXECUTION
            ],
        )
        .unwrap();
        let observed = source.read_latest_task_observation().unwrap();
        assert_eq!(observed.state, expected, "continued {status}");
        assert!(!observed.partial);
    }
    db.execute("DELETE FROM thread_turns WHERE thread_id=?1", [EXECUTION])
        .unwrap();
    let observed = source.read_latest_task_observation().unwrap();
    assert_eq!(
        observed.state,
        TaskState::Unavailable,
        "missing current projection must not revive old interruption"
    );
    assert!(observed.partial);
}

#[test]
fn rollout_mapping_is_exact_and_never_falls_back_to_a_superseded_execution() {
    let home = tempfile::tempdir().unwrap();
    let db = fixture(home.path(), "interrupted", 10000);
    db.execute(
        "UPDATE thread_turns SET thread_id=?1,completed_at=10000",
        [LOGICAL],
    )
    .unwrap();
    db.execute(
        "INSERT INTO thread_turns VALUES (?1,'current',1,'inProgress',10000,NULL)",
        [EXECUTION],
    )
    .unwrap();
    let history = LocalTaskHistory::open(home.path()).unwrap();
    let continued = home.path().join(format!(
        "rollout-2026-10-24T14-00-00-{LOGICAL}_{EXECUTION}.jsonl"
    ));
    assert_eq!(
        history
            .read_state_for_rollout(LOGICAL, Some(&continued), 10179)
            .unwrap(),
        Some(TaskState::Running)
    );
    assert_eq!(
        history
            .read_state_for_rollout(LOGICAL, Some(&continued), 10180)
            .unwrap(),
        Some(TaskState::Unavailable)
    );
    assert_eq!(
        history
            .read_state_for_rollout(LOGICAL, Some(&continued), 9999)
            .unwrap(),
        Some(TaskState::Unavailable)
    );
    let normal = home
        .path()
        .join(format!("rollout-2026-10-24T14-00-00-{LOGICAL}.jsonl"));
    assert_eq!(
        history
            .read_state_for_rollout(LOGICAL, Some(&normal), 10001)
            .unwrap(),
        Some(TaskState::Interrupted)
    );
    for name in [
        format!("rollout-2026-10-24T14-00-00-{EXECUTION}_{LOGICAL}.jsonl"),
        format!("rollout-2026-10-24T14-00-00-{LOGICAL}_{EXECUTION}_extra.jsonl"),
        format!("rollout-2026-10-24T14-00-00-{LOGICAL}_unknown.jsonl"),
        format!("rollout-INVALID-TIMESTAMP00-{LOGICAL}_{EXECUTION}.jsonl"),
        format!("rollout-2026-10-24T14-00-00-{LOGICAL}_{EXECUTION}.txt"),
    ] {
        assert!(
            history
                .read_state_for_rollout(LOGICAL, Some(&home.path().join(name)), 10001)
                .is_err()
        );
    }
    assert!(
        history
            .read_state_for_rollout(LOGICAL, Some(std::path::Path::new("relative.jsonl")), 10001)
            .is_err()
    );
}

#[test]
fn active_record_requires_recent_activity_and_never_reads_message_content() {
    let home = tempfile::tempdir().unwrap();
    drop(fixture(home.path(), "inProgress", 10000));
    let before = std::fs::read(home.path().join("thread_history_1.sqlite")).unwrap();
    let history = LocalTaskHistory::open(home.path()).unwrap();
    assert_eq!(
        history.read_state("local-root", 10000).unwrap(),
        Some(TaskState::Running)
    );
    assert_eq!(
        history.read_state("local-root", 10179).unwrap(),
        Some(TaskState::Running)
    );
    assert_eq!(history.read_state("local-root", 10180).unwrap(), None);
    assert_eq!(history.read_state("local-root", 9999).unwrap(), None);
    assert_eq!(
        history
            .read_state("another-account-or-child", 10000)
            .unwrap(),
        None
    );
    drop(history);
    assert_eq!(
        std::fs::read(home.path().join("thread_history_1.sqlite")).unwrap(),
        before
    );
}

#[test]
fn latest_turn_wins_over_old_failures_and_end_states_need_real_timestamps() {
    for (status, expected) in [
        ("completed", TaskState::Completed),
        ("failed", TaskState::Failed),
        ("interrupted", TaskState::Interrupted),
    ] {
        let home = tempfile::tempdir().unwrap();
        let db = fixture(home.path(), status, 10000);
        let history = LocalTaskHistory::open(home.path()).unwrap();
        assert_eq!(history.read_state("local-root", 10000).unwrap(), None);
        drop(history);
        db.execute("UPDATE thread_turns SET completed_at=10000", [])
            .unwrap();
        let history = LocalTaskHistory::open(home.path()).unwrap();
        assert_eq!(
            history.read_state("local-root", 10001).unwrap(),
            Some(expected)
        );
        assert_eq!(history.read_state("local-root", 96400).unwrap(), None);
        drop(history);
        db.execute(
            "INSERT INTO thread_turns VALUES ('local-root','new',2,'inProgress',10001,NULL)",
            [],
        )
        .unwrap();
        let history = LocalTaskHistory::open(home.path()).unwrap();
        assert_eq!(
            history.read_state("local-root", 10002).unwrap(),
            Some(TaskState::Running)
        );
    }
}

#[test]
fn absent_or_newer_history_format_is_never_created_or_guessed() {
    let home = tempfile::tempdir().unwrap();
    assert!(LocalTaskHistory::open(home.path()).is_err());
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    drop(fixture(home.path(), "inProgress", 10000));
    std::fs::write(home.path().join("thread_history_2.sqlite"), b"future").unwrap();
    assert!(LocalTaskHistory::open(home.path()).is_err());
}

fn fixture(home: &std::path::Path, status: &str, now: i64) -> Connection {
    let db = Connection::open(home.join("thread_history_1.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,rollout_ordinal INTEGER,status TEXT,started_at INTEGER,completed_at INTEGER);
        CREATE TABLE thread_items(thread_id TEXT,turn_id TEXT,created_at_ms INTEGER,started_at_ms INTEGER,completed_at_ms INTEGER,item_json TEXT);").unwrap();
    db.execute(
        "INSERT INTO thread_turns VALUES ('local-root','turn',1,?1,?2,NULL)",
        params![status, now - 600],
    )
    .unwrap();
    db.execute("INSERT INTO thread_items VALUES ('local-root','turn',?1,?1,?1,'PROMPT_MUST_NEVER_BE_READ')",[now*1000]).unwrap();
    db
}

#[test]
fn live_desktop_execution_is_observed_even_when_private_server_reports_not_loaded() {
    let home = tempfile::tempdir().unwrap();
    let now = codex_zectrix_dashboard::platform::now_epoch_seconds().unwrap();
    drop(fixture(home.path(), "inProgress", now));
    let fake = home.path().join(format!(
        "status-local-history.{}",
        std::env::consts::EXE_EXTENSION
    ));
    std::fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &fake).unwrap();
    let source = codex_zectrix_dashboard::CodexStatusSource::new(
        codex_zectrix_dashboard::AppServerClient::new(&fake),
    )
    .with_history_directory(home.path());
    let observed = source.read_latest_task_observation().unwrap();
    assert_eq!(observed.state, TaskState::Running);
    assert!(!observed.partial);
    let output = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .arg("diagnostics")
        .env("CODEX_HOME", home.path())
        .env("CODEX_ZECTRIX_CODEX_BIN", fake)
        .env("CODEX_ZECTRIX_DATA_DIR", home.path().join("data"))
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.ends_with("codex_task_method=supported\n"), "{text}");
    assert!(!text.contains("PROMPT_MUST_NEVER_BE_READ"));
}
