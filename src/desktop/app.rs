use super::*;
use eframe::egui::{self, Color32, RichText};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub struct DesktopApp {
    pub model: AppModel,
    pub jobs: DesktopJobs,
    last_refresh: Instant,
    textures: HashMap<String, egui::TextureHandle>,
    samples: HashMap<(BoardTheme, bool), Vec<u8>>,
    closing: bool,
}
impl DesktopApp {
    pub fn new(cc: &eframe::CreationContext<'_>, jobs: DesktopJobs) -> Self {
        configure_context(&cc.egui_ctx);
        Self::with_jobs(jobs)
    }
    pub fn with_jobs(jobs: DesktopJobs) -> Self {
        Self {
            model: AppModel::default(),
            jobs,
            last_refresh: Instant::now() - Duration::from_secs(3),
            textures: HashMap::new(),
            samples: HashMap::new(),
            closing: false,
        }
    }
    pub fn action(&mut self, action: UiAction) {
        if self.jobs.is_busy() {
            return;
        }
        if let Some(command) = self.model.request_action(action) {
            self.model.notice = None;
            self.model.error = false;
            if self.jobs.submit(command).is_err() {
                self.model.notice = Some("操作尚未完成，请稍后重试。".into());
                self.model.error = true;
            }
        }
    }
    pub fn cancel_draft(&mut self) {
        if let Some(id) = self.model.draft_id
            && self.jobs.cancel_draft(id).is_err()
        {
            return;
        }
        self.model.cancel_draft();
        self.model.notice = Some(
            if self.jobs.is_busy() {
                "正在取消，等待当前请求结束。"
            } else {
                "已取消，未提交配置。"
            }
            .into(),
        );
    }
    pub fn can_modify(&self) -> bool {
        !self.jobs.is_busy()
            && self
                .model
                .status
                .as_ref()
                .is_some_and(|s| s.installed_version.is_some() && !s.read_only && !s.needs_recovery)
    }
    pub fn can_uninstall(&self) -> bool {
        !self.jobs.is_busy()
            && self.model.status.as_ref().is_some_and(|s| {
                s.installed_version.is_some()
                    && !s.read_only
                    && (!s.needs_recovery || s.uninstall_pending)
            })
    }
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Some((job, reply)) = self.jobs.try_recv() {
            if matches!(reply, DesktopReply::Failed(_)) {
                self.cancel_draft();
            }
            let refresh = matches!(reply, DesktopReply::Completed(_));
            self.model.apply_reply(job, reply);
            if refresh {
                self.last_refresh = Instant::now() - Duration::from_secs(3);
                self.textures.clear();
            }
        }
        self.model
            .age_status(crate::platform::now_epoch_seconds().unwrap_or(i64::MAX));
        if self.closing && !self.jobs.is_busy() || self.model.should_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if !self.jobs.is_busy() && self.last_refresh.elapsed() >= Duration::from_secs(2) {
            let _ = self.jobs.submit(DesktopCommand::Refresh);
            self.last_refresh = Instant::now();
        }
        ctx.request_repaint_after(if self.jobs.is_busy() {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(1)
        });
    }
    pub fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.jobs.request_close() == CloseDecision::WaitForCommit {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.closing = true;
            } else {
                self.model.cancel_draft();
            }
        }
        egui::Panel::left("navigation")
            .resizable(false)
            .default_size(186.0)
            .frame(
                egui::Frame::new()
                    .fill(if ui.visuals().dark_mode {
                        Color32::from_rgb(21, 29, 43)
                    } else {
                        Color32::from_rgb(235, 240, 247)
                    })
                    .inner_margin(18),
            )
            .show(ui, |ui| {
                ui.add_space(14.0);
                ui.label(RichText::new("NOTE4").size(28.0).strong());
                ui.label(RichText::new("CODEX 控制台").size(12.0).color(accent()));
                ui.add_space(32.0);
                for (index, page) in ConsolePage::ALL.into_iter().enumerate() {
                    let label = format!("{:02}   {}", index + 1, page.label());
                    if ui
                        .add_sized(
                            [150.0, 43.0],
                            egui::Button::new(label).selected(self.model.page == page),
                        )
                        .clicked()
                    {
                        self.model.page = page;
                    }
                    ui.add_space(7.0);
                }
                ui.add_space(34.0);
                ui.separator();
                ui.add_space(10.0);
                ui.small("关闭窗口后\n后台继续更新墨卡。");
                ui.add_space(12.0);
                ui.small(format!("v{}", env!("CARGO_PKG_VERSION")));
            });
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(24)).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(self.model.page.label());
                    if self.jobs.is_busy() { ui.spinner(); ui.label(if self.closing { "正在完成保存，请稍候再关闭…" } else { "处理中…" }); }
                });
                ui.add_space(8.0);
                if let Some(notice) = &self.model.notice {
                    egui::Frame::group(ui.style()).inner_margin(12).show(ui, |ui| { ui.label(RichText::new(notice).color(if self.model.error { Color32::from_rgb(220,100,92) } else { accent() })); });
                    ui.add_space(10.0);
                }
                let installed = self.model.status.as_ref().is_some_and(|s| s.installed_version.is_some());
                let different = self.model.status.as_ref().is_some_and(|s| s.read_only);
                if !installed || different {
                    egui::Frame::group(ui.style()).inner_margin(16).show(ui, |ui| {
                        ui.strong(if different { "安装当前控制台版本" } else { "欢迎使用 NOTE4 控制台" });
                        ui.label("完整解压下载的 ZIP 后，在这里完成安装。已有 Key 和设备设置会保留。");
                        if ui.add_enabled(!self.jobs.is_busy(),egui::Button::new(if different { "安装 / 升级当前版本" } else { "安装并开始" })).clicked() { self.action(UiAction::Install); }
                    }); ui.add_space(12.0);
                }
                if self.model.status.as_ref().is_some_and(|s| s.needs_recovery) {
                    ui.colored_label(Color32::from_rgb(220,140,70), if self.model.status.as_ref().is_some_and(|s| s.uninstall_pending) { "上次卸载未完成，请在设置页继续卸载。若程序文件已被部分清理，请从完整 ZIP 打开控制台重试。" } else { "上次操作未完成：请在设备配置页重新配置，或在设置页恢复安装。" });
                }
                match self.model.page {
                    ConsolePage::Overview => super::pages::overview::show(self,ui),
                    ConsolePage::Cards => super::pages::cards::show(self,ui),
                    ConsolePage::Device => super::pages::device::show(self,ui),
                    ConsolePage::Settings => super::pages::settings::show(self,ui),
                }
            });
        });
    }
    pub fn show_png(&mut self, ui: &mut egui::Ui, bytes: &[u8], width: f32) {
        let hash = format!("{:x}", Sha256::digest(bytes));
        if !self.textures.contains_key(&hash)
            && let Ok(image) = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        {
            let rgba = image.to_rgba8();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            );
            if self.textures.len() > 24 {
                self.textures.clear();
            }
            self.textures.insert(
                hash.clone(),
                ui.ctx()
                    .load_texture(&hash, image, egui::TextureOptions::NEAREST),
            );
        }
        if let Some(texture) = self.textures.get(&hash) {
            let width = width.min(ui.available_width()).max(1.0);
            ui.image((texture.id(), egui::vec2(width, width * 0.75)));
        } else {
            ui.label("预览暂不可用。");
        }
    }
    pub fn sample_png(&mut self, theme: BoardTheme, dual: bool) -> Option<Vec<u8>> {
        let key = (theme, dual);
        if let std::collections::hash_map::Entry::Vacant(e) = self.samples.entry(key) {
            use crate::status_board::*;
            let now = crate::platform::now_epoch_seconds().ok()?;
            let state = StatusBoardState {
                theme,
                task_state: BoardTaskState::Running,
                quota_remaining_percent: Some(72.0),
                quota_window_duration_mins: Some(if dual { 300 } else { 10080 }),
                quota_resets_at_epoch_seconds: Some(now + 86400 * 3),
                quota_secondary: dual.then_some(BoardQuotaWindow {
                    remaining_percent: 40.0,
                    resets_at_epoch_seconds: Some(now + 86400 * 5),
                    window_duration_mins: Some(10080),
                    stale: false,
                }),
                is_sample: true,
                ..Default::default()
            };
            let png =
                render_status_board(&state, &crate::companion::SystemStatusBoardClock).ok()?;
            e.insert(png);
        }
        self.samples.get(&key).cloned()
    }
}
impl eframe::App for DesktopApp {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.pump(ctx);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.render(ui);
    }
}
pub fn configure_context(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "noto-sc".into(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/NotoSansSC.ttf"))
            .tweak(egui::epaint::text::FontTweak {
                coords: egui::epaint::text::VariationCoords::new([(b"wght", 400.0)]),
                ..Default::default()
            })
            .into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("noto-sc".into());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .push("noto-sc".into());
    ctx.set_fonts(fonts);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(14.0, 9.0);
            style
                .text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
            style
                .text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        });
    }
}
pub fn accent() -> Color32 {
    Color32::from_rgb(44, 157, 139)
}
pub fn time_label(value: Option<i64>) -> String {
    value
        .and_then(|v| time::OffsetDateTime::from_unix_timestamp(v).ok())
        .map(|v| {
            let t = v
                .to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC));
            format!(
                "{:02}-{:02} {:02}:{:02}",
                u8::from(t.month()),
                t.day(),
                t.hour(),
                t.minute()
            )
        })
        .unwrap_or_else(|| "暂无".into())
}
pub fn duration_label(minutes: Option<u32>) -> String {
    match minutes {
        Some(m) if m % 1440 == 0 => format!("{} 天", m / 1440),
        Some(m) if m % 60 == 0 => format!("{} 小时", m / 60),
        Some(m) => format!("{m} 分钟"),
        None => "额度窗口".into(),
    }
}
#[derive(Debug, thiserror::Error)]
#[error("图形界面未能启动，请尝试更新显卡驱动；也可使用同目录 codex-zectrix-dashboard.exe 排查。")]
pub struct DesktopLaunchError;
pub fn run_desktop() -> Result<(), DesktopLaunchError> {
    #[cfg(windows)]
    {
        let data = crate::platform::data_dir().map_err(|_| DesktopLaunchError)?;
        let package = std::env::current_exe()
            .map_err(|_| DesktopLaunchError)?
            .parent()
            .ok_or(DesktopLaunchError)?
            .to_path_buf();
        let backend = WindowsDesktopBackend::new(data, package).map_err(|_| DesktopLaunchError)?;
        let jobs = DesktopJobs::spawn(backend);
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1040.0, 760.0])
                .with_min_inner_size([620.0, 480.0]),
            ..Default::default()
        };
        eframe::run_native(
            "NOTE4 控制台",
            options,
            Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc, jobs)))),
        )
        .map_err(|_| DesktopLaunchError)
    }
    #[cfg(not(windows))]
    {
        Err(DesktopLaunchError)
    }
}
