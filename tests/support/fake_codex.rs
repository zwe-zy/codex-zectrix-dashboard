use std::fs;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--sentinel-child") {
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }

    record_initial_job_membership_if_requested();
    record_processes_if_requested();
    serve_json_lines();
}

fn record_initial_job_membership_if_requested() {
    let marker = std::env::current_exe()
        .unwrap()
        .with_extension("started-in-job");
    if !marker.exists() {
        return;
    }

    #[cfg(windows)]
    let started_in_job = {
        use windows_sys::Win32::System::JobObjects::IsProcessInJob;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let mut in_job = 0;
        let observed =
            unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) };
        assert_ne!(observed, 0);
        in_job != 0
    };
    #[cfg(unix)]
    let started_in_job = true;

    fs::write(marker, started_in_job.to_string()).unwrap();
}

// The deliberately un-waited descendant is the leak that process ownership must catch.
#[allow(clippy::zombie_processes)]
fn record_processes_if_requested() {
    let executable = std::env::current_exe().unwrap();
    let record = executable.with_extension("process-record");
    if !record.exists() {
        return;
    }

    let descendant = Command::new(&executable)
        .arg("--sentinel-child")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fs::write(
        record,
        format!("{}\n{}\n", std::process::id(), descendant.id()),
    )
    .unwrap();
}

fn serve_json_lines() {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let response = match request["method"].as_str() {
            Some("initialize") => json!({
                "id": request["id"],
                "result": {
                    "userAgent": "codex-zectrix-dashboard/test",
                    "platformFamily": std::env::consts::FAMILY,
                    "platformOs": std::env::consts::OS,
                    "codexHome": null
                }
            }),
            Some("account/rateLimits/read")
                if scenario_is("status-quota-method") && has_quota_params(&request) =>
            {
                json!({
                "id": request["id"],
                "error": { "code": -32601, "message": "method not found" }
                })
            }
            Some("account/rateLimits/read")
                if scenario_is("status-quota-method-localized") && has_quota_params(&request) =>
            {
                json!({
                    "id": request["id"],
                    "error": { "code": -32601, "message": "方法不存在" }
                })
            }
            Some("account/rateLimits/read")
                if scenario_is("status-quota-method-lookalike") && has_quota_params(&request) =>
            {
                json!({
                    "id": request["id"],
                    "error": { "code": -32001, "message": "method not found" }
                })
            }
            Some("account/rateLimits/read")
                if scenario_is("status-quota-account") && has_quota_params(&request) =>
            {
                json!({
                "id": request["id"],
                "error": { "code": -32000, "message": "account denied" }
                })
            }
            Some("account/rateLimits/read") if has_quota_params(&request) => json!({
                "id": request["id"],
                "result": {
                    "rateLimits": {
                        "primary": {
                            "usedPercent": 24,
                            "windowDurationMins": 300,
                            "resetsAt": 1_900_000_000
                        },
                        "secondary": null
                    },
                    "rateLimitResetCredits": null
                }
            }),
            Some("thread/list")
                if has_thread_list_params(&request, None)
                    && scenario_is("status-malformed-list") =>
            {
                json!({
                    "id": request["id"],
                    "result": { "data": [{}], "nextCursor": null }
                })
            }
            Some("thread/list")
                if has_thread_list_params(&request, None) && scenario_is("status-empty-cursor") =>
            {
                json!({
                    "id": request["id"],
                    "result": { "data": [], "nextCursor": "" }
                })
            }
            Some("thread/list")
                if scenario_is("status-unique-cursors")
                    && unique_cursor_page(&request).is_some() =>
            {
                let page = unique_cursor_page(&request).unwrap();
                record_unique_cursor_request();
                if page == 65 {
                    json!({
                        "id": request["id"],
                        "result": serde_json::from_str::<Value>(include_str!("../../fixtures/thread-list-latest-top-level-page-two.json")).unwrap()
                    })
                } else {
                    json!({
                        "id": request["id"],
                        "result": { "data": [], "nextCursor": format!("unique-{page}") }
                    })
                }
            }
            Some("thread/list") if scenario_is("status-local-history") => json!({
                "id": request["id"],
                "result": { "data": [
                    { "id": "local-root", "parentThreadId": null, "updatedAt": 1, "status": { "type": "notLoaded" } }
                ], "nextCursor": null }
            }),
            Some("thread/list") if scenario_is("status-aggregate-failed") => json!({
                "id": request["id"],
                "result": {
                    "data": [
                        { "id": "top-level-1", "parentThreadId": null, "updatedAt": 1, "recencyAt": 1, "status": { "type": "active", "activeFlags": [] } },
                        { "id": "top-level-2", "parentThreadId": null, "updatedAt": 2, "recencyAt": 2, "status": { "type": "systemError" } }
                    ],
                    "nextCursor": null
                }
            }),
            Some("thread/list") if scenario_is("status-aggregate-running") => json!({
                "id": request["id"],
                "result": {
                    "data": [
                        { "id": "top-level-1", "parentThreadId": null, "updatedAt": 1, "recencyAt": 1, "status": { "type": "idle" } },
                        { "id": "top-level-2", "parentThreadId": null, "updatedAt": 2, "recencyAt": 2, "status": { "type": "active", "activeFlags": [] } }
                    ],
                    "nextCursor": null
                }
            }),
            Some("thread/list") if scenario_is("status-aggregate-completed") => json!({
                "id": request["id"],
                "result": {
                    "data": [
                        { "id": "top-level-1", "parentThreadId": null, "updatedAt": 1, "recencyAt": 1, "status": { "type": "idle" } },
                        { "id": "top-level-2", "parentThreadId": null, "updatedAt": 2, "recencyAt": 2, "status": { "type": "notLoaded" } }
                    ],
                    "nextCursor": null
                }
            }),
            Some("thread/list") if has_thread_list_params(&request, None) => json!({
                "id": request["id"],
                "result": serde_json::from_str::<Value>(include_str!("../../fixtures/thread-list-latest-top-level.json")).unwrap()
            }),
            Some("thread/list")
                if has_thread_list_params(&request, Some("page-two"))
                    && scenario_is("status-repeated-cursor") =>
            {
                let mut page = serde_json::from_str::<Value>(include_str!(
                    "../../fixtures/thread-list-latest-top-level-page-two.json"
                ))
                .unwrap();
                page["nextCursor"] = Value::String("page-two".to_owned());
                json!({ "id": request["id"], "result": page })
            }
            Some("thread/list") if has_thread_list_params(&request, Some("page-two")) => json!({
                "id": request["id"],
                "result": serde_json::from_str::<Value>(include_str!("../../fixtures/thread-list-latest-top-level-page-two.json")).unwrap()
            }),
            Some("thread/read")
                if has_thread_read_params(&request) && scenario_is("status-malformed-read") =>
            {
                json!({
                    "id": request["id"],
                    "result": serde_json::from_str::<Value>(include_str!("../../fixtures/thread-read-missing-turn-id.json")).unwrap()
                })
            }
            Some("thread/read")
                if has_thread_read_params(&request) && scenario_is("status-not-loaded") =>
            {
                json!({
                    "id": request["id"],
                    "result": serde_json::from_str::<Value>(include_str!("../../fixtures/thread-read-not-loaded.json")).unwrap()
                })
            }
            Some("thread/read") if has_thread_read_params(&request) => json!({
                "id": request["id"],
                "result": serde_json::from_str::<Value>(include_str!("../../fixtures/thread-read-completed.json")).unwrap()
            }),
            Some("initialized") => continue,
            _ => json!({
                "id": request["id"],
                "error": { "code": -1, "message": "unsupported test request" }
            }),
        };
        serde_json::to_writer(&mut stdout, &response).unwrap();
        stdout.write_all(b"\n").unwrap();
        stdout.flush().unwrap();
    }
}

fn has_thread_list_params(request: &Value, cursor: Option<&str>) -> bool {
    let Some(params) = exact_request_params(request).and_then(Value::as_object) else {
        return false;
    };
    has_thread_list_param_object(params, cursor)
}

fn has_thread_list_param_object(
    params: &serde_json::Map<String, Value>,
    cursor: Option<&str>,
) -> bool {
    let cursor_is_exact = match cursor {
        Some(cursor) => params.get("cursor").and_then(Value::as_str) == Some(cursor),
        None => !params.contains_key("cursor"),
    };
    let expected_keys = if cursor.is_some() {
        [
            "cursor",
            "limit",
            "sortKey",
            "sortDirection",
            "archived",
            "useStateDbOnly",
            "sourceKinds",
        ]
        .as_slice()
    } else {
        [
            "limit",
            "sortKey",
            "sortDirection",
            "archived",
            "useStateDbOnly",
            "sourceKinds",
        ]
        .as_slice()
    };
    cursor_is_exact
        && has_exact_keys(params, expected_keys)
        && params.get("limit") == Some(&json!(100))
        && params.get("sortKey") == Some(&json!("recency_at"))
        && params.get("sortDirection") == Some(&json!("desc"))
        && params.get("archived") == Some(&json!(false))
        && params.get("useStateDbOnly") == Some(&json!(true))
        && params.get("sourceKinds") == Some(&json!(["cli", "vscode", "appServer"]))
}

fn has_quota_params(request: &Value) -> bool {
    exact_request_params(request).is_some_and(Value::is_null)
}

fn has_thread_read_params(request: &Value) -> bool {
    let Some(params) = exact_request_params(request).and_then(Value::as_object) else {
        return false;
    };
    has_exact_keys(params, &["threadId", "includeTurns"])
        && params.get("threadId") == Some(&json!("top-level-2"))
        && params.get("includeTurns") == Some(&json!(true))
}

fn has_exact_keys(object: &serde_json::Map<String, Value>, expected: &[&str]) -> bool {
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}

fn exact_request_params(request: &Value) -> Option<&Value> {
    let request = request.as_object()?;
    has_exact_keys(request, &["id", "method", "params"]).then(|| &request["params"])
}

fn unique_cursor_page(request: &Value) -> Option<usize> {
    let params = exact_request_params(request)?.as_object()?;
    match params.get("cursor") {
        None if has_thread_list_param_object(params, None) => Some(1),
        Some(Value::String(cursor)) => {
            let page = cursor.strip_prefix("unique-")?.parse::<usize>().ok()?;
            has_thread_list_param_object(params, Some(cursor)).then_some(page.checked_add(1)?)
        }
        _ => None,
    }
}

fn record_unique_cursor_request() {
    let path = std::env::current_exe()
        .unwrap()
        .with_extension("request-count");
    let count = std::fs::read_to_string(&path)
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0)
        + 1;
    std::fs::write(path, count.to_string()).unwrap();
}

fn scenario_is(name: &str) -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .is_some_and(|stem| stem == name)
}
