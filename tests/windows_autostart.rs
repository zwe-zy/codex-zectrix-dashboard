#![cfg(windows)]
use codex_zectrix_dashboard::autostart::{RunRegistry, quoted_run_command};
use codex_zectrix_dashboard::{
    Autostart, AutostartError, AutostartStatus, CommandOutcome, CommandRunError, CommandRunner,
};
use std::{
    ffi::{OsStr, OsString},
    fs,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct NoCommands {
    calls: usize,
}
impl CommandRunner for NoCommands {
    fn run(&mut self, _: &OsStr, _: &[OsString]) -> Result<CommandOutcome, CommandRunError> {
        self.calls += 1;
        Err(CommandRunError)
    }
}
#[derive(Clone, Default)]
struct Registry {
    value: Arc<Mutex<Option<String>>>,
    fail: bool,
}
impl RunRegistry for Registry {
    fn read(&self) -> Result<Option<String>, AutostartError> {
        if self.fail {
            return Err(AutostartError::Unavailable);
        }
        Ok(self.value.lock().unwrap().clone())
    }
    fn write(&self, value: &str) -> Result<(), AutostartError> {
        if self.fail {
            return Err(AutostartError::Unavailable);
        }
        *self.value.lock().unwrap() = Some(value.into());
        Ok(())
    }
    fn remove(&self) -> Result<(), AutostartError> {
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}
#[test]
fn run_registration_needs_no_scheduler() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("中文目录 & 空格");
    fs::create_dir(&folder).unwrap();
    let exe = folder.join("codex-zectrix-companion.exe");
    fs::write(&exe, b"fixture").unwrap();
    let registry = Registry::default();
    let mut startup =
        Autostart::with_registry(NoCommands::default(), &exe, Box::new(registry.clone()));
    assert_eq!(startup.status(), AutostartStatus::Absent);
    startup.install().unwrap();
    assert_eq!(
        *registry.value.lock().unwrap(),
        Some(format!("\"{}\"", exe.display()))
    );
    assert_eq!(startup.status(), AutostartStatus::Present);
    startup.uninstall().unwrap();
    assert_eq!(startup.status(), AutostartStatus::Absent);
    assert_eq!(startup.into_runner().calls, 0);
}
#[test]
fn long_or_ambiguous_run_command_is_rejected() {
    assert!(
        quoted_run_command(std::path::Path::new(&format!(
            "C:\\{}\\codex-zectrix-companion.exe",
            "x".repeat(260)
        )))
        .is_err()
    );
    assert!(quoted_run_command(std::path::Path::new("relative.exe")).is_err());
    assert!(quoted_run_command(std::path::Path::new("C:\\bad\"path.exe")).is_err());
}
#[test]
fn registry_failure_is_never_reported_as_configured() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("codex-zectrix-companion.exe");
    fs::write(&exe, b"fixture").unwrap();
    let mut startup = Autostart::with_registry(
        NoCommands::default(),
        &exe,
        Box::new(Registry {
            fail: true,
            ..Default::default()
        }),
    );
    assert_eq!(startup.status(), AutostartStatus::Unavailable);
    assert_eq!(startup.install(), Err(AutostartError::Unavailable));
}
#[test]
fn uninstall_does_not_remove_a_different_target() {
    let registry = Registry::default();
    *registry.value.lock().unwrap() = Some("\"C:\\other.exe\"".into());
    let mut startup = Autostart::with_registry(
        NoCommands::default(),
        "C:\\ours\\codex-zectrix-companion.exe",
        Box::new(registry.clone()),
    );
    assert!(startup.uninstall().is_err());
    assert!(registry.value.lock().unwrap().is_some());
}
