use super::*;
use zeroize::Zeroize;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ConsolePage {
    #[default]
    Overview,
    Cards,
    Device,
    Settings,
}
impl ConsolePage {
    pub const ALL: [Self; 4] = [Self::Overview, Self::Cards, Self::Device, Self::Settings];
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "运行总览",
            Self::Cards => "墨卡与主题",
            Self::Device => "设备配置",
            Self::Settings => "设置与诊断",
        }
    }
}
pub enum UiAction {
    SelectPage(u8),
    SelectTheme(BoardTheme),
    ApplyTheme,
    RequestMode(InkcardMode),
    ConfirmMode,
    SaveCards,
    CheckCodex,
    Discover,
    Prepare,
    ConfirmSetup,
    Start,
    Stop,
    SetStartup(bool),
    Install,
    ConfirmUninstall,
    Diagnose,
    Close,
}
pub struct AppModel {
    pub page: ConsolePage,
    pub status: Option<Box<DesktopStatus>>,
    pub selected_page: u8,
    pub selected_theme: BoardTheme,
    pub pending_mode: Option<InkcardMode>,
    pub cards: CardEditor,
    pub show_samples: bool,
    pub sample_dual: bool,
    pub use_saved_key: bool,
    pub secret: Zeroizing<String>,
    pub draft_id: Option<DraftId>,
    next_draft: u64,
    pub devices: Vec<String>,
    pub device_index: usize,
    pub target_page: u8,
    pub setup_preview: Option<Vec<u8>>,
    pub codex_checked: bool,
    pub configuration_saved: bool,
    pub notice: Option<String>,
    pub error: bool,
    refresh_failed: bool,
    pub diagnostics: Vec<String>,
    pub delete_data: bool,
    pub delete_key: bool,
    pub confirm_uninstall: bool,
    pub should_close: bool,
}
impl Default for AppModel {
    fn default() -> Self {
        Self {
            page: ConsolePage::Overview,
            status: None,
            selected_page: 1,
            selected_theme: BoardTheme::default(),
            pending_mode: None,
            cards: CardEditor::default(),
            show_samples: false,
            sample_dual: false,
            use_saved_key: true,
            secret: Zeroizing::new(String::new()),
            draft_id: None,
            next_draft: 1,
            devices: vec![],
            device_index: 0,
            target_page: 1,
            setup_preview: None,
            codex_checked: false,
            configuration_saved: false,
            notice: None,
            error: false,
            refresh_failed: false,
            diagnostics: vec![],
            delete_data: false,
            delete_key: false,
            confirm_uninstall: false,
            should_close: false,
        }
    }
}
impl AppModel {
    pub fn age_status(&mut self, now: i64) {
        if let Some(status) = &mut self.status {
            status.freshness = status
                .snapshot
                .as_ref()
                .map(|s| s.assess_freshness(now, status.background == DesktopBackground::Running))
                .unwrap_or(DesktopFreshness {
                    quota: Freshness::Unavailable,
                    secondary_quota: Freshness::Unavailable,
                    task: Freshness::Unavailable,
                });
        }
    }
    pub fn cancel_draft(&mut self) -> Option<DraftId> {
        self.secret.zeroize();
        self.devices.clear();
        self.setup_preview = None;
        self.draft_id.take()
    }
    pub fn request_action(&mut self, action: UiAction) -> Option<DesktopCommand> {
        match action {
            UiAction::SelectPage(page) => {
                if (1..=5).contains(&page) {
                    self.selected_page = page;
                }
                None
            }
            UiAction::SelectTheme(theme) => {
                self.selected_theme = theme;
                None
            }
            UiAction::ApplyTheme => Some(DesktopCommand::SetTheme(self.selected_theme)),
            UiAction::RequestMode(mode) => {
                self.pending_mode = Some(mode);
                None
            }
            UiAction::ConfirmMode => self.pending_mode.take().map(DesktopCommand::SetMode),
            UiAction::SaveCards => self
                .cards
                .configuration()
                .ok()
                .map(DesktopCommand::SetCards),
            UiAction::CheckCodex => Some(DesktopCommand::CheckCodex),
            UiAction::Discover => {
                self.devices.clear();
                self.setup_preview = None;
                let draft_id = DraftId(self.next_draft);
                self.next_draft += 1;
                self.draft_id = Some(draft_id);
                let credential = if self.use_saved_key {
                    self.secret.zeroize();
                    CredentialChoice::Saved
                } else {
                    CredentialChoice::New(std::mem::take(&mut self.secret))
                };
                Some(DesktopCommand::Discover {
                    draft_id,
                    credential,
                })
            }
            UiAction::Prepare => self.draft_id.map(|draft_id| DesktopCommand::Prepare {
                draft_id,
                device_index: self.device_index,
                page_id: self.target_page,
            }),
            UiAction::ConfirmSetup => self
                .setup_preview
                .as_ref()
                .and(self.draft_id)
                .map(|draft_id| DesktopCommand::Commit { draft_id }),
            UiAction::Start => Some(DesktopCommand::Start),
            UiAction::Stop => Some(DesktopCommand::Stop),
            UiAction::SetStartup(value) => Some(DesktopCommand::SetStartup(value)),
            UiAction::Install => Some(DesktopCommand::Install {
                package_dir: PathBuf::new(),
            }),
            UiAction::ConfirmUninstall if self.confirm_uninstall => {
                Some(DesktopCommand::Uninstall {
                    delete_data: self.delete_data,
                    delete_key: self.delete_key,
                })
            }
            UiAction::ConfirmUninstall => None,
            UiAction::Diagnose => Some(DesktopCommand::Diagnose),
            UiAction::Close => None,
        }
    }
    pub fn apply_reply(&mut self, _: JobId, reply: DesktopReply) {
        match reply {
            DesktopReply::Status(status) => {
                self.cards.load_status(&status);
                if self.refresh_failed {
                    self.error = false;
                    self.notice = None;
                    self.refresh_failed = false;
                }
                if self.status.is_none() {
                    self.selected_theme = status.theme;
                    self.selected_page = status.configured_page;
                    self.target_page = status.configured_page;
                    self.use_saved_key = status.configured;
                    if status.installed_version.is_some() && !status.configured && !status.read_only
                    {
                        self.page = ConsolePage::Device;
                    }
                }
                self.status = Some(status);
            }
            DesktopReply::Devices { draft_id, labels } if self.draft_id == Some(draft_id) => {
                self.devices = labels;
                self.device_index = 0;
                self.error = false;
                self.notice = Some("已找到设备，请选择墨卡页面并生成预览。".into());
            }
            DesktopReply::Preview { draft_id, png } if self.draft_id == Some(draft_id) => {
                self.setup_preview = Some(png);
                self.error = false;
                self.notice = None;
            }
            DesktopReply::Devices { .. } | DesktopReply::Preview { .. } => {}
            DesktopReply::CodexChecked { warning } => {
                self.codex_checked = true;
                self.notice = warning.or_else(|| Some("Codex 登录检查通过。".into()));
                self.error = false;
            }
            DesktopReply::Completed(report) => {
                self.error = false;
                let text = match report.kind {
                    OperationKind::Configured => {
                        self.cancel_draft();
                        self.configuration_saved = true;
                        self.page = ConsolePage::Overview;
                        "配置已保存，测试图已上传。设备同步后刷新。"
                    }
                    OperationKind::Installed => {
                        self.page = if self.status.as_ref().is_some_and(|s| s.configured) {
                            ConsolePage::Overview
                        } else {
                            ConsolePage::Device
                        };
                        "安装已完成。已有配置会继续保留。"
                    }
                    OperationKind::Updated => "操作已完成。设备画面按同步周期刷新。",
                    OperationKind::CardsSaved => {
                        self.cards.dirty = false;
                        "墨卡排列已保存。后台运行时会按新配置更新，设备画面按同步周期刷新。"
                    }
                    OperationKind::UninstallPending => {
                        self.should_close = true;
                        "卸载已开始，窗口关闭后完成清理。"
                    }
                };
                self.notice = Some(
                    report
                        .warning
                        .map(|w| format!("{text}\n{w}"))
                        .unwrap_or_else(|| text.into()),
                );
            }
            DesktopReply::Diagnostics(lines) => {
                self.diagnostics = lines;
                self.error = false;
            }
            DesktopReply::Failed(error) => {
                self.secret.zeroize();
                self.error = error != DesktopError::Cancelled;
                self.notice = Some(error.to_string());
            }
            DesktopReply::RefreshFailed(error) => {
                if let Some(status) = &mut self.status {
                    status.background = DesktopBackground::Unavailable;
                    status.startup = AutostartStatus::Unavailable;
                    status.pages.clear();
                }
                self.age_status(crate::platform::now_epoch_seconds().unwrap_or(i64::MAX));
                self.refresh_failed = true;
                self.error = true;
                self.notice = Some(format!("状态读取失败，下面仅显示上次数据。{error}"));
            }
        }
    }
}
