use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::{fs, io::ErrorKind};

use thiserror::Error;

pub const AUTOSTART_TASK_NAME: &str = "Codex ZECTRIX Dashboard";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandOutcome {
    pub code: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandRunError;

pub trait CommandRunner {
    fn run(
        &mut self,
        program: &OsStr,
        arguments: &[OsString],
    ) -> Result<CommandOutcome, CommandRunError>;
}

pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(
        &mut self,
        program: &OsStr,
        arguments: &[OsString],
    ) -> Result<CommandOutcome, CommandRunError> {
        let status = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| CommandRunError)?;
        Ok(CommandOutcome {
            code: status.code(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutostartStatus {
    Present,
    Absent,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum AutostartError {
    #[error("invalid companion executable")]
    InvalidExecutable,
    #[error("autostart unavailable")]
    Unavailable,
}

pub trait AutostartControl {
    fn install(&mut self) -> Result<(), AutostartError>;
    fn uninstall(&mut self) -> Result<(), AutostartError>;
    fn status(&mut self) -> AutostartStatus;
}

#[cfg(windows)]
pub trait RunRegistry {
    fn read(&self) -> Result<Option<String>, AutostartError>;
    fn write(&self, value: &str) -> Result<(), AutostartError>;
    fn remove(&self) -> Result<(), AutostartError>;
}

pub struct Autostart<R> {
    runner: R,
    companion_executable: PathBuf,
    #[cfg(windows)]
    registry: Box<dyn RunRegistry>,
    #[cfg(target_os = "macos")]
    launchctl_program: PathBuf,
    #[cfg(target_os = "macos")]
    plist_path: PathBuf,
}

impl<R: CommandRunner> Autostart<R> {
    pub fn new(runner: R, companion_executable: impl Into<PathBuf>) -> Self {
        #[cfg(target_os = "macos")]
        let launchctl_program = std::env::var_os("CODEX_ZECTRIX_LAUNCHCTL_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/bin/launchctl"));
        #[cfg(target_os = "macos")]
        let plist_path = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", crate::LAUNCH_AGENT_LABEL));
        Self {
            runner,
            companion_executable: companion_executable.into(),
            #[cfg(windows)]
            registry: Box::new(crate::platform::registry_windows::CurrentUserRun),
            #[cfg(target_os = "macos")]
            launchctl_program,
            #[cfg(target_os = "macos")]
            plist_path,
        }
    }

    pub fn into_runner(self) -> R {
        self.runner
    }

    #[cfg(windows)]
    pub fn with_registry(
        runner: R,
        companion_executable: impl Into<PathBuf>,
        registry: Box<dyn RunRegistry>,
    ) -> Self {
        Self {
            runner,
            companion_executable: companion_executable.into(),
            registry,
        }
    }

    #[cfg(target_os = "macos")]
    fn run_program(
        &mut self,
        program: &OsStr,
        arguments: Vec<OsString>,
    ) -> Result<CommandOutcome, AutostartError> {
        self.runner
            .run(program, &arguments)
            .map_err(|_| AutostartError::Unavailable)
    }

    #[cfg(target_os = "macos")]
    fn run_launchctl(
        &mut self,
        arguments: Vec<OsString>,
    ) -> Result<CommandOutcome, AutostartError> {
        let program = self.launchctl_program.clone();
        self.run_program(program.as_os_str(), arguments)
    }

    fn validate_companion(&self) -> Result<(), AutostartError> {
        let path = &self.companion_executable;
        let valid_name = path
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| {
                #[cfg(windows)]
                {
                    name.eq_ignore_ascii_case("codex-zectrix-companion.exe")
                }
                #[cfg(target_os = "macos")]
                {
                    name == "codex-zectrix-companion"
                }
                #[cfg(not(any(windows, target_os = "macos")))]
                {
                    let _ = name;
                    false
                }
            });
        if !path.is_absolute() || !path.is_file() || !valid_name {
            return Err(AutostartError::InvalidExecutable);
        }
        Ok(())
    }
}

impl<R: CommandRunner> AutostartControl for Autostart<R> {
    fn install(&mut self) -> Result<(), AutostartError> {
        self.validate_companion()?;
        #[cfg(windows)]
        {
            let action = quoted_run_command(&self.companion_executable)?;
            self.registry.write(&action)?;
            if self.registry.read()?.as_deref() != Some(&action) {
                return Err(AutostartError::Unavailable);
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let parent = self
                .plist_path
                .parent()
                .ok_or(AutostartError::Unavailable)?;
            fs::create_dir_all(parent).map_err(|_| AutostartError::Unavailable)?;
            let executable = xml_escape(&self.companion_executable.to_string_lossy());
            let plist = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{executable}</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>",
                crate::LAUNCH_AGENT_LABEL
            );
            fs::write(&self.plist_path, plist).map_err(|_| AutostartError::Unavailable)?;
            let domain = format!("gui/{}", unsafe { libc::geteuid() });
            let service = format!("{domain}/{}", crate::LAUNCH_AGENT_LABEL);
            let _ = self.run_launchctl(vec!["bootout".into(), service.into()]);
            let plist_path = self.plist_path.as_os_str().to_owned();
            let outcome =
                self.run_launchctl(vec!["bootstrap".into(), domain.into(), plist_path])?;
            (outcome.code == Some(0))
                .then_some(())
                .ok_or(AutostartError::Unavailable)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        Err(AutostartError::Unavailable)
    }

    fn uninstall(&mut self) -> Result<(), AutostartError> {
        #[cfg(windows)]
        {
            if let Some(value) = self.registry.read()? {
                if value != quoted_run_command(&self.companion_executable)? {
                    return Err(AutostartError::Unavailable);
                }
                self.registry.remove()?;
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let service = format!(
                "gui/{}/{}",
                unsafe { libc::geteuid() },
                crate::LAUNCH_AGENT_LABEL
            );
            let outcome = self.run_launchctl(vec!["bootout".into(), service.into()])?;
            if !matches!(outcome.code, Some(0 | 1)) {
                return Err(AutostartError::Unavailable);
            }
            match fs::remove_file(&self.plist_path) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(_) => return Err(AutostartError::Unavailable),
            }
            Ok(())
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        Err(AutostartError::Unavailable)
    }

    fn status(&mut self) -> AutostartStatus {
        #[cfg(windows)]
        {
            match self.registry.read() {
                Ok(None) => AutostartStatus::Absent,
                Ok(Some(value))
                    if quoted_run_command(&self.companion_executable).as_ref() == Ok(&value) =>
                {
                    AutostartStatus::Present
                }
                _ => AutostartStatus::Unavailable,
            }
        }
        #[cfg(target_os = "macos")]
        let outcome = {
            let service = format!(
                "gui/{}/{}",
                unsafe { libc::geteuid() },
                crate::LAUNCH_AGENT_LABEL
            );
            self.run_launchctl(vec!["print".into(), service.into()])
        };
        #[cfg(not(any(windows, target_os = "macos")))]
        let outcome: Result<CommandOutcome, AutostartError> = Err(AutostartError::Unavailable);
        #[cfg(not(windows))]
        match outcome {
            Ok(CommandOutcome { code: Some(0) }) => AutostartStatus::Present,
            Ok(CommandOutcome { code: Some(1) }) => AutostartStatus::Absent,
            Ok(_) | Err(_) => AutostartStatus::Unavailable,
        }
    }
}

impl<R: CommandRunner> Autostart<R> {
    pub fn install(&mut self) -> Result<(), AutostartError> {
        AutostartControl::install(self)
    }

    pub fn uninstall(&mut self) -> Result<(), AutostartError> {
        AutostartControl::uninstall(self)
    }

    pub fn status(&mut self) -> AutostartStatus {
        AutostartControl::status(self)
    }
}

#[cfg(windows)]
pub fn quoted_run_command(path: &Path) -> Result<String, AutostartError> {
    let value = path.to_str().ok_or(AutostartError::InvalidExecutable)?;
    let command = format!("\"{value}\"");
    if !path.is_absolute()
        || value.contains(['"', '\0', '\r', '\n'])
        || command.encode_utf16().count() > 260
    {
        return Err(AutostartError::InvalidExecutable);
    }
    Ok(command)
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
