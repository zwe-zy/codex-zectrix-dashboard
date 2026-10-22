#![cfg(windows)]
#[path = "support/install_fixture.rs"]
mod fixture;
use codex_zectrix_dashboard::install::{InstallManager, Installation, Package};
use fixture::*;
use std::fs;

#[test]
fn stale_window_cannot_reactivate_old_version_after_another_installer() {
    use codex_zectrix_dashboard::install::InstallError;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let old = desktop_package(dir.path(), "0.3.0-rc.1");
    let new = desktop_package(dir.path(), "0.3.0-rc.2");
    let mut manager = InstallManager::new(&data, Host::default());
    manager.install_or_update(&old).unwrap();
    let expected = Installation::load(&data).unwrap().unwrap().active_version;
    manager.install_or_update(&new).unwrap();
    let before = fs::read(data.join("installation.json")).unwrap();
    let checks = manager.host().versions_checked;
    for expected in [Some(expected.as_str()), None] {
        assert!(matches!(
            manager.install_if_current(&old, expected),
            Err(InstallError::InstallationChanged)
        ));
        assert_eq!(fs::read(data.join("installation.json")).unwrap(), before);
        assert_eq!(manager.host().versions_checked, checks);
        assert!(!data.join("installation-pending.json").exists());
    }
    // The version check must run inside the same guard as the transaction.
    let _guard =
        codex_zectrix_dashboard::platform::background_windows::OperationGuard::acquire(&data)
            .unwrap();
    assert!(matches!(
        manager.install_if_current(&old, Some("0.3.0-rc.1")),
        Err(InstallError::Background(_))
    ));
}

#[test]
fn v1_to_v2_upgrade_and_failure_restore() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager
        .install_or_update(&package(dir.path(), "0.2.0-rc.5"))
        .unwrap();
    let old = Installation::load(&data).unwrap().unwrap();
    let settings = br#"{"deviceId":"fixture","pageId":2}"#;
    fs::write(data.join("settings.json"), settings).unwrap();
    manager.host_mut().state.running_executable = Some(old.companion(&data));
    manager.host_mut().state.startup = Some(format!("\"{}\"", old.companion(&data).display()));
    let original = manager.host().state.clone();
    let next = desktop_package(dir.path(), "0.3.0-rc.1");
    manager.host_mut().fail_ready = true;
    assert!(matches!(
        manager.install_or_update(&next),
        Err(codex_zectrix_dashboard::install::InstallError::Health)
    ));
    assert_eq!(
        Installation::load(&data).unwrap().unwrap().active_version,
        "0.2.0-rc.5"
    );
    assert_eq!(
        manager.host().state.running_executable,
        original.running_executable
    );
    assert_eq!(manager.host().state.startup, original.startup);
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), settings);
    manager.host_mut().fail_ready = false;
    manager.install_or_update(&next).unwrap();
    let new = Installation::load(&data).unwrap().unwrap();
    assert_eq!(new.active_version, "0.3.0-rc.1");
    assert_eq!(new.previous_version, Some("0.2.0-rc.5".into()));
    assert_eq!(fs::read_dir(new.directory(&data)).unwrap().count(), 7);
    assert_eq!(
        manager.host().state.running_executable,
        Some(new.companion(&data))
    );
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), settings);
}

#[test]
fn upgrade_and_rollback_keep_theme_preferences_and_device_settings() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager
        .install_or_update(&package(dir.path(), "0.2.0-rc.2"))
        .unwrap();
    let appearance = br#"{"theme":"eva"}"#;
    let settings = br#"{"deviceId":"fixture","pageId":2}"#;
    fs::write(data.join("appearance.json"), appearance).unwrap();
    fs::write(data.join("settings.json"), settings).unwrap();
    let mode = br#"{"mode":"all"}"#;
    fs::write(data.join("inkcards.json"), mode).unwrap();
    let state = codex_zectrix_dashboard::inkcards::InkcardCoordinator::new(
        &codex_zectrix_dashboard::Settings {
            device_id: "fixture".into(),
            page_id: 2,
        },
        &Default::default(),
        None,
    )
    .unwrap()
    .snapshot();
    state
        .save_atomically(data.join("inkcard-state.json"))
        .unwrap();
    let original_inkcards = fs::read(data.join("inkcard-state.json")).unwrap();
    manager
        .install_or_update(&package(dir.path(), "0.2.0-rc.3"))
        .unwrap();
    assert_eq!(fs::read(data.join("appearance.json")).unwrap(), appearance);
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), settings);
    assert_eq!(fs::read(data.join("inkcards.json")).unwrap(), mode);
    assert_eq!(
        fs::read(data.join("inkcard-state.json")).unwrap(),
        original_inkcards
    );
    let mut failing = InstallManager::new(
        &data,
        Host {
            fail_ready: true,
            ..Default::default()
        },
    );
    assert!(
        failing
            .install_or_update(&package(dir.path(), "0.2.0-rc.4"))
            .is_err()
    );
    assert_eq!(fs::read(data.join("appearance.json")).unwrap(), appearance);
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), settings);
    assert_eq!(fs::read(data.join("inkcards.json")).unwrap(), mode);
    assert_eq!(
        fs::read(data.join("inkcard-state.json")).unwrap(),
        original_inkcards
    );
}

#[test]
fn installed_files_do_not_depend_on_download_directory() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager.install_or_update(&pkg).unwrap();
    fs::remove_dir_all(&pkg).unwrap();
    let installation = Installation::load(&data).unwrap().unwrap();
    assert_eq!(installation.active_version, "0.2.0-rc.1");
    assert!(installation.companion(&data).is_file());
}
#[test]
fn invalid_package_never_changes_installed_version() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager.install_or_update(&pkg).unwrap();
    let next = package(dir.path(), "0.2.0-rc.2");
    fs::write(
        next.join("codex-zectrix-companion.exe"),
        b"different version",
    )
    .unwrap();
    assert!(manager.install_or_update(&next).is_err());
    assert_eq!(
        Installation::load(&data).unwrap().unwrap().active_version,
        "0.2.0-rc.1"
    );
    fs::remove_file(next.join("codex-zectrix-companion.exe")).unwrap();
    assert!(Package::load(&next).is_err());
}
#[test]
fn legacy_stop_denied_preserves_old_install() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    let host = Host {
        deny_stop: true,
        ..Default::default()
    };
    let mut manager = InstallManager::new(&data, host);
    assert!(manager.install_or_update(&pkg).is_err());
    assert_eq!(manager.host().starts, 0);
    assert!(manager.host().state.startup.is_none());
    assert!(Installation::load(&data).unwrap().is_none());
    assert!(
        !data.join("installation-pending.json").exists(),
        "an unchanged, denied stop must not leave an unnecessary recovery loop"
    );
}
#[test]
fn cleanup_rejects_link_outside_install() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"keep").unwrap();
    let data = dir.path().join("data");
    fs::create_dir(&data).unwrap();
    let junction = data.join("app");
    let status = std::process::Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(status.status.success());
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let mut manager = InstallManager::new(&data, Host::default());
    assert!(manager.install_or_update(&pkg).is_err());
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    fs::remove_dir(&junction).unwrap();
}
