use super::*;
use crate::inkcards::{InkcardCoordinator, InkcardSettings, InkcardState};
use crate::install::{InstallManager, Installation, Package, windows::WindowsHost};
use crate::platform::background_windows::{BackgroundController, BackgroundStatus, OperationGuard};
use crate::*;
use std::path::Path;

struct Draft {
    id: DraftId,
    secret: Zeroizing<String>,
    devices: Vec<SetupDevice>,
    preview: Option<SetupPreview>,
    settings: Option<Settings>,
    companion: PathBuf,
}
pub struct WindowsDesktopBackend<C = ZectrixSetupCloud, K = OsCredentialStore> {
    data: PathBuf,
    package_dir: PathBuf,
    cloud: C,
    credentials: K,
    draft: Option<Draft>,
    initial_version: Option<String>,
}
impl WindowsDesktopBackend {
    pub fn new(data: PathBuf, package_dir: PathBuf) -> Result<Self, DesktopError> {
        let credentials = OsCredentialStore::zectrix_api_key()
            .map_err(|_| DesktopError::CredentialUnavailable)?;
        let cloud = ZectrixSetupCloud::new(
            std::env::var("CODEX_ZECTRIX_API_BASE")
                .unwrap_or_else(|_| "https://cloud.zectrix.com".into()),
        );
        Ok(Self::with_services(data, package_dir, cloud, credentials))
    }
}
impl<C: SetupCloud + Send, K: CredentialStore> WindowsDesktopBackend<C, K> {
    pub fn with_services(data: PathBuf, package_dir: PathBuf, cloud: C, credentials: K) -> Self {
        let initial_version = Installation::load(&data)
            .ok()
            .flatten()
            .map(|i| i.active_version);
        Self {
            data,
            package_dir,
            cloud,
            credentials,
            draft: None,
            initial_version,
        }
    }
    fn installation(&self) -> Result<Installation, DesktopError> {
        let installed = Installation::load(&self.data)
            .map_err(|_| DesktopError::LocalUnavailable)?
            .ok_or(DesktopError::NotInstalled)?;
        if installed.active_version != env!("CARGO_PKG_VERSION") {
            return Err(DesktopError::InstallationChanged);
        }
        Ok(installed)
    }
    fn guard(
        &self,
        allow_configuration_recovery: bool,
    ) -> Result<(OperationGuard, Installation), DesktopError> {
        let guard =
            OperationGuard::acquire(&self.data).map_err(|_| DesktopError::WorkerUnavailable)?;
        let install = self.installation()?;
        if ["installation-pending.json", "uninstall-pending.json"]
            .iter()
            .any(|n| self.data.join(n).exists())
            || (!allow_configuration_recovery
                && self.data.join("configuration-pending.json").exists())
        {
            return Err(DesktopError::RecoveryRequired);
        }
        Ok((guard, install))
    }
    fn refresh(&self) -> Result<DesktopStatus, DesktopError> {
        let installation =
            Installation::load(&self.data).map_err(|_| DesktopError::LocalUnavailable)?;
        let settings: Option<Settings> = load_json(&self.data, "settings.json", 8192)?;
        if settings.as_ref().is_some_and(|s| s.validate().is_err()) {
            return Err(DesktopError::LocalUnavailable);
        }
        let cards = InkcardSettings::load(self.data.join("inkcards.json"))
            .map_err(|_| DesktopError::LocalUnavailable)?;
        let mut theme = AppearanceSettings::load(self.data.join("appearance.json"))
            .map_err(|_| DesktopError::LocalUnavailable)?
            .theme;
        let needs_recovery = [
            "installation-pending.json",
            "configuration-pending.json",
            "uninstall-pending.json",
        ]
        .iter()
        .any(|n| self.data.join(n).exists());
        let (background, startup) = if let Some(install) = &installation {
            let exe = install.companion(&self.data);
            let bg = match BackgroundController::new(&self.data, &exe).status() {
                Ok(BackgroundStatus::Running) => DesktopBackground::Running,
                Ok(BackgroundStatus::Starting) => DesktopBackground::Starting,
                Ok(BackgroundStatus::Stopped) => DesktopBackground::Stopped,
                Err(_) => DesktopBackground::Unavailable,
            };
            (bg, Autostart::new(SystemCommandRunner, exe).status())
        } else {
            (DesktopBackground::Stopped, AutostartStatus::Absent)
        };
        let now =
            crate::platform::now_epoch_seconds().map_err(|_| DesktopError::LocalUnavailable)?;
        let snapshot = settings
            .as_ref()
            .and_then(|s| DesktopSnapshot::load(&self.data, s, now).ok().flatten())
            .filter(|_| !needs_recovery);
        let freshness = snapshot
            .as_ref()
            .map(|s| s.assess_freshness(now, background == DesktopBackground::Running))
            .unwrap_or(DesktopFreshness {
                quota: Freshness::Unavailable,
                secondary_quota: Freshness::Unavailable,
                task: Freshness::Unavailable,
            });
        let mut pages = Vec::new();
        let mut credentials_rejected = false;
        if let Some(settings) = &settings {
            let seed: StatusFramePublisherState =
                load_json(&self.data, "status-frame-state.json", 32768)?.unwrap_or_default();
            let layout = cards.resolved_pages(settings, theme);
            if layout.len() == 1 {
                theme = layout[0].theme;
            }
            let records = if cards.uses_coordinator() {
                let saved = InkcardState::load(self.data.join("inkcard-state.json"))
                    .ok()
                    .flatten();
                let validated = InkcardCoordinator::with_pages(settings, &seed, saved, &layout)
                    .map_err(|_| DesktopError::LocalUnavailable)?
                    .snapshot();
                credentials_rejected = validated.credentials_rejected;
                layout
                    .into_iter()
                    .map(|p| {
                        (
                            p.page,
                            p.theme,
                            validated.pages[usize::from(p.page - 1)].clone(),
                        )
                    })
                    .collect()
            } else {
                credentials_rejected = seed.credentials_rejected();
                vec![(settings.page_id, theme, seed)]
            };
            for (page, theme, record) in records {
                let uploaded_png = record
                    .last_successful_frame_sha256
                    .as_deref()
                    .and_then(|hash| load_uploaded_frame(&self.data, page, hash).ok().flatten());
                pages.push(PageStatus {
                    page,
                    theme,
                    record,
                    uploaded_png,
                });
            }
        }
        let read_only = installation
            .as_ref()
            .is_some_and(|i| i.active_version != env!("CARGO_PKG_VERSION"));
        Ok(DesktopStatus {
            installed_version: installation.map(|i| i.active_version),
            configured: settings.is_some(),
            needs_recovery,
            uninstall_pending: self.data.join("uninstall-pending.json").exists(),
            read_only,
            background,
            mode: cards.mode,
            theme,
            configured_page: if pages.len() == 1 {
                pages[0].page
            } else {
                settings.as_ref().map_or(1, |s| s.page_id)
            },
            device_label: settings
                .as_ref()
                .map(|s| format!("NOTE4 · 尾号 {}", suffix(&s.device_id)))
                .unwrap_or_else(|| "尚未配置".into()),
            snapshot,
            freshness,
            pages,
            startup,
            credentials_rejected,
        })
    }
    fn perform(
        &mut self,
        command: DesktopCommand,
        cancellation: &DraftCancellation,
    ) -> Result<DesktopReply, DesktopError> {
        match command {
            DesktopCommand::Refresh => Ok(DesktopReply::Status(Box::new(self.refresh()?))),
            DesktopCommand::CheckCodex => {
                crate::codex_discovery::CodexLocator::from_environment()
                    .resolve()
                    .map_err(|_| {
                        DesktopError::Operation(
                            "未找到 Codex。请安装 Windows 版 Codex 并登录后重试。".into(),
                        )
                    })?;
                let source = crate::CodexStatusSource::new(crate::AppServerClient::default());
                let warning = match source.read_quota_status() {
                    Ok(_) => None,
                    Err(crate::CodexStatusError::AccountUnavailable) => {
                        return Err(DesktopError::Operation(
                            "请先打开 Codex 并登录 ChatGPT 账号。".into(),
                        ));
                    }
                    Err(_) => Some("暂未能读取额度，可继续配置；后台会稍后重试。".into()),
                };
                Ok(DesktopReply::CodexChecked { warning })
            }
            DesktopCommand::Discover {
                draft_id,
                credential,
            } => {
                self.draft = None;
                let (_guard, install) = self.guard(true)?;
                let secret = match credential {
                    CredentialChoice::Saved => self
                        .credentials
                        .get()
                        .map_err(|_| DesktopError::CredentialUnavailable)?
                        .ok_or(DesktopError::CredentialUnavailable)?,
                    CredentialChoice::New(secret) => secret,
                };
                if secret.trim().is_empty() || secret.len() > 4096 {
                    return Err(DesktopError::Operation(
                        "请输入完整的 ZECTRIX API Key。".into(),
                    ));
                }
                let devices = self.cloud.discover(&secret).map_err(|e| {
                    setup_error(if e == SetupCloudError::KeyRejected {
                        SetupError::KeyRejected
                    } else {
                        SetupError::DiscoveryUnavailable
                    })
                })?;
                if cancellation.is_cancelled(draft_id) {
                    return Err(DesktopError::Cancelled);
                }
                if devices.is_empty() {
                    return Err(setup_error(SetupError::NoDevices));
                }
                let labels = devices
                    .iter()
                    .take(100)
                    .map(|d| {
                        format!(
                            "{} · 尾号 {}",
                            d.display_name()
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(32)
                                .collect::<String>(),
                            suffix(d.routing_id())
                        )
                    })
                    .collect();
                self.draft = Some(Draft {
                    id: draft_id,
                    secret,
                    devices: devices.into_iter().take(100).collect(),
                    preview: None,
                    settings: None,
                    companion: install.companion(&self.data),
                });
                Ok(DesktopReply::Devices { draft_id, labels })
            }
            DesktopCommand::Prepare {
                draft_id,
                device_index,
                page_id,
            } => {
                self.installation()?;
                let draft = self
                    .draft
                    .as_mut()
                    .filter(|d| d.id == draft_id)
                    .ok_or(DesktopError::DraftExpired)?;
                let device = draft
                    .devices
                    .get(device_index)
                    .ok_or(DesktopError::DraftExpired)?;
                let settings = Settings {
                    device_id: device.routing_id().into(),
                    page_id,
                };
                settings
                    .validate()
                    .map_err(|_| setup_error(SetupError::SelectionInvalid))?;
                let preview = DeterministicSetupRenderer
                    .render_sample()
                    .map_err(|_| setup_error(SetupError::PreviewUnavailable))?;
                let png = preview.png().to_vec();
                draft.settings = Some(settings);
                draft.preview = Some(preview);
                Ok(DesktopReply::Preview { draft_id, png })
            }
            DesktopCommand::Commit { draft_id } => {
                self.installation()?;
                let draft = self
                    .draft
                    .take()
                    .filter(|d| d.id == draft_id)
                    .ok_or(DesktopError::DraftExpired)?;
                let previous = crate::desktop_state::read_bounded(
                    &self.data.join("pending-preview.png"),
                    131072,
                )
                .map_err(|_| DesktopError::LocalUnavailable)?;
                let prepared = PreparedSetup::new(
                    draft.settings.ok_or(DesktopError::DraftExpired)?,
                    draft.preview.ok_or(DesktopError::DraftExpired)?,
                    draft.secret,
                    previous,
                )
                .map_err(setup_error)?;
                let mut files = FilesystemSetupStore::new(&self.data)
                    .map_err(|_| DesktopError::LocalUnavailable)?;
                let mut autostart = Autostart::new(SystemCommandRunner, &draft.companion);
                let mut control =
                    crate::operator::WindowsSetupControl::new(&self.data, &draft.companion);
                let report = commit_prepared_setup(
                    prepared,
                    &mut self.cloud,
                    &self.credentials,
                    &mut files,
                    &mut autostart,
                    &SystemSetupClock,
                    &mut control,
                )
                .map_err(setup_error)?;
                let warning = report
                    .background_warning
                    .map(crate::operator::friendly_setup_error)
                    .or_else(|| {
                        (report.outcome == SetupOutcome::ConfiguredWithAutostartWarning).then(
                            || "配置已保存并上传；登录启动未设置成功，可在设置页重试。".into(),
                        )
                    });
                Ok(DesktopReply::Completed(OperationReport {
                    kind: OperationKind::Configured,
                    warning,
                }))
            }
            DesktopCommand::Start | DesktopCommand::Stop | DesktopCommand::SetStartup(_) => {
                let (guard, install) = self.guard(false)?;
                let exe = install.companion(&self.data);
                let ctl = BackgroundController::new(&self.data, &exe);
                match command {
                    DesktopCommand::Start => {
                        Package::load(&install.directory(&self.data))
                            .map_err(|_| DesktopError::LocalUnavailable)?;
                        ctl.start_guarded(&guard)
                            .map_err(|_| DesktopError::WorkerUnavailable)?;
                    }
                    DesktopCommand::Stop => {
                        ctl.stop().map_err(|_| DesktopError::WorkerUnavailable)?
                    }
                    DesktopCommand::SetStartup(enabled) => {
                        let mut startup = Autostart::new(SystemCommandRunner, exe);
                        (if enabled {
                            startup.install()
                        } else {
                            startup.uninstall()
                        })
                        .map_err(|_| {
                            DesktopError::Operation("登录启动设置失败，请检查安装后重试。".into())
                        })?;
                    }
                    _ => unreachable!(),
                }
                Ok(DesktopReply::Completed(OperationReport::updated()))
            }
            DesktopCommand::SetTheme(theme) => {
                self.installation()?;
                crate::operator::save_theme(&self.data, theme).map_err(|_| {
                    DesktopError::Operation("主题未能保存，请检查本地配置或完成恢复。".into())
                })?;
                Ok(DesktopReply::Completed(OperationReport::updated()))
            }
            DesktopCommand::SetMode(mode) => {
                self.installation()?;
                crate::operator::save_inkcard_mode(&self.data, mode).map_err(|_| {
                    DesktopError::Operation(
                        "墨卡模式操作未完成，请查看当前模式和后台状态后重试。".into(),
                    )
                })?;
                Ok(DesktopReply::Completed(OperationReport::updated()))
            }
            DesktopCommand::SetCards(cards) => {
                self.installation()?;
                let report =
                    crate::operator::save_inkcard_settings(&self.data, cards).map_err(|_| {
                        DesktopError::Operation(
                            "墨卡排列未能保存，请查看当前配置和后台状态后重试。".into(),
                        )
                    })?;
                Ok(DesktopReply::Completed(OperationReport {
                    kind: OperationKind::CardsSaved,
                    warning: report.restart_failed.then(|| {
                        "排列已保存，但后台重启失败。请到运行总览点击“启动后台”，无需重新保存。"
                            .into()
                    }),
                }))
            }
            DesktopCommand::Install { package_dir } => {
                let package_dir = if package_dir.as_os_str().is_empty() {
                    &self.package_dir
                } else {
                    &package_dir
                };
                let package = Package::load(package_dir).map_err(|_| {
                    DesktopError::Operation("发布包校验失败，请重新下载并完整解压 ZIP。".into())
                })?;
                if package.manifest.version != env!("CARGO_PKG_VERSION") {
                    return Err(DesktopError::InstallationChanged);
                }
                InstallManager::new(&self.data, WindowsHost::new(&self.data))
                    .install_if_current(package_dir, self.initial_version.as_deref())
                    .map_err(|e| {
                        if matches!(e, crate::install::InstallError::InstallationChanged) {
                            DesktopError::InstallationChanged
                        } else {
                            DesktopError::Operation(install_message(e).into())
                        }
                    })?;
                self.initial_version = Some(env!("CARGO_PKG_VERSION").into());
                Ok(DesktopReply::Completed(OperationReport {
                    kind: OperationKind::Installed,
                    warning: crate::platform::shortcut_windows::ensure_installed_shortcut(
                        &self.data,
                    ),
                }))
            }
            DesktopCommand::Uninstall {
                delete_data,
                delete_key,
            } => {
                self.installation()?;
                self.draft = None;
                crate::operator::uninstall_at(&self.data, delete_data, delete_key).map_err(
                    |_| {
                        DesktopError::Operation(
                            "卸载尚未完成，请关闭其他控制台后继续卸载；若程序文件已被部分清理，请从完整 ZIP 打开控制台重试。".into(),
                        )
                    },
                )?;
                Ok(DesktopReply::Completed(OperationReport {
                    kind: OperationKind::UninstallPending,
                    warning: None,
                }))
            }
            DesktopCommand::Diagnose => {
                let status = self.refresh()?;
                let mut lines = vec![
                    format!("控制台版本：{}", env!("CARGO_PKG_VERSION")),
                    format!("后台状态：{:?}", status.background),
                    format!(
                        "额度：{}；任务：{}",
                        status.freshness.quota.label(),
                        status.freshness.task.label()
                    ),
                    format!(
                        "待恢复操作：{}",
                        if status.needs_recovery { "有" } else { "无" }
                    ),
                ];
                if let Some(bytes) =
                    crate::desktop_state::read_bounded(&self.data.join("companion.log"), 65536)
                        .map_err(|_| DesktopError::LocalUnavailable)?
                {
                    for line in String::from_utf8_lossy(&bytes).lines().rev().take(12) {
                        lines.push(
                            match line {
                                "credentials_rejected" => "云端拒绝 Key，请重新配置。",
                                "source_account_unavailable" => {
                                    "Codex 账号暂不可用，请确认已登录。"
                                }
                                "source_unavailable"
                                | "source_method_unavailable"
                                | "source_incompatible" => "数据源暂不可用或需要兼容更新。",
                                "publish_unavailable" => "云端上传失败，后台按间隔重试。",
                                "state_persist_unavailable" => "本地状态或预览缓存保存失败。",
                                "invalid_configuration" | "setup_required" => "需要检查设备配置。",
                                "credential_unavailable" => "无法读取已保存的 Key。",
                                _ => continue,
                            }
                            .into(),
                        );
                    }
                }
                Ok(DesktopReply::Diagnostics(lines))
            }
        }
    }
}
impl<C: SetupCloud + Send, K: CredentialStore> DesktopBackend for WindowsDesktopBackend<C, K> {
    fn execute(
        &mut self,
        command: DesktopCommand,
        cancellation: &DraftCancellation,
    ) -> DesktopReply {
        let draft = command.draft_id();
        let result = if draft.is_some_and(|id| cancellation.is_cancelled(id)) {
            Err(DesktopError::Cancelled)
        } else {
            self.perform(command, cancellation)
        };
        match result {
            Ok(reply) => reply,
            Err(error) => {
                if draft.is_some() {
                    self.draft = None;
                }
                DesktopReply::Failed(error)
            }
        }
    }
    fn clear_draft(&mut self, draft_id: DraftId) {
        if self.draft.as_ref().is_some_and(|d| d.id == draft_id) {
            self.draft = None;
        }
    }
}
fn setup_error(e: SetupError) -> DesktopError {
    DesktopError::Operation(crate::operator::friendly_setup_error(e))
}
fn suffix(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .filter(|c| !c.is_control())
        .collect()
}
fn load_json<T: serde::de::DeserializeOwned>(
    data: &Path,
    name: &str,
    limit: usize,
) -> Result<Option<T>, DesktopError> {
    crate::desktop_state::read_bounded(&data.join(name), limit)
        .map_err(|_| DesktopError::LocalUnavailable)?
        .map(|b| serde_json::from_slice(&b).map_err(|_| DesktopError::LocalUnavailable))
        .transpose()
}
fn install_message(e: crate::install::InstallError) -> &'static str {
    use crate::install::InstallError::*;
    match e {
        InstallationChanged => "程序版本已变化，请打开新版控制台。",
        InvalidPackage => "发布包不完整或校验失败，请重新下载完整 ZIP。",
        InvalidState => "安装记录损坏，未修改现有安装。",
        LegacyStop => "旧版启动项需要系统授权，请完成授权后重试安装。",
        Health => "新版后台检查失败，已保留或尝试恢复原版。",
        Recovery => "自动恢复尚未完成，请重新运行安装。",
        Io(_) => "安装文件读写失败，请检查目录权限和剩余空间。",
        Background(_) => "后台暂时无法停止或已有操作正在进行，请稍后重试。",
    }
}
