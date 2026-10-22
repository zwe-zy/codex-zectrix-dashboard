#![cfg(windows)]
use codex_zectrix_dashboard::platform::background_windows::{
    BackgroundController, BackgroundError, BackgroundStatus, InstanceGuard, OperationGuard,
};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

#[test]
fn second_instance_never_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let first = InstanceGuard::acquire(dir.path(), None).unwrap();
    let uploads = AtomicUsize::new(0);
    if let Ok(_second) = InstanceGuard::acquire(dir.path(), None) {
        uploads.fetch_add(1, Ordering::SeqCst);
    }
    assert_eq!(uploads.load(Ordering::SeqCst), 0);
    drop(first);
    assert!(InstanceGuard::acquire(dir.path(), None).is_ok());
}
#[test]
fn maintenance_blocks_start() {
    let dir = tempfile::tempdir().unwrap();
    let _op = OperationGuard::acquire(dir.path()).unwrap();
    assert!(OperationGuard::acquire(dir.path()).is_err());
    let ctl = BackgroundController::new(dir.path(), &std::env::current_exe().unwrap());
    assert!(matches!(ctl.start(), Err(BackgroundError::Busy)));
    assert!(matches!(
        InstanceGuard::acquire(dir.path(), None),
        Err(BackgroundError::Busy)
    ));
}
#[test]
fn stop_rejects_reused_pid() {
    let dir = tempfile::tempdir().unwrap();
    let _instance = InstanceGuard::acquire(dir.path(), None).unwrap();
    let marker = dir.path().join("background.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    value["createdAt"] = serde_json::json!(0);
    fs::write(&marker, serde_json::to_vec(&value).unwrap()).unwrap();
    let ctl = BackgroundController::new(dir.path(), &std::env::current_exe().unwrap());
    assert!(matches!(ctl.stop(), Err(BackgroundError::IdentityMismatch)));
    assert!(!dir.path().join("stop-request.json").exists());
}
#[test]
fn stop_waits_for_matching_instance_to_exit() {
    let dir = tempfile::tempdir().unwrap();
    let instance = InstanceGuard::acquire(dir.path(), None).unwrap();
    instance.mark_ready().unwrap();
    let ctl = BackgroundController::new(dir.path(), &std::env::current_exe().unwrap());
    assert_eq!(ctl.status().unwrap(), BackgroundStatus::Running);
    let exited = Arc::new(AtomicUsize::new(0));
    let done = exited.clone();
    let worker = thread::spawn(move || {
        while !instance.stop_requested() {
            thread::sleep(Duration::from_millis(10));
        }
        drop(instance);
        done.store(1, Ordering::SeqCst);
    });
    ctl.stop().unwrap();
    worker.join().unwrap();
    assert_eq!(exited.load(Ordering::SeqCst), 1);
    assert_eq!(ctl.status().unwrap(), BackgroundStatus::Stopped);
}
#[test]
fn credential_commit_interrupted_blocks_publish() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("configuration-pending.json"),
        b"{\"schemaVersion\":1}",
    )
    .unwrap();
    let uploads = AtomicUsize::new(0);
    if let Ok(_worker) = InstanceGuard::acquire(dir.path(), None) {
        uploads.fetch_add(1, Ordering::SeqCst);
    }
    assert_eq!(uploads.load(Ordering::SeqCst), 0);
}

#[test]
fn unfinished_installation_blocks_uncontrolled_start() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("installation-pending.json"),
        b"{\"schemaVersion\":1}",
    )
    .unwrap();
    assert!(InstanceGuard::acquire(dir.path(), None).is_err());
    let ctl = BackgroundController::new(dir.path(), &std::env::current_exe().unwrap());
    assert!(matches!(
        ctl.start(),
        Err(BackgroundError::RecoveryRequired)
    ));
}
