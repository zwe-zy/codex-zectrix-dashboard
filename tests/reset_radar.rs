use codex_zectrix_dashboard::{
    ActivityState, DashboardConfig, ObservedDashboardState, ObservedQuota, ObservedQuotaWindow,
    ObservedTask, RadarCache, RadarClient, RadarDisplayState, RadarFetch, RadarReason, ResetKind,
    TaskActivityAvailability, parse_reset_radar_status, render_dashboard,
    render_normalized_dashboard_with_sync,
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use time::{OffsetDateTime, format_description::well_known::Rfc2822};

const NOW: i64 = 1_787_536_800;
const WATCH_EXPIRES_AT: i64 = 1_787_540_400;

fn status(latest_reset: &str, active_watch: &str) -> String {
    format!(
        r#"{{"data":{{"latest_reset":{latest_reset},"active_watch":{active_watch},"stats":{{"SECRET_STATS":"SECRET_STATS"}}}},"meta":{{"generated_at":"SECRET_GENERATED"}},"SECRET_ADDITIVE":"SECRET_ADDITIVE"}}"#,
    )
}

#[test]
fn explicit_no_watch_is_fresh_in_memory_but_not_after_restart() {
    let parsed = parse_reset_radar_status(&status("null", "null")).unwrap();
    let mut cache = RadarCache::default();

    cache.accept(parsed, NOW);

    assert_eq!(cache.display(NOW), RadarDisplayState::NoActiveWatch);
    assert_eq!(
        cache.display(NOW + 2 * 60 * 60),
        RadarDisplayState::NoActiveWatch
    );
    cache.mark_failed();
    assert_eq!(
        cache.display(NOW + 2 * 60 * 60),
        RadarDisplayState::NoActiveWatch
    );
    assert_eq!(
        cache.display(NOW + 2 * 60 * 60 + 1),
        RadarDisplayState::Unavailable
    );
    let persisted = serde_json::to_vec(cache.semantic()).unwrap();
    let restored = RadarCache::new(serde_json::from_slice(&persisted).unwrap());
    assert_eq!(restored.display(NOW), RadarDisplayState::Unavailable);
    let serialized = String::from_utf8(persisted).unwrap();
    for forbidden in [
        "SECRET_STATS",
        "SECRET_GENERATED",
        "SECRET_ADDITIVE",
        "text",
        "source",
    ] {
        assert!(!serialized.contains(forbidden), "persisted {forbidden}");
    }
}

#[test]
fn newer_valid_watch_precedes_a_confirmation_and_expires_intrinsically() {
    let latest_reset = r#"{"reset_type":"banked","announced_at":"2026-08-24T00:00:00Z","text":"SECRET_RESET_TEXT","source":{"url":"SECRET_RESET_URL"}}"#;
    let watch = r#"{"level":"strong","reset_chance_percent":70,"forecast_window":"today 12:00-15:00 UTC","observed_at":"2026-08-24T01:00:00Z","expires_at":"2026-08-24T03:00:00Z","text":"SECRET_WATCH_TEXT","source":{"url":"SECRET_WATCH_URL"}}"#;
    let parsed = parse_reset_radar_status(&status(latest_reset, watch)).unwrap();
    assert!(!parsed.has_invalid_capability());
    let mut cache = RadarCache::default();

    cache.accept(parsed, NOW);

    let persisted = serde_json::to_string(cache.semantic()).unwrap();
    for forbidden in [
        "SECRET_RESET_TEXT",
        "SECRET_RESET_URL",
        "SECRET_WATCH_TEXT",
        "SECRET_WATCH_URL",
        "SECRET_STATS",
        "SECRET_GENERATED",
        "SECRET_ADDITIVE",
    ] {
        assert!(!persisted.contains(forbidden), "persisted {forbidden}");
    }

    assert_eq!(
        cache.display(NOW),
        RadarDisplayState::ActiveWatch {
            forecast_window: "today 12:00-15:00 UTC".into(),
            reset_chance_percent: Some(70),
            stale: false,
        }
    );
    assert_eq!(
        cache.display(1_787_540_401),
        RadarDisplayState::Confirmed {
            kind: ResetKind::ResetCredit,
            stale: false,
        }
    );
}

#[test]
fn cached_watch_is_unavailable_at_its_intrinsic_expiry() {
    let watch = r#"{"level":"elevated","reset_chance_percent":null,"forecast_window":"today 12:00-15:00 UTC","observed_at":"2026-08-24T01:00:00Z","expires_at":"2026-08-24T03:00:00Z"}"#;
    let parsed = parse_reset_radar_status(&status("null", watch)).unwrap();
    let mut cache = RadarCache::default();

    cache.accept(parsed, NOW);
    cache.mark_failed();

    assert_eq!(
        cache.display(WATCH_EXPIRES_AT - 1),
        RadarDisplayState::ActiveWatch {
            forecast_window: "today 12:00-15:00 UTC".into(),
            reset_chance_percent: None,
            stale: true,
        }
    );
    assert_eq!(
        cache.display(WATCH_EXPIRES_AT),
        RadarDisplayState::Unavailable
    );
}

#[test]
fn confirmation_expires_after_its_announcement_window() {
    let latest_reset = r#"{"reset_type":"regular","announced_at":"2026-08-24T00:00:00Z"}"#;
    let parsed = parse_reset_radar_status(&status(latest_reset, "null")).unwrap();
    let mut cache = RadarCache::default();

    cache.accept(parsed, NOW);

    assert_eq!(
        cache.display(1_787_529_600 + 24 * 60 * 60 - 1),
        RadarDisplayState::Confirmed {
            kind: ResetKind::Regular,
            stale: false,
        }
    );
    assert_eq!(
        cache.display(1_787_529_600 + 24 * 60 * 60),
        RadarDisplayState::Unavailable
    );
}

#[test]
fn watch_observed_before_a_later_confirmation_cannot_override_it() {
    let latest_reset = r#"{"reset_type":"regular","announced_at":"2026-08-24T01:00:00Z"}"#;
    let watch = r#"{"level":"strong","reset_chance_percent":70,"forecast_window":"today 12:00-15:00 UTC","observed_at":"2026-08-24T00:30:00Z","expires_at":"2026-08-24T03:00:00Z"}"#;
    let parsed = parse_reset_radar_status(&status(latest_reset, watch)).unwrap();
    let mut cache = RadarCache::default();

    cache.accept(parsed, NOW);

    assert_eq!(
        cache.display(NOW),
        RadarDisplayState::Confirmed {
            kind: ResetKind::Regular,
            stale: false,
        }
    );
}

#[test]
fn unknown_confirmation_does_not_disable_an_independent_valid_watch() {
    let latest_reset = r#"{"reset_type":"future_kind","announced_at":"2026-08-24T01:00:00Z"}"#;
    let watch = r#"{"level":"strong","reset_chance_percent":70,"forecast_window":"today 12:00-15:00 UTC","observed_at":"2026-08-24T01:30:00Z","expires_at":"2026-08-24T03:00:00Z"}"#;
    let parsed = parse_reset_radar_status(&status(latest_reset, watch)).unwrap();
    assert!(parsed.has_invalid_capability());
    let mut cache = RadarCache::default();

    cache.accept(parsed, NOW);

    assert_eq!(
        cache.display(NOW),
        RadarDisplayState::ActiveWatch {
            forecast_window: "today 12:00-15:00 UTC".into(),
            reset_chance_percent: Some(70),
            stale: false,
        }
    );
}

#[test]
fn invalid_watch_fields_are_not_reinterpreted_as_no_active_watch() {
    for invalid_watch in [
        r#"{"level":"strong","reset_chance_percent":101,"forecast_window":"soon","observed_at":"2026-08-24T01:00:00Z","expires_at":"2026-08-24T03:00:00Z"}"#,
        r#"{"level":"strong","reset_chance_percent":70,"forecast_window":"   ","observed_at":"2026-08-24T01:00:00Z","expires_at":"2026-08-24T03:00:00Z"}"#,
        r#"{"level":"strong","reset_chance_percent":70,"forecast_window":"soon","observed_at":"not-a-timestamp","expires_at":"2026-08-24T03:00:00Z"}"#,
        r#"{"level":"strong","reset_chance_percent":70,"forecast_window":"soon","observed_at":"2026-08-24T03:00:00Z","expires_at":"2026-08-24T03:00:00Z"}"#,
    ] {
        let parsed = parse_reset_radar_status(&status("null", invalid_watch)).unwrap();
        assert!(parsed.has_invalid_capability());
        let mut cache = RadarCache::default();

        cache.accept(parsed, NOW);

        assert_eq!(cache.display(NOW), RadarDisplayState::Unavailable);
    }
}

fn dashboard_state(radar: RadarDisplayState) -> ObservedDashboardState {
    ObservedDashboardState {
        quota: ObservedQuota {
            windows: vec![ObservedQuotaWindow {
                name: "5 小时".into(),
                used_percent: 37,
                resets_at_epoch_seconds: NOW + 7_200,
            }],
            reset_credits: 0,
            stale: false,
        },
        radar,
        task_activity_availability: TaskActivityAvailability::Available,
        task_activity_stale: false,
        tasks: (1..=3)
            .map(|index| ObservedTask::new(format!("任务 {index}"), ActivityState::Running, NOW))
            .collect(),
        prompt: None,
        response: None,
        reasoning: None,
        project_path: None,
        tool: None,
        error_text: None,
        plan: None,
    }
}

fn assert_full_ruler(pixels: &[u8]) {
    assert!((0..10).all(|cell| {
        let x = 18 + cell * 37;
        pixels[155 * 400 + x] == 0
    }));
}

#[test]
fn no_watch_renders_an_empty_ten_cell_ruler_and_preserves_three_task_rows() {
    let output = render_dashboard(
        dashboard_state(RadarDisplayState::NoActiveWatch),
        NOW,
        DashboardConfig::default(),
    )
    .unwrap();

    assert_eq!((output.frame.width, output.frame.height), (400, 300));
    assert!((14..386).all(|x| output.frame.pixels[124 * 400 + x] == 0));
    assert!((14..386).all(|x| output.frame.pixels[168 * 400 + x] == 0));
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "暂无活跃预测")
    );
    for task in ["任务 1", "任务 2", "任务 3"] {
        assert!(output.visible_text.iter().any(|text| text == task));
    }
    assert!((0..10).all(|cell| {
        let x = 18 + cell * 37;
        output.frame.pixels[155 * 400 + x] == 255
    }));
}

#[test]
fn watch_probability_fills_the_ruler_and_footer_names_screen_updates() {
    let normalized = codex_zectrix_dashboard::normalize_dashboard(
        dashboard_state(RadarDisplayState::ActiveWatch {
            forecast_window: "a deliberately long forecast window that must stay clear".into(),
            reset_chance_percent: Some(70),
            stale: false,
        }),
        NOW,
        &DashboardConfig::default(),
    );
    let output = render_normalized_dashboard_with_sync(
        normalized,
        NOW,
        DashboardConfig::default(),
        Some(NOW),
    )
    .unwrap();

    assert!(output.visible_text.iter().any(|text| text == "70%"));
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text.starts_with("画面更新 "))
    );
    assert!((0..7).all(|cell| {
        let x = 18 + cell * 33;
        output.frame.pixels[155 * 400 + x] == 0
    }));
    assert!((7..10).all(|cell| {
        let x = 18 + cell * 33;
        output.frame.pixels[155 * 400 + x] == 255
    }));
    let probability_rows = (145..168)
        .filter(|y| (342..386).any(|x| output.frame.pixels[y * 400 + x] == 0))
        .collect::<Vec<_>>();
    assert_eq!(probability_rows, (151..=159).collect::<Vec<_>>());
}

#[test]
fn watch_without_probability_keeps_the_ruler_empty() {
    let output = render_dashboard(
        dashboard_state(RadarDisplayState::ActiveWatch {
            forecast_window: "today 12:00-15:00 UTC".into(),
            reset_chance_percent: None,
            stale: false,
        }),
        NOW,
        DashboardConfig::default(),
    )
    .unwrap();
    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "today 12:00-15:00 UTC")
    );
    assert!((0..10).all(|cell| {
        let x = 18 + cell * 37;
        output.frame.pixels[155 * 400 + x] == 255
    }));
}

#[test]
fn long_watch_window_clips_without_an_ellipsis_before_probability() {
    let render = |window: &str| {
        render_dashboard(
            dashboard_state(RadarDisplayState::ActiveWatch {
                forecast_window: window.into(),
                reset_chance_percent: Some(70),
                stale: false,
            }),
            NOW,
            DashboardConfig::default(),
        )
        .unwrap()
        .frame
    };

    let long =
        render("forecast window deliberately stays on one line and clips before probability first");
    let clipped = render(
        "forecast window deliberately stays on one line and clips before probability second",
    );

    assert_eq!(long.pixels, clipped.pixels);
}

#[test]
fn cached_watch_staleness_does_not_add_transport_text_to_the_screen() {
    let stale = render_dashboard(
        dashboard_state(RadarDisplayState::ActiveWatch {
            forecast_window: "today 12:00-15:00 UTC".into(),
            reset_chance_percent: None,
            stale: true,
        }),
        NOW,
        DashboardConfig::default(),
    )
    .unwrap();
    let current = render_dashboard(
        dashboard_state(RadarDisplayState::ActiveWatch {
            forecast_window: "today 12:00-15:00 UTC".into(),
            reset_chance_percent: None,
            stale: false,
        }),
        NOW,
        DashboardConfig::default(),
    )
    .unwrap();

    assert!(!stale.visible_text.iter().any(|text| text == "旧"));
    assert!(
        stale
            .visible_text
            .iter()
            .any(|text| text == "today 12:00-15:00 UTC")
    );
    assert_eq!(stale.frame.pixels, current.frame.pixels);
}

#[test]
fn regular_confirmation_renders_its_label_with_a_full_ruler() {
    let output = render_dashboard(
        dashboard_state(RadarDisplayState::Confirmed {
            kind: ResetKind::Regular,
            stale: false,
        }),
        NOW,
        DashboardConfig::default(),
    )
    .unwrap();

    assert!(
        output
            .visible_text
            .iter()
            .any(|text| text == "普通重置已确认")
    );
    assert_full_ruler(&output.frame.pixels);
}

#[test]
fn reset_credit_confirmation_is_distinct_from_the_official_reset_credit_balance() {
    let mut state = dashboard_state(RadarDisplayState::Confirmed {
        kind: ResetKind::ResetCredit,
        stale: false,
    });
    state.quota.reset_credits = 2;

    let output = render_dashboard(state, NOW, DashboardConfig::default()).unwrap();

    for expected in ["重置额度已确认", "重置额度 2"] {
        assert!(
            output.visible_text.iter().any(|text| text == expected),
            "missing {expected}"
        );
    }
    assert_eq!(output.normalized.quota.reset_credits, 2);
    assert_full_ruler(&output.frame.pixels);
}

#[test]
fn radar_http_client_bounds_bodies_and_keeps_etags_only_in_memory() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v1/status", listener.local_addr().unwrap());
    let (sender, receiver) = mpsc::channel();
    let body = status("null", "null");
    thread::spawn(move || {
        let mut heads = Vec::new();
        for response in [
            format!(
                "HTTP/1.1 200 OK\r\nETag: radar-v1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
            "HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".into(),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            heads.push(String::from_utf8(request).unwrap().to_ascii_lowercase());
            stream.write_all(response.as_bytes()).unwrap();
        }
        sender.send(heads).unwrap();
    });

    let mut client = RadarClient::new(endpoint).unwrap();
    assert!(matches!(client.fetch(), RadarFetch::Modified(_)));
    assert_eq!(client.fetch(), RadarFetch::NotModified);
    let heads = receiver.recv().unwrap();
    assert!(!heads[0].contains("if-none-match"));
    assert!(heads[1].contains("if-none-match: radar-v1"));
}

#[test]
fn radar_http_client_rejects_an_unconditional_not_modified_response() {
    let mut client =
        radar_client_responding_with(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");

    assert_eq!(
        client.fetch(),
        RadarFetch::Failed {
            reason: RadarReason::InvalidResponse,
            retry_after_seconds: None,
        }
    );
}

#[test]
fn radar_http_client_rejects_an_unbounded_oversized_body() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v1/status", listener.local_addr().unwrap());
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
            .unwrap();
        stream.write_all(&vec![b'x'; 65_537]).unwrap();
    });

    let mut client = RadarClient::new(endpoint).unwrap();
    assert_eq!(
        client.fetch(),
        RadarFetch::Failed {
            reason: RadarReason::ResponseTooLarge,
            retry_after_seconds: None,
        }
    );
}

#[test]
fn radar_http_client_rejects_an_invalid_response_without_exposing_it() {
    let mut client = radar_client_responding_with(
        b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\nConnection: close\r\n\r\nSECRET_INVALID_RESPONSE",
    );
    let result = client.fetch();
    assert_eq!(
        result,
        RadarFetch::Failed {
            reason: RadarReason::InvalidResponse,
            retry_after_seconds: None,
        }
    );
    assert!(!format!("{result:?}").contains("SECRET_INVALID_RESPONSE"));
}

#[test]
fn radar_http_client_bounds_a_response_timeout_as_network_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v1/status", listener.local_addr().unwrap());
    let (accepted, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        accepted.send(()).unwrap();
        thread::sleep(Duration::from_secs(6));
    });

    let mut client = RadarClient::new(endpoint).unwrap();
    let started_at = std::time::Instant::now();
    let result = client.fetch();
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();

    assert_eq!(
        result,
        RadarFetch::Failed {
            reason: RadarReason::Network,
            retry_after_seconds: None,
        }
    );
    assert!(started_at.elapsed() < Duration::from_secs(6));
}

#[test]
fn radar_http_client_reports_rate_limits_without_exposing_the_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v1/status", listener.local_addr().unwrap());
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(
                b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7200\r\nContent-Length: 18\r\nConnection: close\r\n\r\nSECRET_RATE_LIMIT",
            )
            .unwrap();
    });

    let mut client = RadarClient::new(endpoint).unwrap();
    assert_eq!(
        client.fetch(),
        RadarFetch::Failed {
            reason: RadarReason::RateLimited,
            retry_after_seconds: Some(7_200),
        }
    );
}

#[test]
fn radar_http_client_honors_an_http_date_retry_after() {
    let retry_at = OffsetDateTime::parse("Wed, 21 Oct 2099 07:28:00 GMT", &Rfc2822)
        .unwrap()
        .unix_timestamp();
    let observed_at = OffsetDateTime::now_utc().unix_timestamp();
    let mut client = radar_client_responding_with(
        b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: Wed, 21 Oct 2099 07:28:00 GMT\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    let RadarFetch::Failed {
        reason: RadarReason::RateLimited,
        retry_after_seconds: Some(retry_after_seconds),
    } = client.fetch()
    else {
        panic!("HTTP-date Retry-After was not honored");
    };
    assert!(retry_after_seconds > 60 * 60);
    assert!(observed_at.saturating_add_unsigned(retry_after_seconds) >= retry_at);
}

#[test]
fn missing_status_evidence_is_invalid() {
    assert!(parse_reset_radar_status(r#"{"data":{"latest_reset":null}}"#).is_err());
}

fn radar_client_responding_with(response: &'static [u8]) -> RadarClient {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/api/v1/status", listener.local_addr().unwrap());
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).unwrap();
        stream.write_all(response).unwrap();
    });
    RadarClient::new(endpoint).unwrap()
}
