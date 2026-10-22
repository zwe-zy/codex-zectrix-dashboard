use crate::desktop::{app::*, *};
use eframe::egui;
pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.label("按下面三步完成配置。现有设备在确认提交前保持原设置。");
    ui.add_space(12.0);
    ui.strong("1  检查 Codex 登录");
    if ui
        .add_enabled(
            !app.jobs.is_busy(),
            egui::Button::new(if app.model.codex_checked {
                "重新检查登录"
            } else {
                "检查 Codex 登录"
            }),
        )
        .clicked()
    {
        app.action(UiAction::CheckCodex);
    }
    ui.add_space(10.0);
    ui.separator();
    ui.strong("2  连接 ZECTRIX 设备");
    let ready = app
        .model
        .status
        .as_ref()
        .is_some_and(|s| s.installed_version.is_some() && !s.read_only);
    ui.add_enabled_ui(!app.jobs.is_operation_busy() && ready, |ui| {
        if app.model.devices.is_empty() {
            ui.checkbox(&mut app.model.use_saved_key, "使用已保存的 Key");
            if !app.model.use_saved_key {
                ui.label("ZECTRIX API Key");
                let mut edit = egui::TextEdit::singleline(&mut *app.model.secret)
                    .password(true)
                    .id(egui::Id::unique("zectrix-key"))
                    .hint_text("粘贴 API Key")
                    .desired_width(450.0)
                    .show(ui);
                // Password widgets must never retain copies in persistent undo history.
                edit.state.clear_undoer();
                edit.state.store(ui.ctx(), edit.response.id);
                ui.small("保存在 Windows 凭据管理器中，不写入配置文件或日志。");
            }
            if ui
                .add_enabled(
                    app.model.codex_checked && !app.jobs.is_busy(),
                    egui::Button::new("获取设备列表"),
                )
                .clicked()
            {
                app.action(UiAction::Discover);
            }
        } else {
            egui::ComboBox::from_id_salt("device-choice")
                .selected_text(
                    app.model
                        .devices
                        .get(app.model.device_index)
                        .cloned()
                        .unwrap_or_default(),
                )
                .width(360.0)
                .show_ui(ui, |ui| {
                    for (index, label) in app.model.devices.iter().enumerate() {
                        if ui
                            .selectable_value(&mut app.model.device_index, index, label)
                            .changed()
                        {
                            app.model.setup_preview = None;
                        }
                    }
                });
            ui.horizontal(|ui| {
                ui.label("首次测试上传到墨卡");
                for page in 1..=5 {
                    if ui
                        .selectable_value(&mut app.model.target_page, page, page.to_string())
                        .changed()
                    {
                        app.model.setup_preview = None;
                    }
                }
            });
            ui.small(
                "日常使用的数量、风格和顺序在“墨卡与主题”页设置；重新连接设备会保留已有排列。",
            );
            if ui
                .add_enabled(!app.jobs.is_busy(), egui::Button::new("生成测试预览"))
                .clicked()
            {
                app.action(UiAction::Prepare);
            }
        }
    });
    if app.model.draft_id.is_some() && ui.button("取消本次配置").clicked() {
        app.cancel_draft();
    }
    ui.add_space(10.0);
    ui.separator();
    ui.strong("3  确认预览并上传");
    if let Some(png) = app.model.setup_preview.clone() {
        ui.colored_label(accent(), "测试示例，不是真实额度。");
        app.show_png(ui, &png, 400.0);
        ui.label(crate::CLOUD_PAYLOAD_SUMMARY);
        if ui
            .add_enabled(
                !app.jobs.is_busy(),
                egui::Button::new("确认保存并上传测试图"),
            )
            .clicked()
        {
            app.action(UiAction::ConfirmSetup);
        }
    } else {
        ui.label("选择设备和页面后，先生成预览，再确认保存。");
    }
}
