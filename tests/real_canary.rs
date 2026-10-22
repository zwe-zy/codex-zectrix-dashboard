#![cfg(unix)]

use std::fs;
use std::fs::OpenOptions;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::time::{Duration, Instant};

mod common;

#[test]
fn real_canary_allows_initialization_staging_to_return_to_baseline_before_steady_state() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("transient");

    let output = fixture.run(2);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("initialization=passed"));
    assert!(stdout.contains("steady_state=passed"));
    assert!(stdout.contains("initialization_quota_source=current"));
    assert!(stdout.contains("steady_task_activity_source=inferred"));
    assert!(stdout.contains("owned_process_cleanup=passed"));
    assert!(stdout.contains("global_new_mcp_processes=0"));
    assert!(stdout.contains("physical_note4=not_run"));
    fixture.assert_app_server_exited();
    fixture.assert_launch_agent_was_only_observed();
}

#[test]
fn real_canary_rejects_staging_growth_during_steady_state_and_cleans_up() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("steady-growth");

    let output = fixture.run(4);

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("gate_failed=steady_staging_changed"),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("physical_note4=not_run"));
    fixture.assert_app_server_exited();
    fixture.assert_launch_agent_was_only_observed();
}

#[test]
fn real_canary_does_not_treat_an_unknown_launchctl_failure_as_unloaded() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("transient");

    let output = fixture.run_with_launchctl_mode(2, "generic-error");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gate_failed=launch_agent_not_proven_unloaded_before_start"));
    assert!(!fixture.app_pid.exists());
    fixture.assert_launch_agent_was_only_observed();
}

#[test]
fn real_canary_rejects_stale_task_activity() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("stale-activity");

    let output = fixture.run(1);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gate_failed=initialization_task_activity_source_invalid"));
    fixture.assert_app_server_exited();
}

#[test]
fn real_canary_accepts_expired_running_activity_as_inferred() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("expired-running-activity");

    let output = fixture.run(1);

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("initialization_task_activity_source=inferred"));
    assert!(stdout.contains("steady_task_activity_source=inferred"));
    assert!(stdout.contains("owned_process_cleanup=passed"));
}

#[test]
fn real_canary_rejects_staging_growth_during_shutdown() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("shutdown-growth");

    let output = fixture.run(1);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gate_failed=final_staging_changed"));
    fixture.assert_app_server_exited();
}

#[test]
fn real_canary_rejects_a_companion_that_leaves_its_app_server_running() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("leaked-app-server");

    let output = fixture.run(1);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gate_failed=owned_process_cleanup"));
    fixture.assert_app_server_exited();
}

#[test]
fn real_canary_rejects_an_escaped_owned_descendant() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("leaked-escaped-descendant");

    let output = fixture.run(1);

    let escaped_pid: libc::pid_t = fs::read_to_string(&fixture.escaped_pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let escaped_process_was_cleaned = wait_until_process_exits(escaped_pid);
    if !escaped_process_was_cleaned {
        unsafe {
            libc::kill(escaped_pid, libc::SIGKILL);
        }
        assert!(wait_until_process_exits(escaped_pid));
    }
    assert!(
        !output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gate_failed=owned_process_cleanup"));
    assert!(escaped_process_was_cleaned);
}

#[test]
fn real_canary_restores_an_agent_loaded_during_shutdown_and_reports_the_exact_gate() {
    let _lock = canary_test_lock();
    let fixture = CanaryFixture::new("launch-agent-during-shutdown");

    let output = fixture.run(1);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gate_failed=final_launch_agent_loaded"));
    assert!(!fixture.launch_agent_started.exists());
    let calls = fs::read_to_string(&fixture.launchctl_log).unwrap();
    assert!(calls.lines().any(|line| line.starts_with("bootout gui/")));
    fixture.assert_app_server_exited();
}

struct CanaryFixture {
    _temp: tempfile::TempDir,
    companion: std::path::PathBuf,
    codex: std::path::PathBuf,
    codex_home: std::path::PathBuf,
    data_dir: std::path::PathBuf,
    launchctl: std::path::PathBuf,
    security: std::path::PathBuf,
    launchctl_log: std::path::PathBuf,
    app_pid: std::path::PathBuf,
    escaped_pid: std::path::PathBuf,
    initialized: std::path::PathBuf,
    launch_agent_started: std::path::PathBuf,
    mode: &'static str,
}

impl CanaryFixture {
    fn new(mode: &'static str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let codex_home = temp.path().join("codex-home");
        let data_dir = temp.path().join("data");
        let staging = codex_home.join(".tmp/marketplaces/.staging");
        fs::create_dir_all(&staging).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(
            data_dir.join("settings.json"),
            r#"{"deviceId":"TEST_DEVICE","pageId":3,"privacyMode":false}"#,
        )
        .unwrap();
        let launchctl_log = temp.path().join("launchctl.log");
        let app_pid = temp.path().join("app-server.pid");
        let escaped_pid = temp.path().join("escaped.pid");
        let initialized = temp.path().join("initialized");
        let launch_agent_started = temp.path().join("launch-agent-started");
        let codex = if mode == "expired-running-activity" {
            let sessions = codex_home.join("sessions");
            fs::create_dir_all(&sessions).unwrap();
            fs::write(
                sessions.join("rollout-expired.jsonl"),
                concat!(
                    "{\"type\":\"session_meta\",\"payload\":{\"id\":\"task-1\"}}\n",
                    "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":1}}\n"
                ),
            )
            .unwrap();
            executable(
                temp.path().join("codex"),
                r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"id":1,"result":{"userAgent":"codex-zectrix-dashboard/test","platformFamily":"unix","platformOs":"macos","codexHome":"/tmp/codex"}}'
read -r initialized
while read -r request; do
  request_id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *account/rateLimits/read*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"rateLimits\":{\"primary\":{\"usedPercent\":37,\"windowDurationMins\":300,\"resetsAt\":4102444800},\"secondary\":null},\"rateLimitResetCredits\":{\"availableCount\":0}}}" ;;
    *thread/list*) printf '%s\n' "{\"id\":$request_id,\"result\":{\"data\":[{\"id\":\"task-1\",\"sessionId\":\"session-1\",\"name\":\"TEST_TASK\",\"parentThreadId\":null,\"source\":\"appServer\"}],\"nextCursor\":null}}" ;;
    *) printf '%s\n' "{\"id\":$request_id,\"error\":{\"code\":-1,\"message\":\"unsupported\"}}" ;;
  esac
done
"#,
            )
        } else {
            executable(
                temp.path().join("codex"),
                r#"#!/bin/sh
if [ "$TEST_CANARY_MODE" = leaked-escaped-descendant ]; then
  /usr/bin/perl -MPOSIX -e 'POSIX::setsid(); open my $pid_file, ">", $ENV{TEST_ESCAPED_PID_FILE}; print {$pid_file} "$$\n"; close $pid_file; $SIG{TERM} = sub { exit 0 }; sleep 1 while 1' </dev/null >/dev/null 2>&1 &
fi
trap 'exit 0' TERM INT
while :; do sleep 1; done
"#,
            )
        };
        let companion = if mode == "expired-running-activity" {
            common::dashboard_binary()
        } else {
            executable(
                temp.path().join("companion"),
                r#"#!/bin/sh
set -u
if [ "${1:-}" = diagnostics ]; then
  if [ "$TEST_CANARY_MODE" = steady-growth ]; then
    : > "$TEST_INITIALIZED_FILE"
  fi
  task_source=inferred
  if [ "$TEST_CANARY_MODE" = stale-activity ]; then task_source=stale; fi
  printf 'quota_source=current\ntask_activity_source=%s\n' "$task_source"
  exit 0
fi
app_pid=
cleanup() {
  if [ "$TEST_CANARY_MODE" = leaked-app-server ]; then return; fi
  if [ -n "$app_pid" ]; then
    kill -TERM "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  if [ "$TEST_CANARY_MODE" = shutdown-growth ]; then
    mkdir -p "$TEST_STAGING_ROOT/marketplace-upgrade-shutdown"
    printf 'growth\n' > "$TEST_STAGING_ROOT/marketplace-upgrade-shutdown/entry"
  fi
  if [ "$TEST_CANARY_MODE" = launch-agent-during-shutdown ]; then
    : > "$TEST_LAUNCH_AGENT_STARTED_FILE"
  fi
}
trap 'cleanup; exit 0' TERM INT
/usr/bin/perl -e 'setpgrp(0, 0); exec @ARGV' "$CODEX_ZECTRIX_CODEX_BIN" app-server --stdio &
app_pid=$!
printf '%s\n' "$app_pid" > "$TEST_APP_PID_FILE"
case "$TEST_CANARY_MODE" in
  transient)
    mkdir -p "$TEST_STAGING_ROOT/marketplace-upgrade-transient"
    printf 'transient\n' > "$TEST_STAGING_ROOT/marketplace-upgrade-transient/entry"
    sleep 1
    find "$TEST_STAGING_ROOT/marketplace-upgrade-transient" -depth -delete
    ;;
  steady-growth)
    while [ ! -f "$TEST_INITIALIZED_FILE" ]; do sleep 1; done
    sleep 1
    mkdir -p "$TEST_STAGING_ROOT/marketplace-upgrade-steady"
    printf 'growth\n' > "$TEST_STAGING_ROOT/marketplace-upgrade-steady/entry"
    ;;
esac
while :; do sleep 1; done
"#,
            )
        };
        let launchctl = executable(
            temp.path().join("launchctl"),
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$TEST_LAUNCHCTL_LOG"
if [ "$TEST_CANARY_MODE" = launch-agent-during-shutdown ] && [ -f "$TEST_LAUNCH_AGENT_STARTED_FILE" ]; then
  if [ "$1" = bootout ]; then
    find "$TEST_LAUNCH_AGENT_STARTED_FILE" -delete
    exit 0
  fi
  exit 0
fi
if [ "$TEST_LAUNCHCTL_MODE" = generic-error ]; then
  printf 'launchctl transport failed\n' >&2
  exit 1
fi
printf 'Could not find service "%s" in domain for user\n' "$2" >&2
exit 113
"#,
        );
        let security = executable(temp.path().join("security"), "#!/bin/sh\nexit 44\n");
        executable(
            temp.path().join("ps"),
            "#!/bin/sh\n/bin/ps \"$@\" | /usr/bin/awk '!index($0, \"/plugins/cache/codex-zectrix-dashboard/\")'\n",
        );
        Self {
            _temp: temp,
            companion,
            codex,
            codex_home,
            data_dir,
            launchctl,
            security,
            launchctl_log,
            app_pid,
            escaped_pid,
            initialized,
            launch_agent_started,
            mode,
        }
    }

    fn run(&self, steady_seconds: u64) -> std::process::Output {
        self.run_with_launchctl_mode(steady_seconds, "not-found")
    }

    fn run_with_launchctl_mode(
        &self,
        steady_seconds: u64,
        launchctl_mode: &str,
    ) -> std::process::Output {
        Command::new("scripts/real-runtime-canary.sh")
            .arg(&self.companion)
            .env("CODEX_HOME", &self.codex_home)
            .env("CODEX_ZECTRIX_DATA_DIR", &self.data_dir)
            .env("CODEX_ZECTRIX_CODEX_BIN", &self.codex)
            .env("CODEX_ZECTRIX_SECURITY_BIN", &self.security)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self._temp.path().display()),
            )
            .env("CANARY_LAUNCHCTL_BIN", &self.launchctl)
            .env(
                "CANARY_INITIALIZATION_TIMEOUT_SECONDS",
                if self.mode == "expired-running-activity" {
                    "20"
                } else {
                    "8"
                },
            )
            .env("CANARY_INITIALIZATION_QUIET_SECONDS", "2")
            .env("CANARY_STEADY_SECONDS", steady_seconds.to_string())
            .env("CANARY_SAMPLE_SECONDS", "1")
            .env(
                "CANARY_SHUTDOWN_TIMEOUT_SECONDS",
                if self.mode == "expired-running-activity" {
                    "20"
                } else {
                    "4"
                },
            )
            .env("TEST_APP_PID_FILE", &self.app_pid)
            .env("TEST_ESCAPED_PID_FILE", &self.escaped_pid)
            .env("TEST_INITIALIZED_FILE", &self.initialized)
            .env("TEST_LAUNCH_AGENT_STARTED_FILE", &self.launch_agent_started)
            .env("TEST_CANARY_MODE", self.mode)
            .env(
                "TEST_STAGING_ROOT",
                self.codex_home.join(".tmp/marketplaces/.staging"),
            )
            .env("TEST_LAUNCHCTL_LOG", &self.launchctl_log)
            .env("TEST_LAUNCHCTL_MODE", launchctl_mode)
            .output()
            .unwrap()
    }

    fn assert_app_server_exited(&self) {
        let pid: libc::pid_t = fs::read_to_string(&self.app_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    fn assert_launch_agent_was_only_observed(&self) {
        let calls = fs::read_to_string(&self.launchctl_log).unwrap();
        assert!(!calls.is_empty());
        assert!(calls.lines().all(|line| line.starts_with("print gui/")));
    }
}

fn canary_test_lock() -> fs::File {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open("/tmp/codex-zectrix-real-canary-tests.lock")
        .unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    lock
}

fn executable(path: std::path::PathBuf, contents: &str) -> std::path::PathBuf {
    fs::write(&path, contents).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

fn wait_until_process_exits(pid: libc::pid_t) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
