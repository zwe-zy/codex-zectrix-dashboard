use super::*;
use crate::autostart::{RunRegistry, quoted_run_command};
use crate::platform::{
    background_windows::{BackgroundController, BackgroundStatus},
    registry_windows::CurrentUserRun,
};
use std::{
    os::windows::process::CommandExt,
    process::{Command, Stdio},
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const LEGACY_HASH: &str = "b670cc5e25d05292256b8318fd8fee5119193ea952336d385eed1e94d9b30905";
#[cfg(test)]
mod review_tests {
    use super::*;
    #[test]
    fn unmanaged_legacy_process_blocks_activation_without_a_scheduled_task() {
        let processes = vec![OwnedCompanion {
            pid: 123,
            executable: PathBuf::from(r"C:\old\codex-zectrix-companion.exe"),
        }];
        assert!(validate_companion_processes(&processes, &HostSnapshot::default()).is_err());
    }
    #[test]
    fn committed_migration_no_longer_depends_on_the_old_binary() {
        let dir = tempfile::tempdir().unwrap();
        let task = LegacyTask {
            executable: dir.path().join("removed/codex-zectrix-companion.exe"),
            owner_sid: "S-1-5-21-test".into(),
            enabled: false,
            running: false,
        };
        fs::write(
            dir.path().join("legacy-migration.json"),
            serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"task":task})).unwrap(),
        )
        .unwrap();
        assert!(
            accept_legacy_task(dir.path(), Some(task))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn migration_receipt_cannot_hide_a_reenabled_task() {
        let dir = tempfile::tempdir().unwrap();
        let task = LegacyTask {
            executable: dir.path().join("removed/codex-zectrix-companion.exe"),
            owner_sid: "S-1-5-21-test".into(),
            enabled: true,
            running: false,
        };
        fs::write(
            dir.path().join("legacy-migration.json"),
            serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"task":task})).unwrap(),
        )
        .unwrap();
        assert!(accept_legacy_task(dir.path(), Some(task)).is_err());
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyTask {
    pub executable: PathBuf,
    pub owner_sid: String,
    pub enabled: bool,
    pub running: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedCompanion {
    pub pid: u32,
    pub executable: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyMigration {
    schema_version: u32,
    task: LegacyTask,
}

pub fn owned_companion_processes() -> Result<Vec<OwnedCompanion>, InstallError> {
    let bytes = powershell(
        r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$owned=@(foreach ($item in @(Get-CimInstance Win32_Process -Filter "Name='codex-zectrix-companion.exe'")) {
    $owner=Invoke-CimMethod -InputObject $item -MethodName GetOwnerSid
    if ($owner.ReturnValue -ne 0) { exit 2 }
    if ($owner.Sid -eq $sid) {
        if (!$item.ExecutablePath) { exit 3 }
        @{pid=[uint32]$item.ProcessId;executable=$item.ExecutablePath}
    }
})
ConvertTo-Json -InputObject $owned -Compress"#,
    )?;
    serde_json::from_slice(&bytes).map_err(|_| InstallError::LegacyStop)
}

fn validate_companion_processes(
    processes: &[OwnedCompanion],
    snapshot: &HostSnapshot,
) -> Result<(), InstallError> {
    // Only the worker whose identity was checked through the controller or the
    // known scheduled task can be stopped. An unmanaged worker requires action
    // by its owner; do not start a second uploader alongside it.
    if processes.len() > 1 {
        return Err(InstallError::LegacyStop);
    }
    for process in processes {
        let expected = snapshot
            .running_executable
            .as_ref()
            .ok_or(InstallError::LegacyStop)?;
        if fs::canonicalize(&process.executable)? != fs::canonicalize(expected)? {
            return Err(InstallError::LegacyStop);
        }
    }
    Ok(())
}

pub struct WindowsHost {
    data: PathBuf,
}
impl WindowsHost {
    pub fn new(data: &Path) -> Self {
        Self { data: data.into() }
    }
}
fn powershell(code: &str) -> Result<Vec<u8>, InstallError> {
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", code])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        return Err(InstallError::LegacyStop);
    }
    Ok(output.stdout)
}
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
pub fn inspect_legacy(data: &Path) -> Result<Option<LegacyTask>, InstallError> {
    let bytes = powershell(
        r#"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false)
$task=Get-ScheduledTask -TaskPath '\' -TaskName 'Codex ZECTRIX Dashboard' -ErrorAction SilentlyContinue
if (!$task) { 'null'; exit 0 }
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$owner=$task.Principal.UserId
if ($owner -notmatch '^S-1-') { $owner=([Security.Principal.NTAccount]::new($owner)).Translate([Security.Principal.SecurityIdentifier]).Value }
if ($owner -ne $sid -or @($task.Actions).Count -ne 1 -or $task.Actions[0].Arguments) { exit 2 }
@{executable=$task.Actions[0].Execute.Trim('"');ownerSid=$owner;enabled=[bool]$task.Settings.Enabled;running=($task.State -eq 'Running')} | ConvertTo-Json -Compress"#,
    )?;
    let task: Option<LegacyTask> =
        serde_json::from_slice(&bytes).map_err(|_| InstallError::LegacyStop)?;
    accept_legacy_task(data, task)
}

fn accept_legacy_task(
    data: &Path,
    task: Option<LegacyTask>,
) -> Result<Option<LegacyTask>, InstallError> {
    if let Some(task) = &task {
        if let Some(bytes) = read_optional(&data.join("legacy-migration.json"))? {
            let receipt: LegacyMigration =
                serde_json::from_slice(&bytes).map_err(|_| InstallError::InvalidState)?;
            if receipt.schema_version != 1 {
                return Err(InstallError::InvalidState);
            }
            if !task.enabled
                && !task.running
                && task.executable == receipt.task.executable
                && task.owner_sid == receipt.task.owner_sid
            {
                return Ok(None);
            }
        }
        validate_legacy(task)?;
    }
    Ok(task)
}
fn validate_legacy(task: &LegacyTask) -> Result<(), InstallError> {
    if !task.executable.is_absolute()
        || task.executable.file_name().and_then(|s| s.to_str())
            != Some("codex-zectrix-companion.exe")
        || !task.owner_sid.starts_with("S-1-")
    {
        return Err(InstallError::LegacyStop);
    }
    reject_link(&task.executable)?;
    if format!("{:x}", Sha256::digest(fs::read(&task.executable)?)) != LEGACY_HASH {
        return Err(InstallError::LegacyStop);
    }
    Ok(())
}
fn legacy_change(task: &LegacyTask, restore: bool) -> Result<(), InstallError> {
    validate_legacy(task)?;
    let action = if restore {
        format!(
            "{}; {}",
            if task.enabled {
                "Enable-ScheduledTask -InputObject $task | Out-Null"
            } else {
                "Disable-ScheduledTask -InputObject $task | Out-Null"
            },
            if task.running {
                "Start-ScheduledTask -InputObject $task"
            } else {
                "$null"
            }
        )
    } else {
        "Disable-ScheduledTask -InputObject $task | Out-Null; Stop-ScheduledTask -InputObject $task; $deadline=[DateTime]::UtcNow.AddSeconds(30); do { $running=@(Get-CimInstance Win32_Process -Filter \"Name='codex-zectrix-companion.exe'\" | Where-Object { $_.ExecutablePath -eq $expected }); if (!$running.Count) { break }; Start-Sleep -Milliseconds 200 } while ([DateTime]::UtcNow -lt $deadline); if ($running.Count) {exit 3}".into()
    };
    let code = format!(
        r#"$ErrorActionPreference='Stop'; try {{
$expected={path}; $ownerExpected={sid};
$task=Get-ScheduledTask -TaskPath '\' -TaskName 'Codex ZECTRIX Dashboard'
$owner=$task.Principal.UserId
if ($owner -notmatch '^S-1-') {{ $owner=([Security.Principal.NTAccount]::new($owner)).Translate([Security.Principal.SecurityIdentifier]).Value }}
if ($owner -ne $ownerExpected -or @($task.Actions).Count -ne 1 -or $task.Actions[0].Arguments -or $task.Actions[0].Execute.Trim('"') -ne $expected -or (Get-FileHash -LiteralPath $expected -Algorithm SHA256).Hash -ne '{LEGACY_HASH}') {{exit 2}}
{action}; exit 0 }} catch {{exit 4}}"#,
        path = ps_quote(&task.executable.to_string_lossy()),
        sid = ps_quote(&task.owner_sid)
    );
    if powershell(&code).is_ok() {
        return Ok(());
    }
    // Only this fixed, ownership-checked legacy task operation may require UAC.
    let wrapper = format!(
        "$ErrorActionPreference='Stop'; $code={}; $encoded=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code)); try {{$p=Start-Process powershell.exe -ArgumentList @('-NoProfile','-NonInteractive','-EncodedCommand',$encoded) -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode}} catch {{exit 5}}",
        ps_quote(&code)
    );
    powershell(&wrapper).map(|_| ())
}
impl InstallHost for WindowsHost {
    fn snapshot(&mut self) -> Result<HostSnapshot, InstallError> {
        let startup = CurrentUserRun
            .read()
            .map_err(|_| InstallError::InvalidState)?;
        let legacy = inspect_legacy(&self.data)?;
        if let Some(command) = &startup {
            self.validate_command(command)?;
        }
        let recorded = self.data.join("background.json");
        let mut running_executable = None;
        if let Some(bytes) = read_optional(&recorded)? {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| InstallError::InvalidState)?;
            let exe = PathBuf::from(
                value["executable"]
                    .as_str()
                    .ok_or(InstallError::InvalidState)?,
            );
            self.validate_installed_executable(&exe)?;
            if BackgroundController::new(&self.data, &exe).status()? != BackgroundStatus::Stopped {
                running_executable = Some(exe);
            }
        }
        if running_executable.is_none()
            && let Some(task) = &legacy
            && task.running
        {
            running_executable = Some(task.executable.clone());
        }
        let snapshot = HostSnapshot {
            startup,
            running_executable,
            legacy,
        };
        validate_companion_processes(&owned_companion_processes()?, &snapshot)?;
        Ok(snapshot)
    }
    fn verify_binaries(&mut self, directory: &Path, version: &str) -> Result<(), InstallError> {
        let package = Package::load(directory)?;
        for name in package
            .manifest
            .allowed_files()?
            .iter()
            .copied()
            .filter(|name| name.ends_with(".exe"))
        {
            let output = Command::new(directory.join(name))
                .arg("version")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .output()?;
            if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != version
            {
                return Err(InstallError::InvalidPackage);
            }
        }
        Ok(())
    }
    fn quiesce(&mut self, previous: &HostSnapshot) -> Result<(), InstallError> {
        if let Some(legacy) = &previous.legacy
            && (legacy.enabled || legacy.running)
        {
            legacy_change(legacy, false)?;
        }
        if let Some(exe) = &previous.running_executable
            && previous
                .legacy
                .as_ref()
                .is_none_or(|task| task.executable != *exe)
        {
            self.validate_installed_executable(exe)?;
            BackgroundController::new(&self.data, exe).stop()?;
        }
        Ok(())
    }
    fn activate(
        &mut self,
        executable: &Path,
        previous: &HostSnapshot,
        guard: &OperationGuard,
    ) -> Result<(), InstallError> {
        self.validate_installed_executable(executable)?;
        if previous.running_executable.is_some() {
            BackgroundController::new(&self.data, executable)
                .start_guarded(guard)
                .map_err(|_| InstallError::Health)?;
        }
        if previous.startup.is_some() || previous.legacy.as_ref().is_some_and(|task| task.enabled) {
            CurrentUserRun
                .write(&quoted_run_command(executable).map_err(|_| InstallError::InvalidState)?)
                .map_err(|_| InstallError::InvalidState)?;
        }
        if let Some(task) = &previous.legacy {
            let receipt = LegacyMigration {
                schema_version: 1,
                task: task.clone(),
            };
            atomic_write(
                &self.data.join("legacy-migration.json"),
                &serde_json::to_vec(&receipt).map_err(|_| InstallError::InvalidState)?,
            )?;
        }
        Ok(())
    }
    fn restore(
        &mut self,
        previous: &HostSnapshot,
        guard: &OperationGuard,
    ) -> Result<(), InstallError> {
        match &previous.startup {
            Some(command) => {
                self.validate_command(command)?;
                CurrentUserRun.write(command)
            }
            None => CurrentUserRun.remove(),
        }
        .map_err(|_| InstallError::Recovery)?;
        if let Some(legacy) = &previous.legacy
            && inspect_legacy(&self.data)?.as_ref() != Some(legacy)
        {
            legacy_change(legacy, true)?;
        }
        if let Some(exe) = &previous.running_executable
            && previous
                .legacy
                .as_ref()
                .is_none_or(|task| task.executable != *exe)
        {
            self.validate_installed_executable(exe)?;
            BackgroundController::new(&self.data, exe).start_guarded(guard)?;
        }
        Ok(())
    }
}
impl WindowsHost {
    fn validate_command(&self, command: &str) -> Result<(), InstallError> {
        let path = command
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .ok_or(InstallError::InvalidState)?;
        let path = Path::new(path);
        self.validate_installed_executable(path)?;
        if quoted_run_command(path).map_err(|_| InstallError::InvalidState)? != command {
            return Err(InstallError::InvalidState);
        }
        Ok(())
    }
    fn validate_installed_executable(&self, exe: &Path) -> Result<(), InstallError> {
        let parent = exe.parent().ok_or(InstallError::InvalidState)?;
        reject_link(&self.data.join("app"))?;
        reject_link(parent)?;
        reject_link(exe)?;
        let parent_parent = parent.parent().ok_or(InstallError::InvalidState)?;
        if fs::canonicalize(parent_parent)? != fs::canonicalize(self.data.join("app"))?
            || exe.file_name().and_then(|s| s.to_str()) != Some("codex-zectrix-companion.exe")
        {
            return Err(InstallError::InvalidState);
        }
        Package::load(parent)?;
        Ok(())
    }
}
