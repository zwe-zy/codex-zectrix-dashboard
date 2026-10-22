use std::fs;
use std::process::Command;

mod common;
#[cfg(feature = "test-support")]
#[test]
fn a_running_task_does_not_hide_partial_observation() {
    let temp = tempfile::tempdir().unwrap();
    let fake = temp.path().join(format!(
        "status-aggregate-failed.{}",
        std::env::consts::EXE_EXTENSION
    ));
    fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &fake).unwrap();
    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
        .env("CODEX_ZECTRIX_CODEX_BIN", fake)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("codex_task_method=partial\n")
    );
}

#[cfg(feature = "test-support")]
#[test]
fn diagnostics_check_the_direct_source_with_fixed_support_labels() {
    // Removing either direct observation or label redaction breaks this output contract.
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path())
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"),
        )
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout.clone()).unwrap(),
        "quota_source=unavailable\ntask_activity_source=unavailable\nradar_source=unavailable\nradar_reason=none\ncodex_quota_method=supported\ncodex_task_method=partial\n"
    );
    let diagnostics = [output.stdout, output.stderr].concat();
    for forbidden in [
        "TITLE_SENTINEL",
        "PREVIEW_SENTINEL",
        "TURN_ITEM_SENTINEL",
        "PROMPT_SENTINEL",
        "PROJECT_PATH_SENTINEL",
        "top-level-2",
    ] {
        assert!(
            !diagnostics
                .windows(forbidden.len())
                .any(|bytes| bytes == forbidden.as_bytes())
        );
    }
}

#[cfg(feature = "test-support")]
#[test]
fn diagnostics_distinguish_private_quota_method_categories() {
    // Collapsing either RPC category to transport-unavailable breaks these fixed labels.
    for (scenario, quota_label) in [
        ("status-quota-method", "method_unavailable"),
        ("status-quota-account", "account_unavailable"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let fake = temp
            .path()
            .join(format!("{scenario}.{}", std::env::consts::EXE_EXTENSION));
        fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &fake).unwrap();
        let output = Command::new(common::dashboard_binary())
            .arg("diagnostics")
            .env("CODEX_ZECTRIX_DATA_DIR", temp.path().join("data"))
            .env("CODEX_ZECTRIX_CODEX_BIN", fake)
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "quota_source=unavailable\ntask_activity_source=unavailable\nradar_source=unavailable\nradar_reason=none\ncodex_quota_method={quota_label}\ncodex_task_method=partial\n"
            )
        );
    }
}

#[cfg(feature = "test-support")]
#[test]
fn diagnostics_reports_partial_observation_for_not_loaded_tasks() {
    // A reachable protocol cannot establish a notLoaded task state; report partial observation.
    let temp = tempfile::tempdir().unwrap();
    let fake = temp.path().join(format!(
        "status-not-loaded.{}",
        std::env::consts::EXE_EXTENSION
    ));
    fs::copy(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"), &fake).unwrap();

    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path().join("data"))
        .env("CODEX_ZECTRIX_CODEX_BIN", fake)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .ends_with("codex_quota_method=supported\ncodex_task_method=partial\n")
    );
}

#[test]
fn diagnostics_distinguish_source_freshness_without_exposing_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("data");
    fs::create_dir_all(&data_dir).unwrap();
    fs::write(
        data_dir.join("source-status.json"),
        r#"{"quota":"current","taskActivity":"inferred","radar":"stale","radarReason":"rate_limited","radarNextRequestAtEpochSeconds":1787544000,"arbitrary":"SECRET_PAYLOAD","etag":"SECRET_ETAG","forecastWindow":"SECRET_WINDOW","sourceUrl":"SECRET_URL"}"#,
    )
    .unwrap();

    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", &data_dir)
        .env("CODEX_ZECTRIX_CODEX_BIN", temp.path().join("missing-codex"))
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "quota_source=current\ntask_activity_source=inferred\nradar_source=stale\nradar_reason=rate_limited\nradar_next_request_at=1787544000\ncodex_quota_method=unavailable\ncodex_task_method=unavailable\n"
    );
    let diagnostics = [output.stdout, output.stderr].concat();
    for forbidden in [
        "SECRET_PAYLOAD",
        "SECRET_ETAG",
        "SECRET_WINDOW",
        "SECRET_URL",
    ] {
        assert!(
            !diagnostics
                .windows(forbidden.len())
                .any(|bytes| bytes == forbidden.as_bytes())
        );
    }
}

#[test]
fn diagnostics_report_unavailable_before_any_observation() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(common::dashboard_binary())
        .arg("diagnostics")
        .env("CODEX_ZECTRIX_DATA_DIR", temp.path().join("missing"))
        .env("CODEX_ZECTRIX_CODEX_BIN", temp.path().join("missing-codex"))
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "quota_source=unavailable\ntask_activity_source=unavailable\nradar_source=unavailable\nradar_reason=none\ncodex_quota_method=unavailable\ncodex_task_method=unavailable\n"
    );
}
