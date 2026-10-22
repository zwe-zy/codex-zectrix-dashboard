#![cfg(windows)]
use codex_zectrix_dashboard::platform::background_windows::InstanceGuard;
use codex_zectrix_dashboard::{SetupCommitState, SetupControl, operator::WindowsSetupControl};
fn installed_fixture(data: &std::path::Path) -> std::path::PathBuf {
    std::fs::write(
        data.join("installation.json"),
        br#"{"schemaVersion":1,"activeVersion":"0.2.0-rc.1","previousVersion":null}"#,
    )
    .unwrap();
    data.join("app/0.2.0-rc.1/codex-zectrix-companion.exe")
}
#[test]
fn configuration_cannot_commit_after_another_window_upgrades() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("app/0.1.9/codex-zectrix-companion.exe");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"old").unwrap();
    let mut control = WindowsSetupControl::new(dir.path(), &old);
    std::fs::write(
        dir.path().join("installation.json"),
        br#"{"schemaVersion":1,"activeVersion":"0.2.0-rc.1","previousVersion":"0.1.9"}"#,
    )
    .unwrap();
    assert!(control.begin().is_err());
    assert!(!dir.path().join("configuration-pending.json").exists());
}
#[test]
fn credential_commit_interrupted_blocks_publish_until_configuration_is_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let exe = installed_fixture(dir.path());
    let mut control = WindowsSetupControl::new(dir.path(), &exe);
    control.begin().unwrap();
    assert!(InstanceGuard::acquire(dir.path(), None).is_err());
    let marker = std::fs::read_to_string(dir.path().join("configuration-pending.json")).unwrap();
    assert_eq!(
        marker,
        "{\"schemaVersion\":1,\"requiresConfiguration\":true}"
    );
    control.finish(SetupCommitState::Uncertain).unwrap();
    assert!(InstanceGuard::acquire(dir.path(), None).is_err());
    let mut recovery = WindowsSetupControl::new(dir.path(), &exe);
    recovery.begin().unwrap();
    recovery.finish(SetupCommitState::Restored).unwrap();
    assert!(
        InstanceGuard::acquire(dir.path(), None).is_err(),
        "failed recovery cannot resume a potentially mixed key/route"
    );
}
#[test]
fn clean_rollback_releases_configuration_block() {
    let dir = tempfile::tempdir().unwrap();
    let exe = installed_fixture(dir.path());
    let mut control = WindowsSetupControl::new(dir.path(), &exe);
    control.begin().unwrap();
    control.finish(SetupCommitState::Restored).unwrap();
    assert!(!dir.path().join("configuration-pending.json").exists());
    assert!(InstanceGuard::acquire(dir.path(), None).is_ok());
}
