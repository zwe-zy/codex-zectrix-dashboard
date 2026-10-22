#![cfg(feature = "desktop")]
use codex_zectrix_dashboard::{
    AutostartStatus, desktop::app::*, desktop::*, desktop_state::*, inkcards::InkcardMode,
    status_board::BoardTheme,
};
use eframe::egui;

struct Idle;
impl DesktopBackend for Idle {
    fn execute(&mut self, _: DesktopCommand, _: &DraftCancellation) -> DesktopReply {
        DesktopReply::Failed(DesktopError::LocalUnavailable)
    }
    fn clear_draft(&mut self, _: DraftId) {}
}
fn status() -> DesktopStatus {
    DesktopStatus {
        installed_version: Some(env!("CARGO_PKG_VERSION").into()),
        configured: true,
        needs_recovery: false,
        uninstall_pending: false,
        read_only: false,
        background: DesktopBackground::Running,
        mode: InkcardMode::Single,
        theme: BoardTheme::default(),
        configured_page: 1,
        device_label: "NOTE4 fake".into(),
        snapshot: None,
        freshness: DesktopFreshness {
            quota: Freshness::Live,
            secondary_quota: Freshness::Unavailable,
            task: Freshness::Live,
        },
        pages: vec![],
        startup: AutostartStatus::Absent,
        credentials_rejected: false,
    }
}

#[test]
fn refresh_preserves_unsaved_card_order_and_discard_restores_actual_pages() {
    use codex_zectrix_dashboard::StatusFramePublisherState;
    let mut status = status();
    status.mode = InkcardMode::All;
    status.pages = [BoardTheme::Eva, BoardTheme::CyberHud]
        .into_iter()
        .enumerate()
        .map(|(i, theme)| PageStatus {
            page: i as u8 + 1,
            theme,
            record: StatusFramePublisherState::default(),
            uploaded_png: None,
        })
        .collect();
    let mut model = AppModel::default();
    model.cards.load_status(&status);
    model.cards.move_card(1, 0);
    model.cards.load_status(&status);
    assert_eq!(
        model.cards.themes,
        vec![BoardTheme::CyberHud, BoardTheme::Eva]
    );
    model.cards.discard(&status);
    assert_eq!(
        model.cards.themes,
        vec![BoardTheme::Eva, BoardTheme::CyberHud]
    );
    assert!(!model.cards.dirty);
}
fn fixture() -> (egui::Context, DesktopApp) {
    let ctx = egui::Context::default();
    configure_context(&ctx);
    let mut app = DesktopApp::with_jobs(DesktopJobs::spawn(Idle));
    app.model.status = Some(Box::new(status()));
    app.model.page = ConsolePage::Device;
    app.model.use_saved_key = false;
    frame(&ctx, &mut app, 0.0, vec![]);
    ctx.memory_mut(|m| m.request_focus(egui::Id::unique("zectrix-key")));
    frame(
        &ctx,
        &mut app,
        1.0,
        vec![egui::Event::Paste("synthetic-key".into())],
    );
    frame(&ctx, &mut app, 2.0, vec![]);
    assert_eq!(app.model.secret.as_str(), "synthetic-key");
    (ctx, app)
}
fn frame(ctx: &egui::Context, app: &mut DesktopApp, time: f64, events: Vec<egui::Event>) {
    let mut result = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1040.0, 760.0),
            )),
            ..Default::default()
        },
        |ui| app.render(ui),
    );
    result.textures_delta.clear();
}
#[test]
fn local_refresh_keeps_key_focus_and_accepts_typing() {
    let (ctx, mut app) = fixture();
    app.jobs.submit(DesktopCommand::Refresh).unwrap();
    frame(
        &ctx,
        &mut app,
        2.1,
        vec![egui::Event::Text("-continued".into())],
    );
    assert!(ctx.memory(|m| m.has_focus(egui::Id::unique("zectrix-key"))));
    assert_eq!(app.model.secret.as_str(), "synthetic-key-continued");
}
#[test]
fn cleared_key_cannot_be_restored_from_widget_undo_history() {
    for lifecycle in 0..4 {
        let (ctx, mut app) = fixture();
        match lifecycle {
            0 => app.cancel_draft(),
            1 => {
                app.model.request_action(UiAction::Discover);
            }
            2 => app.model.apply_reply(
                JobId(1),
                DesktopReply::Failed(DesktopError::LocalUnavailable),
            ),
            _ => app.model.apply_reply(
                JobId(1),
                DesktopReply::Completed(OperationReport {
                    kind: OperationKind::Configured,
                    warning: None,
                }),
            ),
        }
        assert!(app.model.secret.is_empty());
        app.model.page = ConsolePage::Device;
        frame(&ctx, &mut app, 3.0, vec![]);
        ctx.memory_mut(|m| m.request_focus(egui::Id::unique("zectrix-key")));
        frame(
            &ctx,
            &mut app,
            4.0,
            vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
        );
        assert!(
            app.model.secret.is_empty(),
            "lifecycle {lifecycle} restored a cleared key"
        );
    }
}
#[test]
fn failed_refresh_invalidates_live_state_without_cancelling_input() {
    let (ctx, mut app) = fixture();
    let now = codex_zectrix_dashboard::platform::now_epoch_seconds().unwrap();
    app.model.status.as_mut().unwrap().snapshot = Some(DesktopSnapshot::new(
        &codex_zectrix_dashboard::Settings {
            device_id: "fixture".into(),
            page_id: 1,
        },
        now,
        &codex_zectrix_dashboard::RuntimeStatus {
            last_successful_quota_refresh_epoch_seconds: Some(now),
            last_successful_task_refresh_epoch_seconds: Some(now),
            ..Default::default()
        },
        codex_zectrix_dashboard::status_board::StatusBoardState {
            quota_remaining_percent: Some(75.0),
            task_state: codex_zectrix_dashboard::status_board::BoardTaskState::Running,
            ..Default::default()
        },
    ));
    app.jobs.submit(DesktopCommand::Refresh).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !app.model.error && std::time::Instant::now() < deadline {
        app.pump(&ctx);
        std::thread::yield_now();
    }
    assert!(app.model.error);
    assert_eq!(app.model.secret.as_str(), "synthetic-key");
    let s = app.model.status.as_ref().unwrap();
    assert_eq!(s.background, DesktopBackground::Unavailable);
    assert_eq!(s.freshness.quota, Freshness::LastKnown);
    app.model.age_status(now + 1801);
    let s = app.model.status.as_ref().unwrap();
    assert_eq!(s.freshness.quota, Freshness::Stale);
    assert_eq!(s.freshness.task, Freshness::Stale);
}
#[test]
fn pending_uninstall_remains_retryable_but_other_recovery_does_not() {
    let (_, mut app) = fixture();
    app.model.status.as_mut().unwrap().needs_recovery = true;
    assert!(!app.can_modify());
    assert!(!app.can_uninstall());
    app.model.status.as_mut().unwrap().uninstall_pending = true;
    assert!(app.can_uninstall());
    app.model.status.as_mut().unwrap().read_only = true;
    assert!(!app.can_uninstall());
}

#[test]
fn populated_overview_keeps_quota_and_task_inside_window() {
    use codex_zectrix_dashboard::{RuntimeStatus, Settings, status_board::*};
    fn collect(shape: &egui::epaint::Shape, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::epaint::Shape::Text(t) => {
                found.push((t.galley.job.text.clone(), shape.visual_bounding_rect()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                for s in shapes {
                    collect(s, found);
                }
            }
            _ => {}
        }
    }
    for width in [620.0, 1040.0] {
        let (ctx, mut app) = fixture();
        app.model.page = ConsolePage::Overview;
        let now = codex_zectrix_dashboard::platform::now_epoch_seconds().unwrap();
        app.model.status.as_mut().unwrap().snapshot = Some(DesktopSnapshot::new(
            &Settings {
                device_id: "fixture".into(),
                page_id: 1,
            },
            now,
            &RuntimeStatus::default(),
            StatusBoardState {
                quota_remaining_percent: Some(75.0),
                quota_window_duration_mins: Some(300),
                quota_secondary: Some(BoardQuotaWindow {
                    remaining_percent: 30.0,
                    stale: true,
                    resets_at_epoch_seconds: Some(now + 86400),
                    window_duration_mins: Some(10080),
                }),
                task_state: BoardTaskState::Running,
                ..Default::default()
            },
        ));
        let mut found = vec![];
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1400.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
            found.clear();
            for s in &output.shapes {
                collect(&s.shape, &mut found);
            }
            output.textures_delta.clear();
        }
        for label in ["Codex 额度", "任务状态", "75% 剩余", "运行中"] {
            let rects: Vec<_> = found.iter().filter(|(text, _)| text == label).collect();
            assert!(!rects.is_empty(), "missing {label} at width {width}");
            for (_, rect) in rects {
                assert!(
                    rect.min.x >= 186.0 && rect.max.x <= width,
                    "{label} outside {width}: {rect:?}"
                );
            }
        }
        let quota = found
            .iter()
            .find(|(text, _)| text == "Codex 额度")
            .unwrap()
            .1;
        let percent = found.iter().find(|(text, _)| text == "75% 剩余").unwrap().1;
        assert!(
            percent.min.y > quota.min.y,
            "quota value should be below its heading"
        );
        if width == 1040.0 {
            let task = found.iter().find(|(text, _)| text == "任务状态").unwrap().1;
            assert!(
                (task.min.y - quota.min.y).abs() < 1.0,
                "adjacent cards should align at the top"
            );
        }
    }
}

#[test]
fn cards_drag_reorders_without_saving_and_rows_fit_a_narrow_window() {
    fn collect(shape: &egui::epaint::Shape, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::epaint::Shape::Text(t) => {
                found.push((t.galley.job.text.clone(), shape.visual_bounding_rect()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                for s in shapes {
                    collect(s, found);
                }
            }
            _ => {}
        }
    }
    for width in [620.0, 1040.0] {
        let ctx = egui::Context::default();
        configure_context(&ctx);
        let mut app = DesktopApp::with_jobs(DesktopJobs::spawn(Idle));
        app.model.status = Some(Box::new(status()));
        app.model.page = ConsolePage::Cards;
        app.model.cards.multiple = true;
        let mut time = 0.0;
        let mut draw = |app: &mut DesktopApp, events| {
            time += 0.1;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1400.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
            let mut found = vec![];
            for s in &output.shapes {
                collect(&s.shape, &mut found);
            }
            output.textures_delta.clear();
            found
        };
        draw(&mut app, vec![]);
        draw(&mut app, vec![]);
        let found = draw(&mut app, vec![]);
        for (text, rect) in &found {
            if text.starts_with("墨卡 ") || ["上移", "下移", "移除"].contains(&text.as_str())
            {
                assert!(rect.max.x <= width, "{text} exceeds {width}: {rect:?}");
            }
        }
        let from = found.iter().find(|(t, _)| t == "拖动").unwrap().1.center();
        let to = found
            .iter()
            .find(|(t, _)| t == "墨卡 3")
            .unwrap()
            .1
            .center();
        draw(&mut app, vec![egui::Event::PointerMoved(from)]);
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            }],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(from + egui::vec2(0.0, 15.0))],
        );
        draw(&mut app, vec![egui::Event::PointerMoved(to)]);
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        draw(&mut app, vec![]);
        assert_eq!(
            app.model.cards.themes,
            vec![
                BoardTheme::Eva,
                BoardTheme::PixelConsole,
                BoardTheme::PopCut,
                BoardTheme::SpaceGauge,
                BoardTheme::CyberHud
            ]
        );
        assert!(app.model.cards.dirty);
        assert!(!app.jobs.is_busy(), "dragging must not save or upload");
    }
}

#[test]
fn direct_count_picker_selects_three_cards_without_saving() {
    fn collect(shape: &egui::epaint::Shape, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::epaint::Shape::Text(t) => {
                found.push((t.galley.job.text.clone(), shape.visual_bounding_rect()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, found);
                }
            }
            _ => {}
        }
    }
    for width in [620.0, 1040.0] {
        let ctx = egui::Context::default();
        configure_context(&ctx);
        let mut app = DesktopApp::with_jobs(DesktopJobs::spawn(Idle));
        app.model.status = Some(Box::new(status()));
        app.model.page = ConsolePage::Cards;
        let mut time = 0.0;
        let mut draw = |app: &mut DesktopApp, events| {
            time += 0.1;
            let mut out = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 1200.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
            let mut found = vec![];
            for shape in &out.shapes {
                collect(&shape.shape, &mut found);
            }
            out.textures_delta.clear();
            found
        };
        draw(&mut app, vec![]);
        draw(&mut app, vec![]);
        let found = draw(&mut app, vec![]);
        for count in 1..=5 {
            let rect = found
                .iter()
                .find(|(text, _)| text == &format!("{count} 张"))
                .expect("all counts must be directly visible")
                .1;
            assert!(rect.max.x <= width && rect.min.y < 300.0);
        }
        let point = found
            .iter()
            .find(|(text, _)| text == "3 张")
            .unwrap()
            .1
            .center();
        draw(&mut app, vec![egui::Event::PointerMoved(point)]);
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            }],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        assert!(app.model.cards.multiple);
        assert_eq!(
            app.model
                .cards
                .configuration()
                .unwrap()
                .pages
                .unwrap()
                .len(),
            3
        );
        assert!(app.model.cards.dirty);
        assert!(!app.jobs.is_busy());
    }
}
