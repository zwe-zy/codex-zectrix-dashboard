#![cfg(windows)]
#[path = "support/install_fixture.rs"]
mod fixture;
use codex_zectrix_dashboard::install::{InstallManager, Installation};
use fixture::*;
use std::fs;

#[test]
fn failed_readiness_restores_old_program_and_data() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager.install_or_update(&pkg).unwrap();
    let old = Installation::load(&data).unwrap().unwrap();
    let settings = br#"{"deviceId":"old-route","pageId":1}"#;
    fs::write(data.join("settings.json"), settings).unwrap();
    manager.host_mut().state.running_executable = Some(old.companion(&data));
    manager.host_mut().fail_ready = true;
    let next = package(dir.path(), "0.2.0-rc.2");
    assert!(manager.install_or_update(&next).is_err());
    assert_eq!(
        Installation::load(&data).unwrap().unwrap().active_version,
        old.active_version
    );
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), settings);
}
#[test]
fn interrupted_upgrade_restores_old_readable_state() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager.install_or_update(&pkg).unwrap();
    let old = Installation::load(&data).unwrap().unwrap();
    let state=br#"{"lastSuccessfulFrameSha256":null,"lastSuccessfulUploadEpochSeconds":100,"nextAllowedAttemptEpochSeconds":null,"failureCount":0}"#;
    fs::write(data.join("status-frame-state.json"), state).unwrap();
    // Capture the same durable boundary used by production, then simulate a newer program writing state and exiting.
    manager.prepare_recovery_checkpoint().unwrap();
    fs::write(
        data.join("status-frame-state.json"),
        br#"{"schemaVersion":999}"#,
    )
    .unwrap();
    manager.recover().unwrap();
    assert_eq!(
        fs::read(data.join("status-frame-state.json")).unwrap(),
        state
    );
    let decoded: codex_zectrix_dashboard::StatusFramePublisherState =
        serde_json::from_slice(state).unwrap();
    assert_eq!(decoded.last_successful_upload_epoch_seconds, Some(100));
    assert_eq!(
        Installation::load(&data).unwrap().unwrap().active_version,
        old.active_version
    );
}
#[test]
fn unknown_journal_schema_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("installation-pending.json"),
        b"{\"schemaVersion\":999}",
    )
    .unwrap();
    let mut manager = InstallManager::new(dir.path(), Host::default());
    assert!(manager.recover().is_err());
    assert_eq!(manager.host().starts, 0);
}
