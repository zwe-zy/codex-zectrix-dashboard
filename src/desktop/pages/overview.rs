use crate::desktop::{app::*, *};
use eframe::egui::{self, RichText};
pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.label("查看数据读取、云端上传和后台运行情况。");
    ui.add_space(14.0);
    if let Some(status) = &app.model.status {
        let background = match status.background {
            DesktopBackground::Running => "运行中",
            DesktopBackground::Starting => "正在启动",
            DesktopBackground::Stopped => "已停止",
            DesktopBackground::Unavailable => "无法确认",
        };
        ui.with_layout(
            egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
            |ui| {
                for (label, value) in [
                    ("后台", background.to_owned()),
                    ("设备", status.device_label.clone()),
                    (
                        "更新模式",
                        if status.mode == crate::inkcards::InkcardMode::All {
                            format!("{} 张自动更新", status.pages.len())
                        } else {
                            "单张自动更新".into()
                        },
                    ),
                ] {
                    stat_card(ui, 180.0, |ui| {
                        ui.small(label);
                        ui.label(RichText::new(value).size(19.0).strong());
                    });
                }
            },
        );
        ui.add_space(12.0);
        if let Some(s) = &status.snapshot {
            ui.with_layout(
                egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
                |ui| {
                    stat_card(ui, 320.0, |ui| {
                        ui.strong("Codex 额度");
                        if let Some(percent) = s.board.quota_remaining_percent {
                            ui.label(
                                RichText::new(format!("{percent:.0}% 剩余"))
                                    .size(30.0)
                                    .color(accent()),
                            );
                            ui.label(format!(
                                "{} · {}",
                                duration_label(s.board.quota_window_duration_mins),
                                status.freshness.quota.label()
                            ));
                            ui.add(
                                egui::ProgressBar::new(percent as f32 / 100.0).desired_width(220.0),
                            );
                            ui.small(format!(
                                "重置：{}",
                                time_label(s.board.quota_resets_at_epoch_seconds)
                            ));
                        }
                        if let Some(q) = &s.board.quota_secondary {
                            ui.label(format!(
                                "{}：{:.0}% 剩余 · {}",
                                duration_label(q.window_duration_mins),
                                q.remaining_percent,
                                status.freshness.secondary_quota.label()
                            ));
                            ui.small(format!("重置：{}", time_label(q.resets_at_epoch_seconds)));
                        }
                        ui.small(format!("读取：{}", time_label(s.quota_observed_at)));
                    });
                    stat_card(ui, 240.0, |ui| {
                        ui.strong("任务状态");
                        ui.label(RichText::new(s.board.task_state.large_label()).size(26.0));
                        ui.label(status.freshness.task.label());
                        ui.small(format!("读取：{}", time_label(s.task_observed_at)));
                    });
                },
            );
        } else {
            ui.label("暂没有可用的状态快照。配置设备并启动后台后，数据会自动出现。");
        }
        if status.credentials_rejected {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "云端拒绝了 Key，请到设备配置页重新配置。",
            );
        }
    }
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(app.can_modify(), egui::Button::new("启动后台"))
            .clicked()
        {
            app.action(UiAction::Start);
        }
        if ui
            .add_enabled(app.can_modify(), egui::Button::new("停止后台"))
            .clicked()
        {
            app.action(UiAction::Stop);
        }
        if ui.button("配置设备").clicked() {
            app.model.page = ConsolePage::Device;
        }
    });
    ui.add_space(16.0);
    ui.separator();
    ui.strong("最近成功上传");
    ui.horizontal_wrapped(|ui| {
        for page in 1..=5 {
            if ui
                .selectable_label(app.model.selected_page == page, format!("墨卡 {page}"))
                .clicked()
            {
                app.model.selected_page = page;
            }
        }
    });
    let page = app
        .model
        .status
        .as_ref()
        .and_then(|s| s.pages.iter().find(|p| p.page == app.model.selected_page));
    let bytes = page.and_then(|p| p.uploaded_png.clone());
    if let Some(p) = page {
        ui.small(format!(
            "{} · 云端接收 {} · 连续失败 {}",
            p.theme.label(),
            time_label(p.record.last_successful_http_upload_epoch_seconds),
            p.record.consecutive_failure_count()
        ));
    }
    if let Some(bytes) = bytes {
        app.show_png(ui, &bytes, 440.0);
    } else {
        ui.label("这一页暂没有可核对的成功图片，可到墨卡页查看示例布局。");
    }
    ui.small("上传成功表示云端已接收；设备同步后刷新。这里不推测设备是否在线。");
}

fn stat_card(ui: &mut egui::Ui, width: f32, contents: impl FnOnce(&mut egui::Ui)) {
    // Reserve width before wrapping; card contents must not inherit the row layout.
    ui.allocate_ui_with_layout(
        egui::vec2(width.min(ui.max_rect().width()), 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            egui::Frame::group(ui.style())
                .inner_margin(16)
                .show(ui, contents);
        },
    );
}
