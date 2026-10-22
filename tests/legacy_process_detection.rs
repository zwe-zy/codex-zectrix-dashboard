#![cfg(all(windows, feature = "test-support"))]
use codex_zectrix_dashboard::install::windows::owned_companion_processes;
use std::process::{Command, Stdio};

#[test]
fn detects_an_owned_companion_without_a_scheduled_task_or_identity_file() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("codex-zectrix-companion.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &exe).unwrap();
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let result = owned_companion_processes();
    drop(child.stdin.take());
    child.wait().unwrap();
    assert!(
        result
            .unwrap()
            .iter()
            .any(|p| p.pid == child.id() && p.executable == exe)
    );
}
