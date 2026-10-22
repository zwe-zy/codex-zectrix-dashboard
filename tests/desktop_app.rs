use codex_zectrix_dashboard::{desktop::*, inkcards::InkcardMode, status_board::BoardTheme};
#[test]
fn preview_click_never_emits_set_theme() {
    let mut model = AppModel::default();
    assert!(model.request_action(UiAction::SelectPage(4)).is_none());
    assert_eq!(model.selected_page, 4);
    assert!(
        model
            .request_action(UiAction::SelectTheme(BoardTheme::CyberHud))
            .is_none()
    );
    assert!(matches!(
        model.request_action(UiAction::ApplyTheme),
        Some(DesktopCommand::SetTheme(BoardTheme::CyberHud))
    ));
}
#[test]
fn all_pages_require_explicit_confirmation() {
    let mut model = AppModel::default();
    assert!(model.request_action(UiAction::ConfirmMode).is_none());
    assert!(
        model
            .request_action(UiAction::RequestMode(InkcardMode::All))
            .is_none()
    );
    assert!(matches!(
        model.request_action(UiAction::ConfirmMode),
        Some(DesktopCommand::SetMode(InkcardMode::All))
    ));
    assert!(model.request_action(UiAction::ConfirmMode).is_none());
}
#[test]
fn only_explicit_stop_stops_background() {
    let mut model = AppModel::default();
    assert!(model.request_action(UiAction::Close).is_none());
    assert!(matches!(
        model.request_action(UiAction::Stop),
        Some(DesktopCommand::Stop)
    ));
}
#[test]
fn configured_warning_keeps_dashboard_usable() {
    let mut model = AppModel::default();
    model.apply_reply(
        JobId(1),
        DesktopReply::Completed(OperationReport {
            kind: OperationKind::Configured,
            warning: Some("启动失败".into()),
        }),
    );
    assert!(model.configuration_saved);
    assert_eq!(model.page, ConsolePage::Overview);
    assert!(model.notice.as_deref().unwrap().contains("启动失败"));
}
#[test]
fn stale_reply_cannot_replace_current_draft() {
    let mut model = AppModel::default();
    model.request_action(UiAction::Discover);
    let first = model.draft_id.unwrap();
    model.cancel_draft();
    model.request_action(UiAction::Discover);
    let second = model.draft_id.unwrap();
    assert_ne!(first, second);
    model.apply_reply(
        JobId(1),
        DesktopReply::Devices {
            draft_id: first,
            labels: vec!["旧设备".into()],
        },
    );
    assert!(model.devices.is_empty());
    model.apply_reply(
        JobId(2),
        DesktopReply::Devices {
            draft_id: second,
            labels: vec!["新设备".into()],
        },
    );
    assert_eq!(model.devices, vec!["新设备"]);
}

#[cfg(feature = "desktop")]
#[test]
fn four_pages_render_at_small_large_and_scaled_sizes_with_long_chinese_labels() {
    use codex_zectrix_dashboard::desktop::app::{DesktopApp, configure_context};
    use eframe::egui;
    struct Idle;
    impl DesktopBackend for Idle {
        fn execute(&mut self, _: DesktopCommand, _: &DraftCancellation) -> DesktopReply {
            DesktopReply::Failed(DesktopError::LocalUnavailable)
        }
        fn clear_draft(&mut self, _: DraftId) {}
    }
    let ctx = egui::Context::default();
    configure_context(&ctx);
    let mut app = DesktopApp::with_jobs(DesktopJobs::spawn(Idle));
    app.model.devices = vec!["很长的中文设备名称用于检查换行与缩放且不能遮挡按钮".repeat(3)];
    app.model.show_samples = true;
    for dark in [false, true] {
        ctx.set_visuals(if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        for scale in [1.0, 1.5, 2.0] {
            ctx.set_pixels_per_point(scale);
            for size in [[620.0, 480.0], [1040.0, 760.0]] {
                for page in ConsolePage::ALL {
                    app.model.page = page;
                    let mut result = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(size[0], size[1]),
                            )),
                            ..Default::default()
                        },
                        |ui| app.render(ui),
                    );
                    assert!(
                        !result.shapes.is_empty(),
                        "{page:?} should paint usable content"
                    );
                    result.textures_delta.clear(); // Headless test intentionally has no GPU consumer.
                }
            }
        }
    }
    for theme in codex_zectrix_dashboard::inkcards::PAGE_THEMES {
        for dual in [false, true] {
            let png = app.sample_png(theme, dual).unwrap();
            let image = image::load_from_memory(&png).unwrap();
            assert_eq!((image.width(), image.height()), (400, 300));
        }
    }
}
