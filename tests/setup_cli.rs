use std::process::Command;

mod common;

#[test]
fn operator_preview_cli_is_cross_platform_offline_and_confined_to_its_output() {
    // Catches reactivating the obsolete setup subprocess tests that touched keyrings/launchctl.
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("operator-preview.png");
    let app_data = directory.path().join("must-not-create-app-data");

    let output = Command::new(common::dashboard_binary())
        .args(["preview", "--output"])
        .arg(&output_path)
        .env("CODEX_ZECTRIX_DATA_DIR", &app_data)
        .env(
            "CODEX_ZECTRIX_API_BASE",
            "http://127.0.0.1:9/must-not-contact",
        )
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            directory.path().join("must-not-start-codex"),
        )
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "preview_written\n"
    );
    assert!(output_path.is_file());
    assert!(!app_data.exists());
    let bytes = std::fs::read(output_path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
}
