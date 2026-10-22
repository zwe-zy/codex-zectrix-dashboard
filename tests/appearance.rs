use codex_zectrix_dashboard::status_board::BoardTheme;
use codex_zectrix_dashboard::{AppFile, AppearanceSettings, Settings};

#[test]
fn missing_appearance_keeps_existing_install_on_pop_cut_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("appearance.json");
    assert_eq!(
        AppearanceSettings::load(&path).unwrap().theme,
        BoardTheme::PopCut
    );
    assert!(!path.exists());
}

#[test]
fn themes_round_trip_without_changing_device_settings() {
    let directory = tempfile::tempdir().unwrap();
    let routing = directory.path().join("settings.json");
    let original = br#"{"deviceId":"device-a","pageId":2}"#;
    std::fs::write(&routing, original).unwrap();
    let path = directory.path().join("appearance.json");
    for name in [
        "pop-cut",
        "eva",
        "pixel-console",
        "space-gauge",
        "cyber-hud",
    ] {
        let theme: BoardTheme = name.parse().expect("registered selectable theme");
        AppearanceSettings { theme }.save_atomically(&path).unwrap();
        assert_eq!(AppearanceSettings::load(&path).unwrap().theme, theme);
        assert_eq!(std::fs::read(&routing).unwrap(), original);
        assert_eq!(Settings::load(&routing).unwrap().unwrap().page_id, 2);
    }
    assert!(AppFile::uninstall_targets().contains(&AppFile::Appearance));
}

#[test]
fn invalid_or_future_appearance_is_reported_without_overwriting_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("appearance.json");
    for invalid in [
        "broken",
        r#"{"theme":"future"}"#,
        r#"{"theme":"eva","apiKey":"private"}"#,
        "{}",
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(AppearanceSettings::load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
    }
}

#[cfg(windows)]
#[test]
fn theme_changes_are_serialized_and_blocked_by_incomplete_operations() {
    use codex_zectrix_dashboard::platform::background_windows::OperationGuard;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("appearance.json");
    let installed = serde_json::json!({"schemaVersion":1,"activeVersion":env!("CARGO_PKG_VERSION"),"previousVersion":null});
    std::fs::write(
        directory.path().join("installation.json"),
        serde_json::to_vec(&installed).unwrap(),
    )
    .unwrap();
    let guard = OperationGuard::acquire(directory.path()).unwrap();
    assert!(
        codex_zectrix_dashboard::operator::save_theme(directory.path(), BoardTheme::Eva).is_err()
    );
    assert!(!path.exists());
    drop(guard);
    for journal in [
        "installation-pending.json",
        "configuration-pending.json",
        "uninstall-pending.json",
    ] {
        std::fs::write(directory.path().join(journal), b"pending").unwrap();
        assert!(
            codex_zectrix_dashboard::operator::save_theme(directory.path(), BoardTheme::Eva)
                .is_err()
        );
        assert!(!path.exists());
        std::fs::remove_file(directory.path().join(journal)).unwrap();
    }
    codex_zectrix_dashboard::operator::save_theme(directory.path(), BoardTheme::Eva).unwrap();
    assert_eq!(
        AppearanceSettings::load(path).unwrap().theme,
        BoardTheme::Eva
    );
}

#[cfg(windows)]
#[test]
fn inkcard_mode_change_honors_operation_locks_pending_work_and_active_version() {
    use codex_zectrix_dashboard::inkcards::InkcardMode;
    use codex_zectrix_dashboard::platform::background_windows::OperationGuard;
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let write_install = |version: &str| {
        std::fs::write(data.join("installation.json"), serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"activeVersion":version,"previousVersion":null})).unwrap()).unwrap()
    };
    write_install(env!("CARGO_PKG_VERSION"));
    let guard = OperationGuard::acquire(data).unwrap();
    assert!(codex_zectrix_dashboard::operator::save_inkcard_mode(data, InkcardMode::All).is_err());
    drop(guard);
    for file in [
        "installation-pending.json",
        "configuration-pending.json",
        "uninstall-pending.json",
    ] {
        std::fs::write(data.join(file), b"pending").unwrap();
        assert!(
            codex_zectrix_dashboard::operator::save_inkcard_mode(data, InkcardMode::All).is_err()
        );
        assert!(!data.join("inkcards.json").exists());
        std::fs::remove_file(data.join(file)).unwrap();
    }
    write_install("0.1.9");
    assert!(codex_zectrix_dashboard::operator::save_inkcard_mode(data, InkcardMode::All).is_err());
    assert!(!data.join("inkcards.json").exists());
}
