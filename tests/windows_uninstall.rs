#![cfg(windows)]
#[path = "support/install_fixture.rs"]
mod fixture;
use codex_zectrix_dashboard::{
    install::{InstallManager, Installation},
    operator::cleanup_installed_files,
};
use fixture::*;
#[test]
fn retry_completes_partial_cleanup_after_locked_binary_is_released() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let pkg = desktop_package(dir.path(), env!("CARGO_PKG_VERSION"));
    InstallManager::new(&data, Host::default())
        .install_or_update(&pkg)
        .unwrap();
    let install = Installation::load(&data).unwrap().unwrap();
    std::fs::write(data.join("settings.json"), b"keep").unwrap();
    // Permit reads for package verification but deny deletion of this binary.
    let busy = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(install.companion(&data))
        .unwrap();
    assert!(cleanup_installed_files(&data).is_err());
    assert!(data.join("uninstall-pending.json").is_file());
    assert!(
        !install
            .directory(&data)
            .join("codex-zectrix-dashboard.exe")
            .exists()
    );
    drop(busy);
    cleanup_installed_files(&data).unwrap();
    assert!(!data.join("uninstall-pending.json").exists());
    assert!(!data.join("installation.json").exists());
    assert_eq!(std::fs::read(data.join("settings.json")).unwrap(), b"keep");
}
#[test]
fn gui_uninstall_uses_verified_cli_helper() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = desktop_package(dir.path(), "0.3.0-rc.1");
    let data = dir.path().join("data");
    InstallManager::new(&data, Host::default())
        .install_or_update(&pkg)
        .unwrap();
    let install = Installation::load(&data).unwrap().unwrap();
    let helper =
        codex_zectrix_dashboard::operator::verified_uninstall_helper_source(&data, &install)
            .unwrap();
    assert_eq!(helper.file_name().unwrap(), "codex-zectrix-dashboard.exe");
    std::fs::write(&helper, b"modified").unwrap();
    assert!(
        codex_zectrix_dashboard::operator::verified_uninstall_helper_source(&data, &install)
            .is_err()
    );
}

#[test]
fn cleanup_understands_both_package_generations() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let mut manager = InstallManager::new(&data, Host::default());
    manager
        .install_or_update(&package(dir.path(), "0.2.0-rc.5"))
        .unwrap();
    manager
        .install_or_update(&desktop_package(dir.path(), "0.3.0-rc.1"))
        .unwrap();
    std::fs::write(data.join("settings.json"), b"keep").unwrap();
    cleanup_installed_files(&data).unwrap();
    assert_eq!(std::fs::read_dir(data.join("app")).unwrap().count(), 0);
    assert_eq!(std::fs::read(data.join("settings.json")).unwrap(), b"keep");
}
#[test]
fn cleanup_refuses_unlisted_files_before_removing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    InstallManager::new(&data, Host::default())
        .install_or_update(&pkg)
        .unwrap();
    let install = Installation::load(&data).unwrap().unwrap();
    let unknown = install.directory(&data).join("my-file.txt");
    std::fs::write(&unknown, b"keep").unwrap();
    assert!(cleanup_installed_files(&data).is_err());
    assert!(install.companion(&data).exists());
    assert_eq!(std::fs::read(&unknown).unwrap(), b"keep");
}
#[test]
fn cleanup_removes_only_verified_program_files_and_preserves_user_data() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = package(dir.path(), "0.2.0-rc.1");
    let data = dir.path().join("data");
    InstallManager::new(&data, Host::default())
        .install_or_update(&pkg)
        .unwrap();
    std::fs::write(data.join("settings.json"), b"keep configured route").unwrap();
    cleanup_installed_files(&data).unwrap();
    assert!(!data.join("installation.json").exists());
    assert!(data.join("settings.json").exists());
}
