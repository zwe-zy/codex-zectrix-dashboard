#![cfg(windows)]
#[path = "support/install_fixture.rs"]
#[allow(dead_code)]
mod fixture;
use codex_zectrix_dashboard::{
    install::InstallManager,
    platform::shortcut_windows::{ShortcutManager, ShortcutOutcome},
};
use std::{fs, path::Path, process::Command};

fn change_link(link: &Path, property: &str, value: &str) {
    let script = format!(
        "$ErrorActionPreference='Stop'; $s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.{}='{}'; $s.Save()",
        link.display().to_string().replace('\'', "''"),
        property,
        value.replace('\'', "''")
    );
    assert!(
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn owned_shortcut_follows_installed_gui_and_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("测试 data & ' apostrophe");
    let programs = dir.path().join("Programs");
    let mut installer = InstallManager::new(&data, fixture::Host::default());
    installer
        .install_or_update(&fixture::desktop_package(dir.path(), "0.3.0-rc.1"))
        .unwrap();
    let manager = ShortcutManager::new(programs.clone(), data.join("app"));
    assert_eq!(
        manager
            .ensure(&data.join("app/0.3.0-rc.1/NOTE4控制台.exe"))
            .unwrap(),
        ShortcutOutcome::Created
    );
    assert!(programs.join("Codex NOTE4.lnk").is_file());
    installer
        .install_or_update(&fixture::desktop_package(dir.path(), "0.3.0-rc.2"))
        .unwrap();
    assert_eq!(
        manager
            .ensure(&data.join("app/0.3.0-rc.2/NOTE4控制台.exe"))
            .unwrap(),
        ShortcutOutcome::Updated
    );
    assert_eq!(manager.remove_owned().unwrap(), ShortcutOutcome::Removed);
    assert_eq!(manager.remove_owned().unwrap(), ShortcutOutcome::Absent);
}

#[test]
fn modified_shortcut_is_not_overwritten_or_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let programs = dir.path().join("Programs");
    InstallManager::new(&data, fixture::Host::default())
        .install_or_update(&fixture::desktop_package(dir.path(), "0.3.0-rc.1"))
        .unwrap();
    let gui = data.join("app/0.3.0-rc.1/NOTE4控制台.exe");
    let manager = ShortcutManager::new(programs.clone(), data.join("app"));
    let link = programs.join("Codex NOTE4.lnk");
    for (property, value) in [
        ("Arguments", "--user-option"),
        ("WorkingDirectory", dir.path().to_str().unwrap()),
        ("TargetPath", "C:\\Windows\\notepad.exe"),
    ] {
        manager.ensure(&gui).unwrap();
        change_link(&link, property, value);
        let original = fs::read(&link).unwrap();
        assert_eq!(
            manager.ensure(&gui).unwrap(),
            ShortcutOutcome::PreservedForeign
        );
        assert_eq!(
            manager.remove_owned().unwrap(),
            ShortcutOutcome::PreservedForeign
        );
        assert_eq!(fs::read(&link).unwrap(), original);
        fs::remove_file(&link).unwrap();
    }
}

#[test]
fn shortcut_accepts_canonical_windows_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let data = root.join("data");
    let programs = root.join("Programs");
    InstallManager::new(&data, fixture::Host::default())
        .install_or_update(&fixture::desktop_package(&root, "0.3.0-rc.1"))
        .unwrap();
    let manager = ShortcutManager::new(programs.clone(), data.join("app"));
    assert_eq!(
        manager
            .ensure(&data.join("app/0.3.0-rc.1/NOTE4控制台.exe"))
            .unwrap(),
        ShortcutOutcome::Created
    );
    assert!(programs.join("Codex NOTE4.lnk").is_file());
    assert_eq!(manager.remove_owned().unwrap(), ShortcutOutcome::Removed);
}
