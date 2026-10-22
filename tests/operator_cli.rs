#![cfg(windows)]
use std::{
    io::Write,
    process::{Command, Stdio},
};
mod common;
#[test]
fn no_arguments_opens_chinese_menu_and_exit_does_not_install() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let mut child = Command::new(common::dashboard_binary())
        .env("CODEX_ZECTRIX_DATA_DIR", &data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"0\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("退出"));
    assert!(!data.join("installation.json").exists());
}

fn theme_fixture(data: &std::path::Path, version: &str) {
    let state =
        serde_json::json!({"schemaVersion":1,"activeVersion":version,"previousVersion":null});
    std::fs::write(
        data.join("installation.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
}

#[test]
fn menu_switches_all_five_themes_and_cancel_keeps_last_choice() {
    use codex_zectrix_dashboard::AppearanceSettings;
    let temp = tempfile::tempdir().unwrap();
    theme_fixture(temp.path(), env!("CARGO_PKG_VERSION"));
    let route = br#"{"deviceId":"local-fixture","pageId":2}"#;
    std::fs::write(temp.path().join("settings.json"), route).unwrap();
    for (input, expected) in [
        ("8\n9\n2\n0\n", "eva"),
        ("8\n3\n0\n", "pixel-console"),
        ("8\n4\n0\n", "space-gauge"),
        ("8\n5\n0\n", "cyber-hud"),
        ("8\n0\n0\n", "cyber-hud"),
        ("8\n1\n0\n", "pop-cut"),
    ] {
        let mut child = Command::new(common::dashboard_binary())
            .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("8. 切换主题"));
        assert_eq!(
            AppearanceSettings::load(temp.path().join("appearance.json"))
                .unwrap()
                .theme
                .as_str(),
            expected
        );
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            route
        );
    }
}

#[test]
fn theme_cli_rejects_invalid_arguments_uninstalled_or_changed_version() {
    let temp = tempfile::tempdir().unwrap();
    for args in [
        vec!["theme"],
        vec!["theme", "future"],
        vec!["theme", "eva", "extra"],
        vec!["theme", "eva"],
    ] {
        let output = Command::new(common::dashboard_binary())
            .args(args)
            .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!temp.path().join("appearance.json").exists());
    }
    theme_fixture(temp.path(), "0.1.9");
    let output = Command::new(common::dashboard_binary())
        .args(["theme", "eva"])
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!temp.path().join("appearance.json").exists());
    theme_fixture(temp.path(), env!("CARGO_PKG_VERSION"));
    let output = Command::new(common::dashboard_binary())
        .args(["theme", "eva"])
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
        .output()
        .unwrap();
    assert!(output.status.success());
}

#[test]
fn inkcard_mode_is_opt_in_and_cli_preserves_the_device_route() {
    let temp = tempfile::tempdir().unwrap();
    theme_fixture(temp.path(), env!("CARGO_PKG_VERSION"));
    let route = br#"{"deviceId":"local-fixture","pageId":3}"#;
    std::fs::write(temp.path().join("settings.json"), route).unwrap();
    assert!(!temp.path().join("inkcards.json").exists());
    for mode in ["all", "single"] {
        let output = Command::new(common::dashboard_binary())
            .args(["inkcards", mode])
            .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.path().join("inkcards.json")).unwrap())
                .unwrap();
        assert_eq!(saved["mode"], mode);
        assert_eq!(
            std::fs::read(temp.path().join("settings.json")).unwrap(),
            route
        );
        assert!(
            !temp.path().join("background.json").exists(),
            "a stopped worker must stay stopped"
        );
    }
    let mut child = Command::new(common::dashboard_binary())
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"9\n8\n1\n9\n0\n0\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("inkcards.json")).unwrap()).unwrap();
    assert_eq!(
        saved["mode"], "all",
        "menu cancellation must preserve five-card mode"
    );
    for args in [
        vec!["inkcards"],
        vec!["inkcards", "future"],
        vec!["inkcards", "single", "extra"],
    ] {
        assert!(
            !Command::new(common::dashboard_binary())
                .args(args)
                .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    assert_eq!(
        std::fs::read(temp.path().join("settings.json")).unwrap(),
        route
    );
}
