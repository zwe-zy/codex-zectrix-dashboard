#![cfg(feature = "test-support")]
use codex_zectrix_dashboard::{TaskState, local_task_history::LocalTaskHistory};
use rusqlite::{Connection, params};
use std::process::Command;

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
