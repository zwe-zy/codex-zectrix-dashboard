use crate::AutostartStatus;
use crate::desktop::{app::*, *};
use eframe::egui;
pub fn show(app: &mut DesktopApp, ui: &mut egui::Ui) {
    ui.label("启动设置、安装维护和本地诊断。");
    ui.add_space(14.0);
    egui::Frame::group(ui.style())
        .inner_margin(16)
        .show(ui, |ui| {
            ui.strong("运行设置");
            let state = app
                .model
                .status
                .as_ref()
                .map(|s| s.startup)
                .unwrap_or(AutostartStatus::Absent);
            let mut enabled = state == AutostartStatus::Present;
            if ui
                .add_enabled(
                    app.can_modify(),
                    egui::Checkbox::new(&mut enabled, "登录 Windows 时自动启动后台"),
                )
                .changed()
            {
                app.action(UiAction::SetStartup(enabled));
            }
            if state == AutostartStatus::Unavailable {
                ui.label("启动项暂时无法确认，请检查安装。");
            }
            ui.small("仅启动后台，不自动弹出控制台窗口。");
        });
    ui.add_space(12.0);
    egui::Frame::group(ui.style())
        .inner_margin(16)
        .show(ui, |ui| {
            ui.strong("版本与安装");
            ui.label(format!("当前控制台：{}", env!("CARGO_PKG_VERSION")));
            ui.label(format!(
                "已安装：{}",
                app.model
                    .status
                    .as_ref()
                    .and_then(|s| s.installed_version.as_deref())
                    .unwrap_or("尚未安装")
            ));
            if ui
                .add_enabled(!app.jobs.is_busy(), egui::Button::new("安装 / 升级 / 恢复"))
                .clicked()
            {
                app.action(UiAction::Install);
            }
            ui.small("请从完整 ZIP 中打开新版本控制台进行升级。");
        });
    ui.add_space(12.0);
    ui.strong("本地诊断");
    if ui
        .add_enabled(!app.jobs.is_busy(), egui::Button::new("查看诊断"))
        .clicked()
    {
        app.action(UiAction::Diagnose);
    }
    for line in &app.model.diagnostics {
        ui.label(line);
    }
    ui.small("诊断仅在本机显示，不会上传日志。");
    ui.add_space(18.0);
    ui.collapsing("卸载程序", |ui| {
        ui.label("卸载会停止后台并移除登录启动项。下方选项默认保留。");
        ui.checkbox(&mut app.model.delete_data, "同时删除设备设置、缓存和日志");
        ui.checkbox(&mut app.model.delete_key, "同时删除已保存的 API Key");
        ui.checkbox(&mut app.model.confirm_uninstall, "确认卸载本程序");
        if ui
            .add_enabled(
                app.can_uninstall() && app.model.confirm_uninstall,
                egui::Button::new(
                    if app
                        .model
                        .status
                        .as_ref()
                        .is_some_and(|s| s.uninstall_pending)
                    {
                        "继续卸载并关闭窗口"
                    } else {
                        "执行卸载并关闭窗口"
                    },
                ),
            )
            .clicked()
        {
            app.action(UiAction::ConfirmUninstall);
        }
    });
}
