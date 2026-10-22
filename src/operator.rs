use crate::install::{InstallManager, Installation, Package, windows::WindowsHost};
use crate::platform::background_windows::{
    BackgroundController, BackgroundStatus, OperationGuard, atomic_write, reject_link,
};
use crate::{
    Autostart, AutostartStatus, ConsoleTerminal, CredentialStore, OsCredentialStore, Settings,
    SetupCommitState, SetupControl, SetupError, SetupTerminal, SystemCommandRunner,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct WindowsSetupControl {
    directory: PathBuf,
    executable: PathBuf,
    guard: Option<OperationGuard>,
    was_running: bool,
    previously_uncertain: bool,
}
impl WindowsSetupControl {
    pub fn new(directory: &Path, executable: &Path) -> Self {
        Self {
            directory: directory.into(),
            executable: executable.into(),
            guard: None,
            was_running: false,
            previously_uncertain: false,
        }
    }
}
impl SetupControl for WindowsSetupControl {
    fn begin(&mut self) -> std::result::Result<(), SetupError> {
        let guard = OperationGuard::acquire(&self.directory)
            .map_err(|_| SetupError::BackgroundUnavailable)?;
        if Installation::load(&self.directory)
            .map_err(|_| SetupError::LocalStateUnavailable)?
            .is_none_or(|current| current.companion(&self.directory) != self.executable)
        {
            return Err(SetupError::InstallationChanged);
        }
        if self.directory.join("installation-pending.json").exists()
            || self.directory.join("uninstall-pending.json").exists()
        {
            return Err(SetupError::LocalStateUnavailable);
        }
        let ctl = BackgroundController::new(&self.directory, &self.executable);
        self.was_running = ctl
            .status()
            .map_err(|_| SetupError::BackgroundUnavailable)?
            != BackgroundStatus::Stopped;
        ctl.stop().map_err(|_| SetupError::BackgroundUnavailable)?;
        self.previously_uncertain = self.directory.join("configuration-pending.json").exists();
        // Never write the previous or proposed secret into a recovery journal.
        if atomic_write(
            &self.directory.join("configuration-pending.json"),
            b"{\"schemaVersion\":1,\"requiresConfiguration\":true}",
        )
        .is_err()
        {
            if self.was_running && !self.previously_uncertain {
                let _ = ctl.start_guarded(&guard);
            }
            return Err(SetupError::LocalStateUnavailable);
        }
        self.guard = Some(guard);
        Ok(())
    }
    fn finish(&mut self, state: SetupCommitState) -> std::result::Result<(), SetupError> {
        let guard = self.guard.take().ok_or(SetupError::LocalStateUnavailable)?;
        if state == SetupCommitState::Uncertain
            || (state == SetupCommitState::Restored && self.previously_uncertain)
        {
            return Ok(());
        }
        fs::remove_file(self.directory.join("configuration-pending.json"))
            .map_err(|_| SetupError::LocalStateUnavailable)?;
        if state == SetupCommitState::Configured || self.was_running {
            BackgroundController::new(&self.directory, &self.executable)
                .start_guarded(&guard)
                .map_err(|_| SetupError::BackgroundStartUnavailable)?;
        }
        Ok(())
    }
}
pub fn installed_companion() -> Result<PathBuf> {
    let data = crate::platform::data_dir()?;
    let install = Installation::load(&data)?.ok_or("请先选择安装/升级")?;
    Package::load(&install.directory(&data))?;
    Ok(install.companion(&data))
}

fn configure() -> Result<()> {
    crate::codex_discovery::CodexLocator::from_environment().resolve()?;
    let source = crate::CodexStatusSource::new(crate::AppServerClient::default());
    if let Err(error) = source.read_quota_status() {
        match error {
            crate::CodexStatusError::AccountUnavailable => {
                return Err("请先打开 Windows 版 Codex 并登录 ChatGPT 账号，然后重新配置".into());
            }
            _ => println!("目前未能读取 Codex 额度。设备配置可继续，后台会稍后重试。"),
        }
    }
    crate::run_setup().map_err(friendly_setup_error)?;
    Ok(())
}
pub fn friendly_setup_error(error: SetupError) -> String {
    match error {
        SetupError::SecretUnavailable => "未读取到 Key，请重新输入（输入时不会显示字符）".into(),
        SetupError::DiscoveryUnavailable => {
            "无法获取设备列表，请检查网络、ZECTRIX 服务及 Key 后重试".into()
        }
        SetupError::SelectionInvalid => "选择无效或输入已结束，请按列表中的编号重试".into(),
        SetupError::UploadRejected => "设备拒绝了上传，请检查 Key、设备在线状态及页面编号".into(),
        SetupError::UploadResultUnknown => {
            "上传结果暂不确定，请先查看屏幕；网络恢复后可重试".into()
        }
        SetupError::CredentialPersistUnavailable => {
            "无法保存 API Key，请检查 Windows 凭据管理器后重试".into()
        }
        SetupError::SettingsPersistUnavailable | SetupError::LocalStateUnavailable => {
            "无法读写本地配置，请检查目录权限和可用磁盘空间".into()
        }
        SetupError::PreviewUnavailable => "无法生成测试图，请检查目录权限".into(),
        SetupError::RollbackUnavailable
        | SetupError::StatePersistFailedScreenMayHaveChangedRollbackIncomplete
        | SetupError::AutostartFailedScreenMayHaveChangedRollbackIncomplete => {
            "屏幕可能已更新，但本地恢复未完成；请重新配置后再启动后台".into()
        }
        SetupError::StatePersistFailedScreenMayHaveChanged => {
            "屏幕可能已更新，但本地保存失败；已尝试恢复旧配置，请检查磁盘后重试".into()
        }
        SetupError::CompletionNoticeFailedScreenMayHaveChanged => {
            "配置已完成，但无法显示完成提示".into()
        }
        SetupError::AutostartUnavailable | SetupError::AutostartFailedScreenMayHaveChanged => {
            "开机启动未能设置，请在菜单中重试".into()
        }
        other => other.to_string(),
    }
}
pub fn install_current_package() -> Result<()> {
    let data = crate::platform::data_dir()?;
    let current = std::env::current_exe()?;
    let package = current.parent().ok_or("无法确定发布包目录")?;
    let mut manager = InstallManager::new(&data, WindowsHost::new(&data));
    manager.install_or_update(package)?;
    if let Some(warning) = crate::platform::shortcut_windows::ensure_installed_shortcut(&data) {
        println!("{warning}");
    }
    println!("安装已完成。程序存放于：{}", data.join("app").display());
    Ok(())
}
pub fn start_background() -> Result<()> {
    let data = crate::platform::data_dir()?;
    let exe = installed_companion()?;
    BackgroundController::new(&data, &exe).start()?;
    println!("后台已启动，关闭此窗口后仍会运行。");
    Ok(())
}
pub fn stop_background() -> Result<()> {
    let data = crate::platform::data_dir()?;
    let exe = installed_companion()?;
    let _guard = OperationGuard::acquire(&data)?;
    BackgroundController::new(&data, &exe).stop()?;
    println!("后台已停止，屏幕会保留最后一张画面。");
    Ok(())
}
pub fn show_local_status() -> Result<()> {
    let data = crate::platform::data_dir()?;
    let Some(install) = Installation::load(&data)? else {
        println!("尚未安装，请选择安装/升级。");
        return Ok(());
    };
    let exe = install.companion(&data);
    let state = BackgroundController::new(&data, &exe).status()?;
    println!(
        "版本：{}\n后台：{}",
        install.active_version,
        match state {
            BackgroundStatus::Running => "运行中",
            BackgroundStatus::Starting => "正在启动",
            BackgroundStatus::Stopped => "已停止",
        }
    );
    let configured = Settings::load(data.join("settings.json"))?.is_some();
    println!("设备配置：{}", if configured { "已保存" } else { "未配置" });
    let cards = crate::inkcards::InkcardSettings::load(data.join("inkcards.json"))?;
    let all_cards = cards.uses_coordinator();
    println!(
        "墨卡模式：{}",
        if cards.pages.is_some() {
            "自定义排列（在图形控制台配置）"
        } else if all_cards {
            "五张自动更新（1 怪盗 / 2 EVA / 3 掌机 / 4 仪表 / 5 HUD）"
        } else {
            "单页自动更新"
        }
    );
    match crate::AppearanceSettings::load(data.join("appearance.json")) {
        Ok(appearance) => println!(
            "{}：{}",
            if all_cards {
                "单页模式预选主题"
            } else {
                "主题"
            },
            appearance.theme.label()
        ),
        Err(_) => println!("主题设置损坏，请选 8 重新选择；后台保留已读取的主题"),
    }
    if data.join("configuration-pending.json").exists() {
        println!("上次配置中断。为避免使用不一致的设备和 Key，后台暂不上传，请重新配置。");
    }
    let startup = Autostart::new(SystemCommandRunner, exe).status();
    println!(
        "开机启动：{}",
        match startup {
            AutostartStatus::Present => "已开启",
            AutostartStatus::Absent => "已关闭",
            AutostartStatus::Unavailable => "未能确认",
        }
    );
    if let Ok(Some(runtime)) = crate::RuntimeStatus::load(data.join("runtime-status.json")) {
        println!(
            "最近额度读取：{}\n最近内容上传：{}",
            format_time(runtime.last_successful_quota_refresh_epoch_seconds),
            format_time(runtime.last_successful_upload_epoch_seconds)
        );
    }
    if all_cards {
        if let Ok(Some(state)) =
            crate::inkcards::InkcardState::load(data.join("inkcard-state.json"))
        {
            for (index, page) in state.pages.iter().enumerate() {
                let theme = if let Some(layout) = &cards.pages {
                    let Some(route) = layout.iter().find(|p| usize::from(p.page) == index + 1)
                    else {
                        continue;
                    };
                    route.theme
                } else {
                    crate::inkcards::PAGE_THEMES[index]
                };
                println!(
                    "墨卡 {} {}：最近传图 {}；连续失败 {}",
                    index + 1,
                    theme.label(),
                    format_time(page.last_successful_http_upload_epoch_seconds),
                    page.consecutive_failure_count()
                );
            }
            if state.credentials_rejected {
                println!("Key 已被拒绝，墨卡已暂停上传，请选 2 重新配置。");
            }
        }
    } else if let Ok(Some(state)) =
        crate::StatusFramePublisherState::load(data.join("status-frame-state.json"))
    {
        println!(
            "最近传图成功：{}",
            format_time(state.last_successful_http_upload_epoch_seconds)
        );
    }
    println!("诊断日志：{}", data.join("companion.log").display());
    Ok(())
}
fn format_time(value: Option<i64>) -> String {
    value
        .and_then(|v| time::OffsetDateTime::from_unix_timestamp(v).ok())
        .map(|v| {
            v.to_offset(time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC))
                .to_string()
        })
        .unwrap_or_else(|| "暂无".into())
}
fn toggle_startup() -> Result<()> {
    let data = crate::platform::data_dir()?;
    let exe = installed_companion()?;
    let _guard = OperationGuard::acquire(&data)?;
    let mut startup = Autostart::new(SystemCommandRunner, exe);
    match startup.status() {
        AutostartStatus::Present => {
            startup.uninstall()?;
            println!("开机启动已关闭。");
        }
        AutostartStatus::Absent => {
            startup.install()?;
            println!("开机启动已开启。");
        }
        AutostartStatus::Unavailable => {
            return Err("启动项无法核实，请查看安装路径或重新安装".into());
        }
    }
    Ok(())
}

pub fn save_theme(data: &Path, theme: crate::status_board::BoardTheme) -> Result<()> {
    let _guard = OperationGuard::acquire(data)?;
    for file in [
        "installation-pending.json",
        "configuration-pending.json",
        "uninstall-pending.json",
    ] {
        if data.join(file).exists() {
            return Err("请先完成安装、配置或卸载恢复，再切换主题".into());
        }
    }
    let installation = Installation::load(data)?.ok_or("请先选择 1 安装/升级")?;
    if installation.active_version != env!("CARGO_PKG_VERSION") {
        return Err("请先选择 1 安装/升级，再切换主题".into());
    }
    if crate::inkcards::InkcardSettings::load(data.join("inkcards.json"))?
        .pages
        .is_some()
    {
        return Err("当前使用自定义墨卡排列，请在图形控制台的墨卡与主题页修改风格".into());
    }
    crate::AppearanceSettings { theme }.save_atomically(data.join("appearance.json"))?;
    Ok(())
}
pub fn set_theme(theme: crate::status_board::BoardTheme) -> Result<()> {
    let data = crate::platform::data_dir()?;
    save_theme(&data, theme)?;
    if crate::inkcards::InkcardSettings::load(data.join("inkcards.json"))?.mode
        == crate::inkcards::InkcardMode::All
    {
        println!(
            "已保存单页模式主题：{}。当前五张墨卡分别使用固定风格，可在设备上翻页；菜单 9 可切回单页模式。",
            theme.label()
        );
        return Ok(());
    }
    println!(
        "已保存主题：{}。运行中的后台会在下次观察时应用；实体屏幕按设备同步周期刷新。",
        theme.label()
    );
    Ok(())
}

pub fn save_inkcard_mode(data: &Path, mode: crate::inkcards::InkcardMode) -> Result<()> {
    let report =
        save_inkcard_settings(data, crate::inkcards::InkcardSettings { mode, pages: None })?;
    if report.restart_failed {
        return Err("墨卡模式已保存，但后台未能重启，请选 3 启动后台".into());
    }
    Ok(())
}

pub struct InkcardSaveReport {
    pub restart_failed: bool,
}

fn finish_inkcard_save(saved: Result<()>, restart: Result<()>) -> Result<InkcardSaveReport> {
    saved?;
    Ok(InkcardSaveReport {
        restart_failed: restart.is_err(),
    })
}

#[cfg(test)]
mod inkcard_save_tests {
    use super::*;
    #[test]
    fn committed_layout_survives_restart_failure_as_saved_with_warning() {
        let report = finish_inkcard_save(Ok(()), Err("worker restart failed".into()))
            .expect("already committed preferences must not be reported as unsaved");
        assert!(report.restart_failed);
        assert!(!finish_inkcard_save(Ok(()), Ok(())).unwrap().restart_failed);
        assert!(finish_inkcard_save(Err("write failed".into()), Ok(())).is_err());
    }
}

pub fn save_inkcard_settings(
    data: &Path,
    cards: crate::inkcards::InkcardSettings,
) -> Result<InkcardSaveReport> {
    use crate::inkcards::{InkcardCoordinator, InkcardState};
    cards.validate()?;
    let guard = OperationGuard::acquire(data)?;
    for file in [
        "installation-pending.json",
        "configuration-pending.json",
        "uninstall-pending.json",
    ] {
        if data.join(file).exists() {
            return Err("请先完成安装、配置或卸载恢复，再切换墨卡模式".into());
        }
    }
    let installation = Installation::load(data)?.ok_or("请先选择 1 安装/升级")?;
    if installation.active_version != env!("CARGO_PKG_VERSION") {
        return Err("请先安装/升级当前版本".into());
    }
    let settings = Settings::load(data.join("settings.json"))?.ok_or("请先配置设备")?;
    let ctl = BackgroundController::new(data, &installation.companion(data));
    let was_running = ctl.status()? != BackgroundStatus::Stopped;
    ctl.stop()?;
    let result: Result<()> = (|| {
        let state_path = data.join("status-frame-state.json");
        reject_link(&state_path)?;
        let mut seed = crate::StatusFramePublisherState::load(&state_path)?.unwrap_or_default();
        // Selecting a mode explicitly repairs corrupt derived multi-page state, but never
        // follows links or conceals filesystem errors. Keep the last HTTP cooldown.
        let saved = match InkcardState::load(data.join("inkcard-state.json")) {
            Ok(value) => value,
            Err(crate::SettingsError::Decode) => None,
            Err(error) => return Err(error.into()),
        };
        seed.last_successful_http_upload_epoch_seconds = seed
            .last_successful_http_upload_epoch_seconds
            .into_iter()
            .chain(saved.as_ref().and_then(InkcardState::latest_http_upload))
            .max();
        seed.last_successful_content_fingerprint = None;
        seed.last_successful_frame_sha256 = None;
        seed.failure_count = 0;
        seed.save_atomically(&state_path)?;
        if cards.uses_coordinator() {
            InkcardCoordinator::with_pages(
                &settings,
                &seed,
                None,
                &cards.resolved_pages(&settings, crate::status_board::BoardTheme::default()),
            )?
            .snapshot()
            .save_atomically(data.join("inkcard-state.json"))?;
        }
        cards.save_atomically(data.join("inkcards.json"))?;
        Ok(())
    })();
    // Even when a preference write fails, restore the user's previously running worker.
    let restart = if was_running {
        ctl.start_guarded(&guard).map(|_| ())
    } else {
        Ok(())
    };
    finish_inkcard_save(result, restart.map_err(Into::into))
}
pub fn set_inkcard_mode(mode: crate::inkcards::InkcardMode) -> Result<()> {
    save_inkcard_mode(&crate::platform::data_dir()?, mode)?;
    println!(
        "已保存：{}。原先运行的后台已重新启动；原先停止的后台请选 3 启动。",
        if mode == crate::inkcards::InkcardMode::All {
            "五张墨卡自动更新：1 怪盗、2 EVA、3 掌机、4 仪表、5 HUD"
        } else {
            "仅自动更新配置时选择的页面"
        }
    );
    Ok(())
}
fn choose_inkcard_mode(terminal: &mut impl SetupTerminal) -> Result<()> {
    loop {
        let Some(answer) = terminal
            .read_line(
                "墨卡模式：1 五张都自动更新（替换设备墨卡 1–5 的内容），2 只更新配置页，0 取消：",
            )
            .map_err(|_| "无法读取输入")?
        else {
            return Ok(());
        };
        match answer.trim() {
            "0" => return Ok(()),
            "1" => return set_inkcard_mode(crate::inkcards::InkcardMode::All),
            "2" => return set_inkcard_mode(crate::inkcards::InkcardMode::Single),
            _ => println!("请输入 0、1 或 2。"),
        }
    }
}
fn choose_theme(terminal: &mut impl SetupTerminal) -> Result<()> {
    loop {
        let Some(answer) = terminal
            .read_line("选择主题：1 怪盗剪贴风，2 EVA 机库终端风，3 像素掌机风，4 航天仪表风，5 赛博 HUD 风，0 取消：")
            .map_err(|_| "无法读取输入")?
        else {
            return Ok(());
        };
        match answer.trim() {
            "0" => return Ok(()),
            "1" => return set_theme(crate::status_board::BoardTheme::PopCut),
            "2" => return set_theme(crate::status_board::BoardTheme::Eva),
            "3" => return set_theme(crate::status_board::BoardTheme::PixelConsole),
            "4" => return set_theme(crate::status_board::BoardTheme::SpaceGauge),
            "5" => return set_theme(crate::status_board::BoardTheme::CyberHud),
            _ => println!("请输入 0 到 5。"),
        }
    }
}
fn confirm(terminal: &mut impl SetupTerminal, prompt: &str) -> Result<bool> {
    Ok(terminal
        .read_line(prompt)
        .map_err(|_| "无法读取输入")?
        .as_deref()
        .is_some_and(|s| matches!(s.trim(), "y" | "Y")))
}
pub fn uninstall_interactive() -> Result<()> {
    let mut terminal = ConsoleTerminal::new();
    if !confirm(&mut terminal, "停止后台并卸载本程序？[y/N]")? {
        return Ok(());
    }
    let delete_data = confirm(&mut terminal, "同时删除设备配置和本地日志？[y/N]")?;
    let delete_key = confirm(&mut terminal, "同时删除凭据管理器中的 API Key？[y/N]")?;
    let data = crate::platform::data_dir()?;
    uninstall_at(&data, delete_data, delete_key)?;
    println!("卸载操作已执行；若程序仍在运行，关闭窗口后完成清理。");
    Ok(())
}

pub fn uninstall_at(data: &Path, delete_data: bool, delete_key: bool) -> Result<()> {
    let _guard = OperationGuard::acquire(data)?;
    let Some(installation) = Installation::load(data)? else {
        return Ok(());
    };
    if installation.active_version != env!("CARGO_PKG_VERSION") {
        return Err("程序版本已变化，请打开新版控制台".into());
    }
    prepare_uninstall(data)?;
    crate::platform::shortcut_windows::ShortcutManager::for_current_user(data)?.remove_owned()?;
    let exe = installation.companion(data);
    BackgroundController::new(data, &exe).stop()?;
    Autostart::new(SystemCommandRunner, &exe).uninstall()?;
    if delete_key {
        OsCredentialStore::zectrix_api_key()?.delete()?;
    }
    if delete_data {
        for file in crate::AppFile::uninstall_targets() {
            let path = data.join(file.file_name());
            reject_link(&path)?;
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
    }
    prepare_uninstall(data)?;
    let current = fs::canonicalize(std::env::current_exe()?)?;
    let installed_app = fs::canonicalize(data.join("app"))?;
    if current.starts_with(&installed_app) {
        use std::os::windows::process::CommandExt;
        let (created, _) =
            crate::platform::background_windows::process_identity(std::process::id())?;
        let helper = std::env::temp_dir().join(format!(
            "codex-zectrix-uninstall-{}-{created}.exe",
            std::process::id()
        ));
        let helper_source = verified_uninstall_helper_source(data, &installation)?;
        let bytes = fs::read(&helper_source)?;
        atomic_write(&helper, &bytes)?;
        std::process::Command::new(&helper)
            .args([
                "finish-uninstall",
                &std::process::id().to_string(),
                &created.to_string(),
            ])
            .env("CODEX_ZECTRIX_DATA_DIR", data)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .spawn()?;
    } else {
        cleanup_installed_files(data)?;
    }
    Ok(())
}

pub fn verified_uninstall_helper_source(
    data: &Path,
    installation: &Installation,
) -> Result<PathBuf> {
    reject_link(&data.join("app"))?;
    let package = Package::load(&installation.directory(data))?;
    Ok(package.directory.join("codex-zectrix-dashboard.exe"))
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UninstallJournal {
    schema_version: u32,
    packages: Vec<crate::install::PackageManifest>,
}
fn prepare_uninstall(data: &Path) -> Result<UninstallJournal> {
    reject_link(&data.join("app"))?;
    let journal_path = data.join("uninstall-pending.json");
    reject_link(&journal_path)?;
    let journal = if journal_path.exists() {
        serde_json::from_slice::<UninstallJournal>(&fs::read(&journal_path)?)?
    } else {
        let installation = Installation::load(data)?.ok_or("尚未安装")?;
        let packages = std::iter::once(&installation.active_version)
            .chain(installation.previous_version.iter())
            .map(|v| Package::load(&data.join("app").join(v)).map(|p| p.manifest))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        UninstallJournal {
            schema_version: 1,
            packages,
        }
    };
    if journal.schema_version != 1 || journal.packages.is_empty() || journal.packages.len() > 2 {
        return Err("卸载记录不受支持".into());
    }
    for package in &journal.packages {
        let allowed = package.allowed_files()?;
        if package.platform != "windows-x86_64"
            || !crate::install::valid_version(&package.version)
            || package.files.len() != allowed.len()
            || allowed.iter().any(|n| !package.files.contains_key(*n))
        {
            return Err("卸载清单不受支持".into());
        }
        let directory = data.join("app").join(&package.version);
        reject_link(&directory)?;
        if !directory.exists() {
            continue;
        }
        for entry in fs::read_dir(&directory)? {
            let name = entry?.file_name();
            if name != "package.json" && !allowed.iter().any(|allowed| name == *allowed) {
                return Err("安装目录含清单以外的文件，已停止清理".into());
            }
        }
        for (name, hash) in &package.files {
            use sha2::{Digest, Sha256};
            let path = directory.join(name);
            reject_link(&path)?;
            if path.exists()
                && format!("{:x}", Sha256::digest(fs::read(path)?)) != hash.to_ascii_lowercase()
            {
                return Err("程序文件已改变，已停止清理".into());
            }
        }
        reject_link(&directory.join("package.json"))?;
    }
    atomic_write(&journal_path, &serde_json::to_vec(&journal)?)?;
    Ok(journal)
}
/// Caller holds the shared operation lock; only verified, explicitly named files are removed.
pub fn cleanup_installed_files(data: &Path) -> Result<()> {
    let journal = prepare_uninstall(data)?;
    for package in journal.packages {
        let directory = data.join("app").join(&package.version);
        if !directory.exists() {
            continue;
        }
        for name in package
            .allowed_files()?
            .iter()
            .copied()
            .chain(["package.json"])
        {
            let path = directory.join(name);
            reject_link(&path)?;
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        fs::remove_dir(directory)?;
    }
    if data.join("installation.json").exists() {
        fs::remove_file(data.join("installation.json"))?;
    }
    fs::remove_file(data.join("uninstall-pending.json"))?;
    Ok(())
}
pub fn finish_uninstall(parent: u32, created: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while crate::platform::background_windows::process_identity(parent)
        .is_ok_and(|(actual, _)| actual == created)
    {
        if std::time::Instant::now() >= deadline {
            return Err("等待卸载窗口关闭超时".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let data = crate::platform::data_dir()?;
    let _guard = OperationGuard::acquire(&data)?;
    if !data.join("uninstall-pending.json").is_file() {
        return Err("没有待完成的卸载".into());
    }
    cleanup_installed_files(&data)
}
pub fn run_operator_menu() -> Result<()> {
    let mut terminal = ConsoleTerminal::new();
    loop {
        println!(
            "\nCODEX 指示牌 · Windows {}\n1. 安装 / 升级\n2. 首次配置 / 重新配置\n3. 启动后台\n4. 停止后台\n5. 查看状态\n6. 开启 / 关闭开机启动\n7. 卸载\n8. 切换主题\n9. 墨卡模式（单页 / 五张自动更新）\n0. 退出",
            env!("CARGO_PKG_VERSION")
        );
        let Some(answer) = terminal
            .read_line("请选择（首次使用选 1）：")
            .map_err(|_| "无法读取输入")?
        else {
            return Ok(());
        };
        if answer == "0" {
            return Ok(());
        }
        let result: Result<()> = (|| {
            match answer.as_str() {
                "1" => {
                    install_current_package()?;
                    let data = crate::platform::data_dir()?;
                    if Settings::load(data.join("settings.json"))?.is_none() {
                        configure()?;
                    }
                }
                "2" => {
                    installed_companion()?;
                    configure()?;
                }
                "3" => start_background()?,
                "4" => stop_background()?,
                "5" => show_local_status()?,
                "6" => toggle_startup()?,
                "7" => uninstall_interactive()?,
                "8" => choose_theme(&mut terminal)?,
                "9" => choose_inkcard_mode(&mut terminal)?,
                _ => println!("请输入 0–9。"),
            };
            Ok(())
        })();
        if result.is_ok() && answer == "7" {
            return Ok(());
        }
        if let Err(error) = result {
            eprintln!("操作未完成：{error}");
        }
    }
}
