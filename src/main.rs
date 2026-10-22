use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use codex_zectrix_dashboard::CredentialStore;
use codex_zectrix_dashboard::StatusProbe;
use codex_zectrix_dashboard::platform;
#[cfg(target_os = "macos")]
use codex_zectrix_dashboard::platform::Shutdown;
use codex_zectrix_dashboard::{
    AppServerClient, AppServerError, Autostart, CodexReachability, CodexStatusError,
    CodexStatusSource, ConsoleTerminal, DashboardConfig, FilesystemSetupStore,
    ObservedDashboardState, ObservedQuota, ObservedTask, QuotaCache, RadarReason,
    ReadonlyObservationConfig, ReadonlyRolloutObserver, SystemCommandRunner,
    TaskActivityAvailability, TaskActivityCache, TaskActivitySnapshot, TaskState,
    hook_is_tombstoned, parse_hook_event, persist_hook_event, read_hook_events, record_hook_owner,
    reduce_task_activity, render_dashboard, run_operator_status, write_deterministic_preview,
};
#[cfg(not(windows))]
use codex_zectrix_dashboard::{OsCredentialStore, run_uninstall};
#[cfg(target_os = "macos")]
use codex_zectrix_dashboard::{
    PluginLifecycle, PublishAttempt, PublishCoordinator, PublisherState, RadarCache, RadarClient,
    RadarDisplayState, RadarFetch, RadarSemanticState, ZectrixPublisher, find_codex_owner_pid,
};

const CORRELATION_SALT: &str = "codex-zectrix-dashboard-v1";

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum QuotaSourceStatus {
    Current,
    Stale,
    #[default]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum TaskActivitySourceStatus {
    Inferred,
    Stale,
    #[default]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum RadarSourceStatus {
    Current,
    Stale,
    #[default]
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceStatus {
    quota: QuotaSourceStatus,
    task_activity: TaskActivitySourceStatus,
    radar: RadarSourceStatus,
    radar_reason: RadarReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    radar_next_request_at_epoch_seconds: Option<i64>,
}

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySettings {
    device_id: String,
    page_id: u8,
    privacy_mode: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let command = match args.next() {
        Some(command) => command,
        None => {
            #[cfg(windows)]
            return codex_zectrix_dashboard::operator::run_operator_menu();
            #[cfg(not(windows))]
            return Err(
                "用法：codex-zectrix-dashboard <setup|preview|status|uninstall|run> ...".into(),
            );
        }
    };
    #[cfg(windows)]
    if command == "inkcards" {
        let mode = args.next().ok_or("缺少墨卡模式：all 或 single")?.parse()?;
        if args.next().is_some() {
            return Err("inkcards 只接受一个模式参数".into());
        }
        return codex_zectrix_dashboard::operator::set_inkcard_mode(mode);
    }
    #[cfg(windows)]
    if command == "theme" {
        let theme = args
            .next()
            .ok_or("缺少主题：pop-cut、eva、pixel-console、space-gauge 或 cyber-hud")?
            .parse()?;
        if args.next().is_some() {
            return Err("theme 只接受一个主题参数".into());
        }
        return codex_zectrix_dashboard::operator::set_theme(theme);
    }
    #[cfg(windows)]
    if command == "verify-package" {
        use codex_zectrix_dashboard::install::{InstallHost, Package, windows::WindowsHost};
        let directory = PathBuf::from(args.next().ok_or("缺少发布包目录")?);
        if args.next().is_some() {
            return Err("发布包参数无效".into());
        }
        let package = Package::load(&directory)?;
        WindowsHost::new(&directory).verify_binaries(&directory, &package.manifest.version)?;
        println!("package_verified");
        return Ok(());
    }
    #[cfg(windows)]
    if command == "finish-uninstall" {
        let pid = args.next().ok_or("缺少卸载父进程")?.parse::<u32>()?;
        let created = args.next().ok_or("缺少卸载进程时间")?.parse::<u64>()?;
        if args.next().is_some() {
            return Err("卸载参数无效".into());
        }
        return codex_zectrix_dashboard::operator::finish_uninstall(pid, created);
    }
    #[cfg(windows)]
    if matches!(command.as_str(), "install" | "start" | "stop" | "menu") {
        if args.next().is_some() {
            return Err("此命令不接受其他参数".into());
        }
        return match command.as_str() {
            "install" => codex_zectrix_dashboard::operator::install_current_package(),
            "start" => codex_zectrix_dashboard::operator::start_background(),
            "stop" => codex_zectrix_dashboard::operator::stop_background(),
            _ => codex_zectrix_dashboard::operator::run_operator_menu(),
        };
    }
    if command == "build-fingerprint" {
        if args.next().is_some() {
            return Err("build-fingerprint 不接受命令行参数".into());
        }
        println!(
            "{}",
            option_env!("CODEX_ZECTRIX_SOURCE_FINGERPRINT").unwrap_or("development")
        );
        return Ok(());
    }
    if command == "version" {
        if args.next().is_some() {
            return Err("version 不接受命令行参数".into());
        }
        println!(env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if command == "diagnostics" {
        if args.next().is_some() {
            return Err("diagnostics 不接受命令行参数".into());
        }
        return run_diagnostics();
    }
    if command == "setup" {
        if args.next().is_some() {
            return Err("setup 不接受命令行参数".into());
        }
        codex_zectrix_dashboard::run_setup()?;
        return Ok(());
    }
    if command == "status" {
        if args.next().is_some() {
            return Err("status 不接受命令行参数".into());
        }
        return run_status_command();
    }
    if command == "uninstall" {
        if args.next().is_some() {
            return Err("uninstall 不接受命令行参数".into());
        }
        #[cfg(windows)]
        return codex_zectrix_dashboard::operator::uninstall_interactive();
        #[cfg(not(windows))]
        return run_uninstall_command();
    }
    if command == "lifecycle" {
        return run_lifecycle(args.collect());
    }
    if command == "hook-record" {
        if args.next().is_some() {
            return Err("hook-record 不接受命令行参数".into());
        }
        return record_hook();
    }
    if matches!(command.as_str(), "companion" | "run") {
        if args.next().is_some() {
            return Err("companion 不接受命令行参数".into());
        }
        return run_companion();
    }
    if !matches!(command.as_str(), "preview" | "live-preview") {
        return Err("用法：codex-zectrix-dashboard <preview|live-preview|setup> ...".into());
    }

    let mut input = None;
    let mut output = None;
    let mut privacy_mode = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--input" => input = args.next().map(PathBuf::from),
            "--output" => output = args.next().map(PathBuf::from),
            "--privacy" => privacy_mode = true,
            _ => return Err(format!("未知参数：{argument}").into()),
        }
    }

    let output = output.ok_or("缺少 --output")?;
    if command == "preview" && input.is_none() {
        write_deterministic_preview(&output)?;
        println!("preview_written");
        return Ok(());
    }
    let now_epoch_seconds = platform::now_epoch_seconds()?;
    let (observed, render_time) = if command == "preview" {
        let input = input.ok_or("缺少 --input")?;
        (
            serde_json::from_slice::<ObservedDashboardState>(&fs::read(input)?)?,
            1_786_330_000,
        )
    } else {
        if input.is_some() {
            return Err("只有 preview 支持 --input".into());
        }
        let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
            .map(AppServerClient::new)
            .unwrap_or_default();
        let data_dir = platform::data_dir()?;
        fs::create_dir_all(&data_dir)?;
        let quota_cache_path = data_dir.join("quota.json");
        let last_known = fs::read(&quota_cache_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ObservedQuota>(&bytes).ok());
        let mut quota_cache = QuotaCache::new(last_known);
        let quota = match client.read_quota() {
            Ok(quota) => {
                let quota = quota_cache.update::<std::convert::Infallible>(Ok(quota))?;
                fs::write(&quota_cache_path, serde_json::to_vec(&quota)?)?;
                quota
            }
            Err(error) => quota_cache.update(Err(error))?,
        };
        let activity_cache_path = data_dir.join("activity.json");
        let last_known_tasks = fs::read(&activity_cache_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<ObservedTask>>(&bytes).ok());
        let mut activity_cache = TaskActivityCache::new(last_known_tasks);
        let resolved = resolve_task_activity(
            observe_activity(&client, &data_dir, now_epoch_seconds),
            &mut activity_cache,
        );
        if resolved.observed && !resolved.snapshot.stale {
            fs::write(
                &activity_cache_path,
                serde_json::to_vec(&resolved.snapshot.tasks)?,
            )?;
        }
        (
            ObservedDashboardState {
                quota,
                radar: Default::default(),
                task_activity_availability: resolved.availability,
                task_activity_stale: resolved.snapshot.stale,
                tasks: resolved.snapshot.tasks,
                prompt: None,
                response: None,
                reasoning: None,
                project_path: None,
                tool: None,
                error_text: None,
                plan: None,
            },
            now_epoch_seconds,
        )
    };
    let dashboard = render_dashboard(
        observed,
        render_time,
        DashboardConfig {
            privacy_mode,
            previous_frame_hash: None,
        },
    )?;
    dashboard.frame.write_png(&output)?;
    println!("{}  {}", dashboard.frame.sha256, output.display());
    Ok(())
}

struct ProductionStatusProbe {
    source: CodexStatusSource,
}

impl StatusProbe for ProductionStatusProbe {
    fn task_reachability(&mut self) -> CodexReachability {
        map_task_reachability(self.source.read_latest_task_observation())
    }

    fn quota_reachability(&mut self) -> CodexReachability {
        match self.source.read_quota_status() {
            Ok(_) => CodexReachability::Supported,
            Err(CodexStatusError::Unavailable) => CodexReachability::Unavailable,
            Err(CodexStatusError::MethodUnavailable) => CodexReachability::MethodUnavailable,
            Err(CodexStatusError::AccountUnavailable) => CodexReachability::AccountUnavailable,
            Err(CodexStatusError::Incompatible) => CodexReachability::Incompatible,
        }
    }
}

fn map_task_reachability(
    result: Result<codex_zectrix_dashboard::TaskObservation, CodexStatusError>,
) -> CodexReachability {
    match result {
        Ok(observation) if observation.partial || observation.state == TaskState::Unavailable => {
            CodexReachability::Partial
        }
        Ok(_) => CodexReachability::Supported,
        Err(CodexStatusError::Unavailable) => CodexReachability::Unavailable,
        Err(CodexStatusError::MethodUnavailable) => CodexReachability::MethodUnavailable,
        Err(CodexStatusError::AccountUnavailable) => CodexReachability::AccountUnavailable,
        Err(CodexStatusError::Incompatible) => CodexReachability::Incompatible,
    }
}

fn production_autostart() -> Result<Autostart<SystemCommandRunner>, Box<dyn std::error::Error>> {
    let current = env::current_exe()?;
    let companion = current
        .parent()
        .ok_or("operator command unavailable")?
        .join(if cfg!(windows) {
            "codex-zectrix-companion.exe"
        } else {
            "codex-zectrix-companion"
        });
    Ok(Autostart::new(SystemCommandRunner, companion))
}

fn run_status_command() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = platform::data_dir()?;
    let files = FilesystemSetupStore::new(data_dir).map_err(|_| "operator command unavailable")?;
    let mut autostart = production_autostart()?;
    let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(AppServerClient::new)
        .unwrap_or_default();
    let mut probe = ProductionStatusProbe {
        source: CodexStatusSource::new(client),
    };
    let mut terminal = ConsoleTerminal::new();
    run_operator_status(&mut terminal, &files, &mut autostart, &mut probe)?;
    Ok(())
}

#[cfg(not(windows))]
fn run_uninstall_command() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = platform::data_dir()?;
    let mut files =
        FilesystemSetupStore::new(data_dir).map_err(|_| "operator command unavailable")?;
    let credentials =
        OsCredentialStore::zectrix_api_key().map_err(|_| "operator command unavailable")?;
    let mut autostart = production_autostart()?;
    let mut terminal = ConsoleTerminal::new();
    run_uninstall(&mut terminal, &credentials, &mut files, &mut autostart)?;
    Ok(())
}

fn run_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    let path = platform::data_dir()?.join("source-status.json");
    let status = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SourceStatus>(&bytes).ok())
        .unwrap_or_default();
    let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(AppServerClient::new)
        .unwrap_or_default();
    let source = CodexStatusSource::new(client);
    let quota_method = match source.read_quota_status() {
        Ok(_) => "supported",
        Err(CodexStatusError::Unavailable) => "unavailable",
        Err(CodexStatusError::MethodUnavailable) => "method_unavailable",
        Err(CodexStatusError::AccountUnavailable) => "account_unavailable",
        Err(CodexStatusError::Incompatible) => "incompatible",
    };
    let task_method = match map_task_reachability(source.read_latest_task_observation()) {
        CodexReachability::Supported => "supported",
        CodexReachability::Partial => "partial",
        CodexReachability::Unavailable => "unavailable",
        CodexReachability::MethodUnavailable => "method_unavailable",
        CodexReachability::AccountUnavailable => "account_unavailable",
        CodexReachability::Incompatible => "incompatible",
    };
    println!(
        "quota_source={}\ntask_activity_source={}\nradar_source={}\nradar_reason={}{}\ncodex_quota_method={quota_method}\ncodex_task_method={task_method}",
        match status.quota {
            QuotaSourceStatus::Current => "current",
            QuotaSourceStatus::Stale => "stale",
            QuotaSourceStatus::Unavailable => "unavailable",
        },
        match status.task_activity {
            TaskActivitySourceStatus::Inferred => "inferred",
            TaskActivitySourceStatus::Stale => "stale",
            TaskActivitySourceStatus::Unavailable => "unavailable",
        },
        match status.radar {
            RadarSourceStatus::Current => "current",
            RadarSourceStatus::Stale => "stale",
            RadarSourceStatus::Unavailable => "unavailable",
        },
        radar_reason_label(status.radar_reason),
        status
            .radar_next_request_at_epoch_seconds
            .map(|timestamp| format!("\nradar_next_request_at={timestamp}"))
            .unwrap_or_default(),
    );
    Ok(())
}

fn observe_activity(
    client: &AppServerClient,
    data_dir: &std::path::Path,
    now_epoch_seconds: i64,
) -> Result<TaskActivitySnapshot, Box<dyn std::error::Error>> {
    let metadata = client.read_task_metadata(CORRELATION_SALT)?;
    let codex_home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or("无法确定 Codex 数据目录")?;
    let observer = ReadonlyRolloutObserver::new(ReadonlyObservationConfig {
        codex_home,
        installation_salt: CORRELATION_SALT.into(),
    });
    let hook_path = data_dir.join("hook-events.jsonl");
    let rollout_events = observer.observe();
    let hook_events = read_hook_events(&hook_path);
    let events = match (rollout_events, hook_events) {
        (Ok(mut rollout), Ok(hooks)) => {
            rollout.extend(hooks);
            rollout
        }
        (Err(error), Ok(hooks)) => {
            let snapshot = reduce_task_activity(metadata, hooks, now_epoch_seconds);
            if snapshot.tasks.is_empty() {
                return Err(Box::new(error));
            }
            return Ok(snapshot);
        }
        (Ok(rollout), Err(_)) => rollout,
        (Err(error), _) => return Err(Box::new(error)),
    };
    Ok(reduce_task_activity(metadata, events, now_epoch_seconds))
}

fn record_hook() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(executable) = env::current_exe() else {
        return Ok(());
    };
    if hook_is_tombstoned(&executable) {
        return Ok(());
    }
    let Ok(data_dir) = platform::data_dir() else {
        return Ok(());
    };
    let owner_pid = env::var("CODEX_ZECTRIX_HOOK_OWNER_PID")
        .ok()
        .and_then(|value| value.parse().ok());
    #[cfg(target_os = "macos")]
    let owner_pid = owner_pid.or_else(|| {
        let ps = env::var_os("CODEX_ZECTRIX_PS_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/ps"));
        find_codex_owner_pid(&ps, unsafe { libc::getppid() as u32 })
    });
    let owner_pid = owner_pid.unwrap_or(0);
    record_hook_owner(&data_dir, owner_pid);
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return Ok(());
    }
    let Ok(now_epoch_seconds) = platform::now_epoch_seconds() else {
        return Ok(());
    };
    if let Ok(Some(event)) = parse_hook_event(&input, CORRELATION_SALT, now_epoch_seconds) {
        let _ = persist_hook_event(&data_dir.join("hook-events.jsonl"), &event);
    }
    Ok(())
}

#[cfg(windows)]
fn run_lifecycle(_arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    Err(codex_zectrix_dashboard::platform::PlatformError::UnsupportedPlatform.into())
}

#[cfg(target_os = "macos")]
fn run_lifecycle(arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let action = arguments.first().map(String::as_str).ok_or(
        "用法：codex-zectrix-dashboard lifecycle <install|update|uninstall|resume|restore|diagnostics>",
    )?;
    let mut plugin_root = None;
    let mut plugin_id = None;
    let mut index = 1;
    while index < arguments.len() {
        let value = arguments.get(index + 1).ok_or("生命周期参数缺少值")?;
        match arguments[index].as_str() {
            "--plugin-root" => plugin_root = Some(PathBuf::from(value)),
            "--plugin-id" => plugin_id = Some(value.clone()),
            _ => return Err("未知生命周期参数".into()),
        }
        index += 2;
    }
    let codex = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"));
    let launchctl = env::var_os("CODEX_ZECTRIX_LAUNCHCTL_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/launchctl"));
    let launch_agents_dir = env::var_os("CODEX_ZECTRIX_LAUNCH_AGENTS_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
        })
        .ok_or("无法确定 LaunchAgents 目录")?;
    let ps = env::var_os("CODEX_ZECTRIX_PS_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/ps"));
    let lifecycle = PluginLifecycle::new(
        platform::data_dir()?,
        codex,
        launchctl,
        launch_agents_dir,
        ps,
    );
    match action {
        "install" => lifecycle.install(
            plugin_root.as_deref().ok_or("install 缺少 --plugin-root")?,
            plugin_id.as_deref().ok_or("install 缺少 --plugin-id")?,
        )?,
        "update" => {
            lifecycle.begin_update(
                plugin_root.as_deref().ok_or("update 缺少 --plugin-root")?,
                plugin_id.as_deref().ok_or("update 缺少 --plugin-id")?,
            )?;
            println!("hooks_disabled=true\ncompanion_stopped=true\ndesktop_reload_required=true");
        }
        "uninstall" => {
            lifecycle.begin_uninstall()?;
            println!("hooks_disabled=true\ncompanion_stopped=true\ndesktop_reload_required=true");
        }
        "resume" => lifecycle.resume()?,
        "restore" => lifecycle.restore()?,
        "diagnostics" => print!("{}", lifecycle.diagnostics()?),
        _ => return Err("未知生命周期操作".into()),
    }
    Ok(())
}

#[cfg(windows)]
fn run_companion() -> Result<(), Box<dyn std::error::Error>> {
    match codex_zectrix_dashboard::companion::run_companion_process_code() {
        0 => Ok(()),
        _ => Err("companion_unavailable".into()),
    }
}

#[cfg(target_os = "macos")]
fn run_companion() -> Result<(), Box<dyn std::error::Error>> {
    let shutdown = Shutdown::install()?;
    let data_dir = platform::data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let settings: LegacySettings =
        serde_json::from_slice(&fs::read(data_dir.join("settings.json"))?)?;
    let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(AppServerClient::new)
        .unwrap_or_default();

    let quota_cache_path = data_dir.join("quota.json");
    let last_known_quota = fs::read(&quota_cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ObservedQuota>(&bytes).ok());
    let mut quota_cache = QuotaCache::new(last_known_quota.clone());
    let mut current_quota = last_known_quota.map(|mut quota| {
        quota.stale = true;
        quota
    });
    let activity_cache_path = data_dir.join("activity.json");
    let last_known_tasks = fs::read(&activity_cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<ObservedTask>>(&bytes).ok());
    let mut activity_cache = TaskActivityCache::new(last_known_tasks.clone());
    let mut current_activity = last_known_tasks
        .map(|tasks| {
            (
                TaskActivitySnapshot { tasks, stale: true },
                TaskActivityAvailability::Available,
            )
        })
        .unwrap_or_else(unavailable_task_activity);
    let radar_cache_path = data_dir.join("radar.json");
    let last_known_radar = fs::read(&radar_cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RadarSemanticState>(&bytes).ok())
        .unwrap_or_default();
    let mut radar_cache = RadarCache::new(last_known_radar);
    let publisher_state_path = data_dir.join("publisher-state.json");
    let publisher_state = fs::read(&publisher_state_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PublisherState>(&bytes).ok())
        .unwrap_or_default();
    let source_status_path = data_dir.join("source-status.json");
    let mut source_status = fs::read(&source_status_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SourceStatus>(&bytes).ok())
        .unwrap_or_default();
    let mut coordinator = PublishCoordinator::new(
        DashboardConfig {
            privacy_mode: settings.privacy_mode,
            previous_frame_hash: None,
        },
        publisher_state,
    );
    let max_cycles = env::var("CODEX_ZECTRIX_MAX_CYCLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let legacy_poll_interval = env::var("CODEX_ZECTRIX_POLL_INTERVAL_MILLIS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis);
    let task_poll_interval = duration_from_environment(
        "CODEX_ZECTRIX_TASK_POLL_INTERVAL_MILLIS",
        legacy_poll_interval,
        Duration::from_secs(30),
    );
    let quota_poll_interval = duration_from_environment(
        "CODEX_ZECTRIX_QUOTA_POLL_INTERVAL_MILLIS",
        legacy_poll_interval,
        Duration::from_secs(10 * 60),
    );
    let radar_poll_interval = if max_cycles.is_some() {
        duration_from_environment(
            "CODEX_ZECTRIX_RADAR_POLL_INTERVAL_MILLIS",
            None,
            Duration::from_secs(60 * 60),
        )
    } else {
        Duration::from_secs(60 * 60)
    };
    let radar_endpoint = radar_endpoint(max_cycles.is_some());
    let mut radar_client = RadarClient::new(radar_endpoint)?;
    let base_url =
        env::var("CODEX_ZECTRIX_API_BASE").unwrap_or_else(|_| "https://cloud.zectrix.com".into());
    let api_key = OsCredentialStore::zectrix_api_key()
        .ok()
        .and_then(|store| store.get().ok().flatten());

    let mut cycles = 0;
    let mut task_deadline = Instant::now();
    let mut quota_deadline = task_deadline;
    let mut radar_deadline = task_deadline;
    loop {
        let now_epoch_seconds = platform::now_epoch_seconds()?;
        let now = Instant::now();
        if now >= quota_deadline {
            current_quota = match client.read_quota() {
                Ok(quota) => {
                    source_status.quota = QuotaSourceStatus::Current;
                    let mut quota = quota_cache.update::<std::convert::Infallible>(Ok(quota))?;
                    if write_json_atomically_if_changed(&quota_cache_path, &quota).is_err() {
                        quota.stale = true;
                        source_status.quota = QuotaSourceStatus::Stale;
                        eprintln!("state_persist_unavailable");
                    }
                    Some(quota)
                }
                Err(error) => match quota_cache.update(Err(error)) {
                    Ok(quota) => {
                        source_status.quota = QuotaSourceStatus::Stale;
                        Some(quota)
                    }
                    Err(_) => {
                        source_status.quota = QuotaSourceStatus::Unavailable;
                        eprintln!("observation_unavailable");
                        None
                    }
                },
            };
            quota_deadline = Instant::now() + quota_poll_interval;
        }

        if now >= task_deadline {
            let mut resolved = resolve_task_activity(
                observe_activity(&client, &data_dir, now_epoch_seconds),
                &mut activity_cache,
            );
            source_status.task_activity = resolved.source_status;
            if resolved.observed
                && write_json_atomically_if_changed(&activity_cache_path, &resolved.snapshot.tasks)
                    .is_err()
            {
                resolved.snapshot.stale = true;
                source_status.task_activity = TaskActivitySourceStatus::Stale;
                eprintln!("state_persist_unavailable");
            }
            if resolved.observation_unavailable {
                eprintln!("observation_unavailable");
            }
            current_activity = (resolved.snapshot, resolved.availability);
            task_deadline = Instant::now() + task_poll_interval;
        }
        if now >= radar_deadline {
            let mut next_interval = radar_poll_interval;
            let mut request_failed = false;
            match radar_client.fetch() {
                RadarFetch::Modified(parsed) => {
                    let invalid_capability = parsed.has_invalid_capability();
                    radar_cache.accept(parsed, now_epoch_seconds);
                    if write_json_atomically_if_changed(&radar_cache_path, radar_cache.semantic())
                        .is_err()
                    {
                        radar_cache.mark_failed();
                        eprintln!("state_persist_unavailable");
                    }
                    source_status.radar = if radar_cache.display(now_epoch_seconds)
                        == RadarDisplayState::Unavailable
                    {
                        RadarSourceStatus::Unavailable
                    } else {
                        RadarSourceStatus::Current
                    };
                    source_status.radar_reason = if invalid_capability {
                        RadarReason::InvalidResponse
                    } else {
                        RadarReason::None
                    };
                    source_status.radar_next_request_at_epoch_seconds = None;
                }
                RadarFetch::NotModified => {
                    radar_cache.confirm_unchanged(now_epoch_seconds);
                    source_status.radar = if radar_cache.display(now_epoch_seconds)
                        == RadarDisplayState::Unavailable
                    {
                        RadarSourceStatus::Unavailable
                    } else {
                        RadarSourceStatus::Current
                    };
                    source_status.radar_reason = RadarReason::None;
                    source_status.radar_next_request_at_epoch_seconds = None;
                }
                RadarFetch::Failed {
                    reason,
                    retry_after_seconds,
                } => {
                    request_failed = true;
                    radar_cache.mark_failed();
                    source_status.radar = if radar_cache.display(now_epoch_seconds)
                        == RadarDisplayState::Unavailable
                    {
                        RadarSourceStatus::Unavailable
                    } else {
                        RadarSourceStatus::Stale
                    };
                    source_status.radar_reason = reason;
                    if let Some(seconds) = retry_after_seconds {
                        next_interval = next_interval.max(Duration::from_secs(seconds));
                    }
                }
            }
            radar_deadline = Instant::now() + next_interval;
            if request_failed {
                source_status.radar_next_request_at_epoch_seconds =
                    Some(epoch_seconds_after(next_interval)?);
            }
        }
        let current_radar = radar_cache.display(now_epoch_seconds);
        if current_radar == RadarDisplayState::Unavailable {
            source_status.radar = RadarSourceStatus::Unavailable;
        }
        if write_json_atomically_if_changed(&source_status_path, &source_status).is_err() {
            eprintln!("state_persist_unavailable");
        }
        if let Some(quota) = current_quota.clone() {
            coordinator.observe(
                ObservedDashboardState {
                    quota,
                    radar: current_radar.clone(),
                    task_activity_availability: current_activity.1,
                    task_activity_stale: current_activity.0.stale,
                    tasks: current_activity.0.tasks.clone(),
                    prompt: None,
                    response: None,
                    reasoning: None,
                    project_path: None,
                    tool: None,
                    error_text: None,
                    plan: None,
                },
                now_epoch_seconds,
            );
        }

        if coordinator.has_pending() {
            let attempt = api_key.as_ref().and_then(|api_key| {
                ZectrixPublisher::new(api_key, &base_url, &settings.device_id, settings.page_id)
                    .ok()
                    .and_then(|mut publisher| {
                        coordinator
                            .try_publish_with_reservation(
                                now_epoch_seconds,
                                &mut publisher,
                                |state| {
                                    write_json_atomically_if_changed(&publisher_state_path, state)
                                        .is_ok()
                                },
                            )
                            .ok()
                    })
            });
            match attempt {
                Some(PublishAttempt::Published | PublishAttempt::Unchanged) => {
                    if write_json_atomically_if_changed(&publisher_state_path, coordinator.state())
                        .is_err()
                    {
                        eprintln!("state_persist_unavailable");
                    }
                }
                Some(PublishAttempt::Failed) => {
                    if write_json_atomically_if_changed(&publisher_state_path, coordinator.state())
                        .is_err()
                    {
                        eprintln!("state_persist_unavailable");
                    }
                    eprintln!("publish_unavailable");
                }
                Some(PublishAttempt::ReservationFailed) => {
                    eprintln!("state_persist_unavailable");
                }
                None => eprintln!("publish_unavailable"),
                Some(PublishAttempt::Idle | PublishAttempt::Deferred { .. }) => {}
            }
        }

        if finish_cycle(
            &mut cycles,
            max_cycles,
            task_deadline,
            quota_deadline,
            radar_deadline,
            &shutdown,
        ) {
            break;
        }
    }
    Ok(())
}

fn unavailable_task_activity() -> (TaskActivitySnapshot, TaskActivityAvailability) {
    (
        TaskActivitySnapshot {
            tasks: Vec::new(),
            stale: false,
        },
        TaskActivityAvailability::Unavailable,
    )
}

fn transport_unavailable_task_activity(
    activity_cache: &TaskActivityCache,
) -> (TaskActivitySnapshot, TaskActivityAvailability) {
    activity_cache
        .last_known_stale()
        .map(|snapshot| (snapshot, TaskActivityAvailability::Available))
        .unwrap_or_else(unavailable_task_activity)
}

#[cfg_attr(windows, allow(dead_code))]
struct ResolvedTaskActivity {
    snapshot: TaskActivitySnapshot,
    availability: TaskActivityAvailability,
    source_status: TaskActivitySourceStatus,
    observed: bool,
    observation_unavailable: bool,
}

fn resolve_task_activity(
    observation: Result<TaskActivitySnapshot, Box<dyn std::error::Error>>,
    activity_cache: &mut TaskActivityCache,
) -> ResolvedTaskActivity {
    match observation {
        Ok(snapshot) => {
            let snapshot = activity_cache.update::<std::convert::Infallible>(Ok(snapshot));
            let source_status = if snapshot.stale {
                TaskActivitySourceStatus::Stale
            } else {
                TaskActivitySourceStatus::Inferred
            };
            ResolvedTaskActivity {
                snapshot,
                availability: TaskActivityAvailability::Available,
                source_status,
                observed: true,
                observation_unavailable: false,
            }
        }
        Err(error) if is_transport_unavailable(error.as_ref()) => {
            let (snapshot, availability) = transport_unavailable_task_activity(activity_cache);
            let source_status = if snapshot.stale {
                TaskActivitySourceStatus::Stale
            } else {
                TaskActivitySourceStatus::Unavailable
            };
            ResolvedTaskActivity {
                snapshot,
                availability,
                source_status,
                observed: false,
                observation_unavailable: true,
            }
        }
        Err(_) => {
            let (snapshot, availability) = unavailable_task_activity();
            ResolvedTaskActivity {
                snapshot,
                availability,
                source_status: TaskActivitySourceStatus::Unavailable,
                observed: false,
                observation_unavailable: true,
            }
        }
    }
}

fn is_transport_unavailable(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<AppServerError>()
        .is_some_and(AppServerError::is_transport_unavailable)
}

#[cfg(target_os = "macos")]
fn finish_cycle(
    cycles: &mut usize,
    max_cycles: Option<usize>,
    task_deadline: Instant,
    quota_deadline: Instant,
    radar_deadline: Instant,
    shutdown: &Shutdown,
) -> bool {
    *cycles += 1;
    if max_cycles.is_some_and(|maximum| *cycles >= maximum) {
        return true;
    }
    let next_deadline = task_deadline.min(quota_deadline).min(radar_deadline);
    shutdown.wait_until(next_deadline)
}

#[cfg(target_os = "macos")]
fn radar_endpoint(test_mode: bool) -> String {
    if test_mode {
        return env::var("CODEX_ZECTRIX_TEST_RADAR_URL")
            .ok()
            .filter(|url| url.starts_with("http://127.0.0.1:") || url.starts_with("http://[::1]:"))
            .unwrap_or_else(|| "http://127.0.0.1:1/api/v1/status".into());
    }
    "https://codex-resets.com/api/v1/status".into()
}

fn radar_reason_label(reason: RadarReason) -> &'static str {
    match reason {
        RadarReason::None => "none",
        RadarReason::Network => "network",
        RadarReason::RateLimited => "rate_limited",
        RadarReason::UpstreamUnavailable => "upstream_unavailable",
        RadarReason::InvalidResponse => "invalid_response",
        RadarReason::ResponseTooLarge => "response_too_large",
    }
}

#[cfg(target_os = "macos")]
fn duration_from_environment(name: &str, legacy: Option<Duration>, default: Duration) -> Duration {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .or(legacy)
        .unwrap_or(default)
}

#[cfg(target_os = "macos")]
fn duration_seconds_ceil(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
}

#[cfg(target_os = "macos")]
fn epoch_seconds_after(duration: Duration) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(platform::now_epoch_seconds()?.saturating_add(duration_seconds_ceil(duration).try_into()?))
}

#[cfg(target_os = "macos")]
fn write_json_atomically_if_changed(
    path: &std::path::Path,
    value: &impl serde::Serialize,
) -> Result<bool, Box<dyn std::error::Error>> {
    let semantic_value = serde_json::to_value(value)?;
    if fs::read(path)
        .ok()
        .and_then(|existing| serde_json::from_slice::<serde_json::Value>(&existing).ok())
        .is_some_and(|existing| existing == semantic_value)
    {
        return Ok(false);
    }
    let serialized = serde_json::to_vec(&semantic_value)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serialized)?;
    fs::rename(temporary, path)?;
    Ok(true)
}
