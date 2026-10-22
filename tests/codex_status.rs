#[cfg(feature = "test-support")]
use std::io::{BufRead, BufReader, Write};
#[cfg(feature = "test-support")]
use std::process::{Command, Stdio};

use codex_zectrix_dashboard::{
    CodexStatusError, TaskState, parse_quota_status, parse_task_state, render_status_diagnostic,
};

#[cfg(feature = "test-support")]
use codex_zectrix_dashboard::{AppServerClient, CodexStatusSource};
#[cfg(feature = "test-support")]
use serde_json::{Value, json};

#[test]
fn malformed_other_bucket_keeps_codex_windows() {
    let value = r#"{"rateLimitsByLimitId":{"other":"malformed","codex":{"primary":{"usedPercent":24.5,"windowDurationMins":300},"secondary":{"usedPercent":30,"windowDurationMins":10080,"resetsAt":1900000000}}}}"#;
    let quota = parse_quota_status(value).unwrap();
    assert_eq!(quota.primary.unwrap().remaining_percent, 75.5);
    assert_eq!(quota.secondary.unwrap().remaining_percent, 70.0);
    assert_eq!(quota.primary.unwrap().resets_at_epoch_seconds, None);
}
#[test]
fn unrelated_bucket_is_never_a_codex_fallback() {
    assert!(parse_quota_status(r#"{"rateLimitsByLimitId":{"other":{"primary":{"usedPercent":24}}},"rateLimits":{"limitId":"other","primary":{"usedPercent":24}}}"#).is_err());
}
#[test]
#[cfg(feature = "test-support")]
fn not_loaded_never_means_completed() {
    let (_temp, source) = scenario_source("status-aggregate-completed");
    assert_ne!(
        source.read_latest_task_state().unwrap(),
        TaskState::Completed
    );
}

#[test]
fn ended_turns_require_their_own_timestamp_within_twenty_four_hours() {
    use codex_zectrix_dashboard::parse_task_state_at;
    let base = r#"{"thread":{"status":{"type":"idle"},"turns":[{"id":"t","items":[],"status":"completed","completedAt":100000}]}}"#;
    assert_eq!(
        parse_task_state_at(base, 100001).unwrap(),
        TaskState::Completed
    );
    assert_eq!(
        parse_task_state_at(base, 186400).unwrap(),
        TaskState::Unavailable
    );
    assert_eq!(
        parse_task_state_at(&base.replace("100000", "null"), 100001).unwrap(),
        TaskState::Unavailable
    );
}

#[test]
fn selects_codex_primary_bucket_and_computes_remaining() {
    // Selecting a different bucket/window or changing subtraction breaks these literals.
    let quota = parse_quota_status(include_str!("../fixtures/quota-multi-bucket.json")).unwrap();
    assert_eq!(quota.primary.unwrap().remaining_percent, 76.0);
    assert_eq!(
        quota.primary.unwrap().resets_at_epoch_seconds,
        Some(1_900_000_000)
    );
}

#[test]
fn quota_keeps_secondary_separate_and_accepts_the_unambiguous_legacy_bucket() {
    // Removing either fallback returns an incompatibility error instead of these values.
    let secondary = r#"{"rateLimitsByLimitId":{"codex":{"primary":null,"secondary":{"usedPercent":30,"resetsAt":1900000100}}}}"#;
    let legacy = r#"{"rateLimitsByLimitId":{},"rateLimits":{"primary":{"usedPercent":45,"resetsAt":1900000200}}}"#;
    assert!(parse_quota_status(secondary).unwrap().primary.is_none());
    assert_eq!(
        parse_quota_status(secondary)
            .unwrap()
            .secondary
            .unwrap()
            .remaining_percent,
        70.0
    );
    assert_eq!(
        parse_quota_status(legacy)
            .unwrap()
            .primary
            .unwrap()
            .resets_at_epoch_seconds,
        Some(1_900_000_200)
    );
}

#[test]
fn quota_rejects_malformed_numeric_and_missing_boundaries() {
    // Weakening a strict protocol boundary would accept at least one malformed response.
    for response in [
        r#"{"rateLimits":{"primary":{"usedPercent":-1,"resetsAt":1900000000}}}"#,
        r#"{"rateLimits":{"primary":{"usedPercent":101,"resetsAt":1900000000}}}"#,
        r#"{"rateLimits":{"primary":{"usedPercent":null,"resetsAt":1900000000}}}"#,
        r#"{"rateLimits":{"primary":{"resetsAt":1900000000}}}"#,
    ] {
        assert!(parse_quota_status(response).is_err(), "accepted {response}");
    }
}

#[test]
fn thread_status_and_latest_turn_statuses_map_independently() {
    // Each literal catches a different status branch, rather than coupling two branches.
    for (fixture, response, expected) in [
        (
            "running",
            include_str!("../fixtures/thread-read-running.json"),
            TaskState::Running,
        ),
        (
            "in-progress",
            include_str!("../fixtures/thread-read-in-progress.json"),
            TaskState::Running,
        ),
        (
            "completed",
            include_str!("../fixtures/thread-read-completed.json"),
            TaskState::Completed,
        ),
        (
            "failed",
            include_str!("../fixtures/thread-read-failed.json"),
            TaskState::Failed,
        ),
        (
            "interrupted",
            include_str!("../fixtures/thread-read-interrupted.json"),
            TaskState::Interrupted,
        ),
        (
            "system-error",
            include_str!("../fixtures/thread-read-system-error.json"),
            TaskState::Failed,
        ),
        (
            "empty",
            include_str!("../fixtures/thread-read-empty.json"),
            TaskState::Empty,
        ),
        (
            "multiple",
            include_str!("../fixtures/thread-read-multiple-turns.json"),
            TaskState::Completed,
        ),
        (
            "not-loaded",
            include_str!("../fixtures/thread-read-not-loaded.json"),
            TaskState::Unavailable,
        ),
    ] {
        assert_eq!(
            parse_task_state(response).unwrap_or_else(|error| panic!("{fixture}: {error}")),
            expected
        );
    }
}

#[test]
fn task_parser_rejects_unknown_or_missing_required_protocol_fields() {
    // Treating unsupported tags or missing nested fields as an ordinary state hides incompatibility.
    for response in [
        include_str!("../fixtures/thread-read-unknown-status.json"),
        include_str!("../fixtures/thread-read-missing-turn-id.json"),
        include_str!("../fixtures/thread-read-missing-turn-items.json"),
        r#"{"thread":{"status":{"type":"idle"}}}"#,
        r#"{"thread":{"turns":[]}}"#,
        r#"{"thread":{"status":{"type":"active"},"turns":[]}}"#,
    ] {
        assert!(matches!(
            parse_task_state(response),
            Err(CodexStatusError::Incompatible)
        ));
    }
}

#[cfg(feature = "test-support")]
#[test]
fn source_paginates_by_recency_and_ignores_subagents_or_missing_parent_fields() {
    // Dropping pagination, recency ordering, or explicit-null filtering returns Running instead.
    let program = std::path::PathBuf::from(env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"));
    let source = CodexStatusSource::new(AppServerClient::new(program));
    assert_eq!(
        source
            .read_quota_status()
            .unwrap()
            .primary
            .unwrap()
            .remaining_percent,
        76.0
    );
    assert_eq!(
        source.read_latest_task_state().unwrap(),
        TaskState::Unavailable
    );
}

#[cfg(feature = "test-support")]
#[test]
fn source_prioritizes_running_and_does_not_invent_completion_from_idle() {
    for (scenario, expected) in [
        ("status-aggregate-failed", TaskState::Running),
        ("status-aggregate-running", TaskState::Running),
        ("status-aggregate-completed", TaskState::Unavailable),
    ] {
        let (_temp, source) = scenario_source(scenario);
        assert_eq!(
            source.read_latest_task_state().unwrap(),
            expected,
            "{scenario}"
        );
    }
}

#[cfg(feature = "test-support")]
#[test]
fn source_rejects_malformed_pages_and_bad_pagination() {
    // Treating any malformed list response/cursor as data would hide an incompatible app-server.
    for scenario in [
        "status-malformed-list",
        "status-empty-cursor",
        "status-repeated-cursor",
    ] {
        let (_temp, source) = scenario_source(scenario);
        assert!(matches!(
            source.read_latest_task_state(),
            Err(CodexStatusError::Incompatible)
        ));
    }
}

#[cfg(feature = "test-support")]
#[test]
fn source_preserves_method_and_account_rpc_categories() {
    // Collapsing the app-server category loses the safe diagnostic distinction.
    let (_temp, source) = scenario_source("status-quota-method");
    assert!(matches!(
        source.read_quota_status(),
        Err(CodexStatusError::MethodUnavailable)
    ));
    let (_temp, source) = scenario_source("status-quota-account");
    assert!(matches!(
        source.read_quota_status(),
        Err(CodexStatusError::AccountUnavailable)
    ));
}

#[cfg(feature = "test-support")]
#[test]
fn source_uses_json_rpc_method_not_found_code_not_error_text() {
    // Reverting to English-message matching misclassifies either of these real RPC outcomes.
    let (_temp, source) = scenario_source("status-quota-method-localized");
    assert!(matches!(
        source.read_quota_status(),
        Err(CodexStatusError::MethodUnavailable)
    ));

    let (_temp, source) = scenario_source("status-quota-method-lookalike");
    assert!(matches!(
        source.read_quota_status(),
        Err(CodexStatusError::Unavailable)
    ));
}

#[cfg(feature = "test-support")]
#[test]
fn source_stops_after_sixty_four_unique_page_cursors() {
    // Removing the finite page budget requests page 65 and returns the fake's Completed status.
    let (temp, program) = scenario_program("status-unique-cursors");
    let request_count = program.with_extension("request-count");
    let source = CodexStatusSource::new(AppServerClient::new(program));

    assert!(matches!(
        source.read_latest_task_state(),
        Err(CodexStatusError::Incompatible)
    ));
    assert_eq!(std::fs::read_to_string(request_count).unwrap(), "64");
    drop(temp);
}

#[cfg(feature = "test-support")]
#[test]
fn fake_rejects_surplus_quota_list_and_read_parameters() {
    // Loosening any fake predicate would make an invalid source request look supported.
    let (_temp, program) = scenario_program("status-quota-method");
    let mut child = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    for request in [
        json!({ "id": 1, "method": "initialize", "params": { "clientInfo": { "name": "test", "version": "test" } } }),
        json!({ "method": "initialized" }),
    ] {
        serde_json::to_writer(&mut stdin, &request).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        if request.get("id").is_some() {
            let mut response = String::new();
            stdout.read_line(&mut response).unwrap();
        }
    }

    for request in [
        json!({ "id": 2, "method": "account/rateLimits/read", "params": { "surplus": true } }),
        json!({ "id": 3, "method": "thread/list", "params": { "limit": 100, "sortKey": "recency_at", "sortDirection": "desc", "archived": false, "useStateDbOnly": true, "surplus": true } }),
        json!({ "id": 4, "method": "thread/read", "params": { "threadId": "top-level-2", "includeTurns": true, "surplus": true } }),
        json!({ "id": 5, "method": "account/rateLimits/read", "params": null, "surplus": true }),
        json!({ "id": 6, "method": "thread/list", "params": { "limit": 100, "sortKey": "recency_at", "sortDirection": "desc", "archived": false, "useStateDbOnly": true }, "surplus": true }),
        json!({ "id": 7, "method": "thread/read", "params": { "threadId": "top-level-2", "includeTurns": true }, "surplus": true }),
    ] {
        serde_json::to_writer(&mut stdin, &request).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        let mut response = String::new();
        stdout.read_line(&mut response).unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["error"]["code"], -1);
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn public_statuses_diagnostics_and_errors_do_not_render_private_response_content() {
    // Adding a raw response field to any public output path leaks a sentinel.
    let quota = parse_quota_status(include_str!("../fixtures/quota-multi-bucket.json")).unwrap();
    let task = parse_task_state(include_str!("../fixtures/thread-read-completed.json")).unwrap();
    let error =
        parse_task_state(include_str!("../fixtures/thread-read-unknown-status.json")).unwrap_err();
    let output = [
        serde_json::to_vec(&quota).unwrap(),
        serde_json::to_vec(&task).unwrap(),
        format!("{quota:?}{task:?}{error}{error:?}").into_bytes(),
        render_status_diagnostic(Some(&quota), task).into_bytes(),
    ]
    .concat();
    for sentinel in [
        "TITLE_SENTINEL",
        "PREVIEW_SENTINEL",
        "TURN_ITEM_SENTINEL",
        "PROMPT_SENTINEL",
        "PROJECT_PATH_SENTINEL",
        "ACCOUNT_SENTINEL",
        "top-level-2",
    ] {
        assert!(
            !output
                .windows(sentinel.len())
                .any(|bytes| bytes == sentinel.as_bytes()),
            "public output leaked {sentinel}"
        );
    }
}

#[cfg(feature = "test-support")]
fn scenario_source(scenario: &str) -> (tempfile::TempDir, CodexStatusSource) {
    let (temp, program) = scenario_program(scenario);
    (temp, CodexStatusSource::new(AppServerClient::new(program)))
}

#[cfg(feature = "test-support")]
fn scenario_program(scenario: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let program = temp
        .path()
        .join(format!("{scenario}.{}", std::env::consts::EXE_EXTENSION));
    std::fs::copy(
        env!("CARGO_BIN_EXE_codex-zectrix-test-app-server"),
        &program,
    )
    .unwrap();
    (temp, program)
}
