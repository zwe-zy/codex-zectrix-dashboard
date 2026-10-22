use codex_zectrix_dashboard::inkcards::{
    InkcardCoordinator, InkcardMode, InkcardSettings, InkcardState, PagePublisher,
};
use codex_zectrix_dashboard::status_board::{BoardTaskState, StatusBoardState};
use codex_zectrix_dashboard::{
    Settings, StatusFrameAttempt, StatusFramePublishError, StatusFramePublisherState,
    StatusFrameRenderError, StatusFrameRenderer, UploadCompletion,
};

struct Renderer;
impl StatusFrameRenderer for Renderer {
    fn render(
        &mut self,
        state: &StatusBoardState,
        _: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError> {
        Ok(serde_json::to_vec(state).unwrap())
    }
}
#[derive(Default)]
struct Cloud {
    now: i64,
    clock: Option<std::rc::Rc<std::cell::Cell<i64>>>,
    fail: Option<(u8, StatusFramePublishError)>,
    sent: Vec<(u8, serde_json::Value)>,
}
impl PagePublisher for Cloud {
    fn publish_page(
        &mut self,
        page: u8,
        png: &[u8],
    ) -> Result<UploadCompletion, StatusFramePublishError> {
        self.sent.push((page, serde_json::from_slice(png).unwrap()));
        if let Some(clock) = &self.clock {
            if page < 5 {
                clock.set(clock.get() + 30);
            }
            self.now = clock.get();
        }
        if self.fail.as_ref().is_some_and(|(p, _)| *p == page) {
            return Err(self.fail.take().unwrap().1);
        }
        Ok(UploadCompletion {
            epoch_seconds: self.now,
        })
    }
}
impl codex_zectrix_dashboard::StatusFramePublisher for Cloud {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        self.publish_page(3, png)
    }
}
fn settings() -> Settings {
    Settings {
        device_id: "fixture-device".into(),
        page_id: 3,
    }
}
fn source() -> StatusBoardState {
    StatusBoardState {
        task_state: BoardTaskState::Running,
        quota_remaining_percent: Some(70.0),
        quota_window_duration_mins: Some(10080),
        ..Default::default()
    }
}
fn coordinator() -> InkcardCoordinator {
    InkcardCoordinator::new(&settings(), &StatusFramePublisherState::default(), None).unwrap()
}
fn publish(
    c: &mut InkcardCoordinator,
    cloud: &mut Cloud,
    now: i64,
) -> Vec<(u8, StatusFrameAttempt)> {
    cloud.now = now;
    c.publish_due(|| Some(now), &mut Renderer, cloud, || true, |_| true)
        .unwrap()
}

#[test]
fn fixed_pages_share_live_windows_and_restart_does_not_repeat_unchanged_content() {
    let mut c = coordinator();
    let mut cloud = Cloud::default();
    c.observe(source());
    assert!(
        publish(&mut c, &mut cloud, 100)
            .iter()
            .all(|(_, a)| *a == StatusFrameAttempt::Published)
    );
    for ((page, payload), (expected_page, theme)) in cloud.sent.iter().zip([
        (1, "pop-cut"),
        (2, "eva"),
        (3, "pixel-console"),
        (4, "space-gauge"),
        (5, "cyber-hud"),
    ]) {
        assert_eq!(*page, expected_page);
        assert_eq!(payload["theme"], theme);
        assert_eq!(payload["quota_window_duration_mins"], 10080);
        assert_eq!(payload["quota_remaining_percent"], 70.0);
    }
    assert_eq!(cloud.sent.len(), 5);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inkcard-state.json");
    c.snapshot().save_atomically(&path).unwrap();
    let mut c = InkcardCoordinator::new(
        &settings(),
        &StatusFramePublisherState::default(),
        InkcardState::load(&path).unwrap(),
    )
    .unwrap();
    c.observe(source());
    publish(&mut c, &mut cloud, 159);
    assert_eq!(cloud.sent.len(), 5);
    publish(&mut c, &mut cloud, 160);
    assert_eq!(
        cloud.sent.len(),
        10,
        "exactly one timestamp followup per page"
    );
    publish(&mut c, &mut cloud, 220);
    assert_eq!(cloud.sent.len(), 10);
    assert_eq!(c.last_content_upload(), Some(100));
}

#[test]
fn a_failed_page_retries_without_resending_successful_content() {
    let mut c = coordinator();
    let mut cloud = Cloud {
        fail: Some((3, StatusFramePublishError::TransientUnavailable)),
        ..Default::default()
    };
    c.observe(source());
    publish(&mut c, &mut cloud, 100);
    assert_eq!(
        c.snapshot().pages[2].last_successful_content_fingerprint,
        None
    );
    assert_eq!(
        c.last_content_upload(),
        None,
        "not all five pages have succeeded"
    );
    assert!(
        c.snapshot().pages[4]
            .last_successful_content_fingerprint
            .is_some(),
        "later pages must still upload"
    );
    publish(&mut c, &mut cloud, 160);
    cloud.sent.clear();
    publish(&mut c, &mut cloud, 220);
    assert_eq!(cloud.sent.iter().map(|x| x.0).collect::<Vec<_>>(), vec![3]);
    assert!(
        c.snapshot()
            .pages
            .iter()
            .all(|p| p.consecutive_failure_count() == 0)
    );
}

#[test]
fn slow_earlier_pages_do_not_shorten_the_later_page_retry_interval() {
    let clock = std::rc::Rc::new(std::cell::Cell::new(100));
    let mut cloud = Cloud {
        clock: Some(clock.clone()),
        fail: Some((5, StatusFramePublishError::TransientUnavailable)),
        ..Default::default()
    };
    let mut c = coordinator();
    c.observe(source());
    c.publish_due(
        || Some(clock.get()),
        &mut Renderer,
        &mut cloud,
        || true,
        |_| true,
    )
    .unwrap();
    assert_eq!(clock.get(), 220);
    assert_eq!(
        c.snapshot().pages[4].next_allowed_attempt_epoch_seconds,
        Some(280)
    );
}

#[test]
fn rejected_key_stops_all_pages_and_remains_stopped_after_restart() {
    let mut c = coordinator();
    let mut cloud = Cloud {
        fail: Some((2, StatusFramePublishError::CredentialsRejected)),
        ..Default::default()
    };
    c.observe(source());
    publish(&mut c, &mut cloud, 100);
    assert_eq!(cloud.sent.len(), 2);
    let mut c = InkcardCoordinator::new(
        &settings(),
        &StatusFramePublisherState::default(),
        Some(c.snapshot()),
    )
    .unwrap();
    c.observe(source());
    publish(&mut c, &mut cloud, 1000);
    assert_eq!(cloud.sent.len(), 2);
}

#[test]
fn new_device_or_new_setup_seed_invalidates_all_cached_pages() {
    let mut c = coordinator();
    let mut cloud = Cloud::default();
    c.observe(source());
    publish(&mut c, &mut cloud, 100);
    publish(&mut c, &mut cloud, 160);
    let saved = c.snapshot();
    let mut other = settings();
    other.device_id = "new-device".into();
    let seed = StatusFramePublisherState {
        last_successful_http_upload_epoch_seconds: Some(300),
        ..Default::default()
    };
    for (route, seed) in [
        (other, StatusFramePublisherState::default()),
        (settings(), seed),
    ] {
        let mut next = InkcardCoordinator::new(&route, &seed, Some(saved.clone())).unwrap();
        next.observe(source());
        cloud.sent.clear();
        publish(&mut next, &mut cloud, 1000);
        assert_eq!(cloud.sent.len(), 5);
    }
}

#[test]
fn maintenance_can_interrupt_between_pages_and_resume_the_rest() {
    let mut c = coordinator();
    c.observe(source());
    let mut cloud = Cloud {
        now: 100,
        ..Default::default()
    };
    let mut checks = 0;
    c.publish_due(
        || Some(100),
        &mut Renderer,
        &mut cloud,
        || {
            checks += 1;
            checks <= 2
        },
        |_| true,
    )
    .unwrap();
    assert_eq!(cloud.sent.len(), 2);
    publish(&mut c, &mut cloud, 101);
    assert_eq!(cloud.sent.len(), 5);
    assert_eq!(
        cloud.sent.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
}

#[test]
fn missing_mode_keeps_single_page_and_corrupt_state_is_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let mode = dir.path().join("inkcards.json");
    assert_eq!(
        InkcardSettings::load(&mode).unwrap().mode,
        InkcardMode::Single
    );
    assert!(!mode.exists());
    InkcardSettings {
        mode: InkcardMode::All,
        pages: None,
    }
    .save_atomically(&mode)
    .unwrap();
    assert_eq!(InkcardSettings::load(&mode).unwrap().mode, InkcardMode::All);
    let state = dir.path().join("inkcard-state.json");
    std::fs::write(&state, b"broken").unwrap();
    assert!(InkcardState::load(&state).is_err());
    assert_eq!(std::fs::read(&state).unwrap(), b"broken");
}

#[test]
fn custom_inkcard_layout_roundtrips_and_rejects_ambiguous_routes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inkcards.json");
    let value = serde_json::json!({"mode":"all","pages":[
        {"page":1,"theme":"cyber-hud"}, {"page":2,"theme":"pop-cut"}
    ]});
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let loaded = InkcardSettings::load(&path).expect("custom ordering must load");
    loaded.save_atomically(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),
        value
    );
    for pages in [
        serde_json::json!([]),
        serde_json::json!([{"page":0,"theme":"eva"}]),
        serde_json::json!([{"page":6,"theme":"eva"}]),
        serde_json::json!([{"page":1,"theme":"eva"},{"page":1,"theme":"pop-cut"}]),
    ] {
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"mode":"all","pages":pages})).unwrap(),
        )
        .unwrap();
        assert!(InkcardSettings::load(&path).is_err());
    }
}

#[test]
fn custom_pages_publish_only_selected_routes_and_keep_cooldowns_after_reorder() {
    use codex_zectrix_dashboard::{inkcards::InkcardPage, status_board::BoardTheme::*};
    let layout = [
        InkcardPage {
            page: 1,
            theme: CyberHud,
        },
        InkcardPage {
            page: 2,
            theme: PopCut,
        },
    ];
    let mut c = InkcardCoordinator::with_pages(
        &settings(),
        &StatusFramePublisherState::default(),
        None,
        &layout,
    )
    .unwrap();
    let mut cloud = Cloud::default();
    c.observe(source());
    publish(&mut c, &mut cloud, 100);
    assert_eq!(
        cloud
            .sent
            .iter()
            .map(|(p, data)| (*p, data["theme"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        vec![(1, "cyber-hud"), (2, "pop-cut")]
    );
    assert_eq!(c.last_content_upload(), Some(100));
    let swapped = [
        InkcardPage {
            page: 1,
            theme: PopCut,
        },
        InkcardPage {
            page: 2,
            theme: CyberHud,
        },
    ];
    let mut restarted = InkcardCoordinator::with_pages(
        &settings(),
        &StatusFramePublisherState::default(),
        Some(c.snapshot()),
        &swapped,
    )
    .unwrap();
    restarted.observe(source());
    cloud.sent.clear();
    publish(&mut restarted, &mut cloud, 159);
    assert!(cloud.sent.is_empty());
    publish(&mut restarted, &mut cloud, 160);
    assert_eq!(
        cloud
            .sent
            .iter()
            .map(|(p, data)| (*p, data["theme"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        vec![(1, "pop-cut"), (2, "cyber-hud")]
    );
    let single = [InkcardPage {
        page: 4,
        theme: Eva,
    }];
    let mut c = InkcardCoordinator::with_pages(
        &settings(),
        &StatusFramePublisherState::default(),
        None,
        &single,
    )
    .unwrap();
    c.observe(source());
    cloud.sent.clear();
    publish(&mut c, &mut cloud, 200);
    assert_eq!(cloud.sent.len(), 1);
    assert_eq!(cloud.sent[0].0, 4);
    assert_eq!(cloud.sent[0].1["theme"], "eva");
    assert_eq!(c.last_content_upload(), Some(200));
}

#[test]
fn each_completed_page_is_durable_before_the_next_request_and_restart_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inkcard-state.json");
    let mut c = coordinator();
    c.observe(source());
    let mut cloud = Cloud {
        now: 100,
        ..Default::default()
    };
    let mut checks = 0;
    c.publish_due(
        || Some(100),
        &mut Renderer,
        &mut cloud,
        || {
            checks += 1;
            if checks == 2 {
                let saved = InkcardState::load(&path).unwrap().unwrap();
                assert_eq!(
                    saved.pages[0].last_successful_http_upload_epoch_seconds,
                    Some(100)
                );
                assert!(saved.pages[1].last_successful_content_fingerprint.is_none());
                return false; // Process stops before the second network request.
            }
            true
        },
        |saved| saved.save_atomically(&path).is_ok(),
    )
    .unwrap();
    let mut restarted = InkcardCoordinator::new(
        &settings(),
        &StatusFramePublisherState::default(),
        InkcardState::load(&path).unwrap(),
    )
    .unwrap();
    restarted.observe(source());
    cloud.sent.clear();
    publish(&mut restarted, &mut cloud, 101);
    assert_eq!(
        cloud.sent.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![2, 3, 4, 5]
    );
}

#[test]
fn failed_checkpoint_prevents_further_network_requests() {
    let mut c = coordinator();
    c.observe(source());
    let mut cloud = Cloud {
        now: 100,
        ..Default::default()
    };
    let result = c.publish_due(|| Some(100), &mut Renderer, &mut cloud, || true, |_| false);
    assert_eq!(
        result,
        Err(codex_zectrix_dashboard::inkcards::InkcardPublishError::StatePersistUnavailable)
    );
    assert_eq!(cloud.sent.len(), 1);
}

#[cfg(windows)]
#[test]
fn returning_to_single_forces_the_selected_style_but_keeps_last_page_cooldown() {
    let directory = tempfile::tempdir().unwrap();
    let data = directory.path();
    let install = serde_json::json!({"schemaVersion":1,"activeVersion":env!("CARGO_PKG_VERSION"),"previousVersion":null});
    std::fs::write(
        data.join("installation.json"),
        serde_json::to_vec(&install).unwrap(),
    )
    .unwrap();
    std::fs::write(
        data.join("settings.json"),
        serde_json::to_vec(&settings()).unwrap(),
    )
    .unwrap();
    let mut all = coordinator();
    let mut cloud = Cloud::default();
    all.observe(source());
    publish(&mut all, &mut cloud, 100);
    publish(&mut all, &mut cloud, 160);
    all.snapshot()
        .save_atomically(data.join("inkcard-state.json"))
        .unwrap();
    codex_zectrix_dashboard::operator::save_inkcard_mode(data, InkcardMode::Single).unwrap();
    let seed = StatusFramePublisherState::load(data.join("status-frame-state.json"))
        .unwrap()
        .unwrap();
    let mut single = codex_zectrix_dashboard::StatusFrameCoordinator::new(seed);
    single.observe(source());
    cloud.sent.clear();
    assert!(matches!(
        single.try_publish(219, &mut Renderer, &mut cloud),
        StatusFrameAttempt::Deferred { .. }
    ));
    assert!(cloud.sent.is_empty());
    cloud.now = 220;
    assert_eq!(
        single.try_publish(220, &mut Renderer, &mut cloud),
        StatusFrameAttempt::Published
    );
    assert_eq!(cloud.sent[0].0, 3);
    assert_eq!(cloud.sent[0].1["theme"], "pop-cut");
    // An explicit mode choice also recovers a corrupt, derived five-page cache.
    std::fs::write(data.join("inkcard-state.json"), b"corrupt").unwrap();
    codex_zectrix_dashboard::operator::save_inkcard_mode(data, InkcardMode::All).unwrap();
    assert!(
        InkcardState::load(data.join("inkcard-state.json"))
            .unwrap()
            .is_some()
    );
}
