use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use codex_zectrix_dashboard::status_board::{BoardTaskState, StatusBoardState};
use codex_zectrix_dashboard::{
    AppFile, AutostartControl, AutostartError, AutostartStatus, CodexReachability, ConsoleTerminal,
    CredentialError, CredentialStore, OperatorError, RuntimeStatus, SetupBoundaryError, SetupClock,
    SetupCloud, SetupCloudError, SetupDevice, SetupError, SetupFileStore, SetupOutcome,
    SetupPreview, SetupRenderer, SetupTerminal, StatusFrameAttempt, StatusFrameCoordinator,
    StatusFramePublishError, StatusFramePublisher, StatusFramePublisherState,
    StatusFrameRenderError, StatusFrameRenderer, StatusProbe, UninstallOutcome, UploadCompletion,
    run_operator_status, run_setup_transaction, run_uninstall, write_deterministic_preview,
};
use zeroize::Zeroizing;

const SECRET: &str = "SECRET_API_KEY_SENTINEL";
const DEVICE_ID: &str = "SECRET_DEVICE_ID_SENTINEL";
const PNG_SENTINEL: &[u8] = b"\x89PNG\r\n\x1a\nPRIVATE_PNG_SENTINEL";
const COMPLETION: i64 = 1_786_330_123;

struct BrokenWriter;

impl Write for BrokenWriter {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"))
    }
}

struct FlushFailingWriter;

impl Write for FlushFailingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"))
    }
}

#[test]
fn console_terminal_propagates_closed_output_for_lines_and_prompts() {
    // Catches print!/println! panics or a prompt continuing to read after output is already closed.
    let mut line_terminal = ConsoleTerminal::with_output(BrokenWriter);
    assert_eq!(
        line_terminal.write_line("fixed line"),
        Err(SetupBoundaryError)
    );
    let mut flush_terminal = ConsoleTerminal::with_output(FlushFailingWriter);
    assert_eq!(
        flush_terminal.write_line("buffered fixed line"),
        Err(SetupBoundaryError)
    );

    let mut prompt_terminal = ConsoleTerminal::with_output(BrokenWriter);
    assert_eq!(
        prompt_terminal.read_line("fixed prompt"),
        Err(SetupBoundaryError)
    );

    let mut secret_terminal = ConsoleTerminal::with_output(BrokenWriter);
    assert_eq!(
        secret_terminal.read_secret("fixed secret prompt"),
        Err(SetupBoundaryError)
    );
}

#[derive(Clone)]
struct Events(Arc<Mutex<Vec<&'static str>>>);

impl Events {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn push(&self, event: &'static str) {
        self.0.lock().unwrap().push(event);
    }

    fn get(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

struct FakeTerminal {
    secret: String,
    answers: VecDeque<Option<String>>,
    transcript: Vec<String>,
    events: Events,
    secret_reads: usize,
    fail_output_line: Option<String>,
}

impl FakeTerminal {
    fn new(confirmation: Option<&str>, events: Events) -> Self {
        Self {
            secret: SECRET.into(),
            answers: [
                Some("1".into()),
                Some("3".into()),
                confirmation.map(str::to_owned),
            ]
            .into(),
            transcript: Vec::new(),
            events,
            secret_reads: 0,
            fail_output_line: None,
        }
    }
}

impl SetupTerminal for FakeTerminal {
    fn read_secret(&mut self, prompt: &str) -> Result<Zeroizing<String>, SetupBoundaryError> {
        self.events.push("secret");
        self.secret_reads += 1;
        self.transcript.push(prompt.into());
        Ok(Zeroizing::new(self.secret.clone()))
    }

    fn read_line(&mut self, prompt: &str) -> Result<Option<String>, SetupBoundaryError> {
        let event = if prompt == "选择 NOTE4 设备编号：" {
            "device_selection"
        } else if prompt == "选择页面（1-5）：" {
            "page_selection"
        } else if prompt == "确认首次上传到这块屏幕？[y/N]" {
            "confirmation"
        } else {
            "other_prompt"
        };
        self.events.push(event);
        self.transcript.push(prompt.into());
        Ok(self.answers.pop_front().unwrap_or(None))
    }

    fn write_line(&mut self, line: &str) -> Result<(), SetupBoundaryError> {
        if self.fail_output_line.as_deref() == Some(line) {
            return Err(SetupBoundaryError);
        }
        self.transcript.push(line.into());
        Ok(())
    }
}

struct FakeCloud {
    events: Events,
    upload_result: Result<(), SetupCloudError>,
    uploads: usize,
    fail_discovery: bool,
    empty_discovery: bool,
}

impl FakeCloud {
    fn success(events: Events) -> Self {
        Self {
            events,
            upload_result: Ok(()),
            uploads: 0,
            fail_discovery: false,
            empty_discovery: false,
        }
    }
}

impl SetupCloud for FakeCloud {
    fn discover(&mut self, secret: &str) -> Result<Vec<SetupDevice>, SetupCloudError> {
        self.events.push("discover");
        assert_eq!(secret, SECRET);
        if self.fail_discovery {
            return Err(SetupCloudError::DiscoveryUnavailable);
        }
        if self.empty_discovery {
            return Ok(Vec::new());
        }
        Ok(vec![SetupDevice::new(DEVICE_ID)])
    }

    fn upload_png(
        &mut self,
        secret: &str,
        device_id: &str,
        page_id: u8,
        png: &[u8],
    ) -> Result<(), SetupCloudError> {
        self.events.push("upload");
        self.uploads += 1;
        assert_eq!(secret, SECRET);
        assert_eq!(device_id, DEVICE_ID);
        assert_eq!(page_id, 3);
        assert_eq!(png, PNG_SENTINEL);
        self.upload_result
    }
}

struct FakeCredentials {
    value: Mutex<Option<String>>,
    fail_next_set: Mutex<bool>,
    fail_next_get: Mutex<bool>,
    events: Events,
    deletes: Mutex<usize>,
}

impl FakeCredentials {
    fn new(value: Option<&str>, events: Events) -> Self {
        Self {
            value: Mutex::new(value.map(str::to_owned)),
            fail_next_set: Mutex::new(false),
            fail_next_get: Mutex::new(false),
            events,
            deletes: Mutex::new(0),
        }
    }
}

impl CredentialStore for FakeCredentials {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError> {
        self.events.push("credential_get");
        if std::mem::take(&mut *self.fail_next_get.lock().unwrap()) {
            return Err(CredentialError::Read);
        }
        Ok(self.value.lock().unwrap().clone().map(Zeroizing::new))
    }

    fn set(&self, secret: &str) -> Result<(), CredentialError> {
        self.events.push("credential_set");
        if std::mem::take(&mut *self.fail_next_set.lock().unwrap()) {
            return Err(CredentialError::Write);
        }
        *self.value.lock().unwrap() = Some(secret.into());
        Ok(())
    }

    fn delete(&self) -> Result<(), CredentialError> {
        self.events.push("credential_delete");
        *self.deletes.lock().unwrap() += 1;
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}

struct MemoryFiles {
    files: HashMap<AppFile, Vec<u8>>,
    fail_write_once: Option<AppFile>,
    fail_write_always: Option<AppFile>,
    fail_read_once: Mutex<Option<AppFile>>,
    events: Events,
    preview_path: PathBuf,
    removed: Vec<AppFile>,
}

impl MemoryFiles {
    fn new(events: Events) -> Self {
        Self {
            files: HashMap::new(),
            fail_write_once: None,
            fail_write_always: None,
            fail_read_once: Mutex::new(None),
            events,
            preview_path: PathBuf::from(r"C:\safe-data\pending-preview.png"),
            removed: Vec::new(),
        }
    }
}

impl SetupFileStore for MemoryFiles {
    fn read(&self, file: AppFile) -> Result<Option<Vec<u8>>, SetupBoundaryError> {
        let mut failure = self.fail_read_once.lock().unwrap();
        if *failure == Some(file) {
            *failure = None;
            return Err(SetupBoundaryError);
        }
        Ok(self.files.get(&file).cloned())
    }

    fn write(&mut self, file: AppFile, bytes: &[u8]) -> Result<(), SetupBoundaryError> {
        self.events.push(match file {
            AppFile::PendingPreview => "preview_write",
            AppFile::Settings => "settings_write",
            AppFile::StatusFrameState => "state_write",
            _ => "other_write",
        });
        if self.fail_write_always == Some(file) {
            return Err(SetupBoundaryError);
        }
        if self.fail_write_once == Some(file) {
            self.fail_write_once = None;
            return Err(SetupBoundaryError);
        }
        self.files.insert(file, bytes.to_vec());
        Ok(())
    }

    fn remove(&mut self, file: AppFile) -> Result<(), SetupBoundaryError> {
        self.removed.push(file);
        self.files.remove(&file);
        Ok(())
    }

    fn path(&self, file: AppFile) -> &Path {
        assert_eq!(file, AppFile::PendingPreview);
        &self.preview_path
    }
}

struct FakeAutostart {
    status: AutostartStatus,
    fail_install_after_creation: bool,
    fail_uninstall: bool,
    events: Events,
    installs: usize,
    uninstalls: usize,
}

impl FakeAutostart {
    fn absent(events: Events) -> Self {
        Self {
            status: AutostartStatus::Absent,
            fail_install_after_creation: false,
            fail_uninstall: false,
            events,
            installs: 0,
            uninstalls: 0,
        }
    }
}

impl AutostartControl for FakeAutostart {
    fn install(&mut self) -> Result<(), AutostartError> {
        self.events.push("autostart_install");
        self.installs += 1;
        self.status = AutostartStatus::Present;
        if self.fail_install_after_creation {
            Err(AutostartError::Unavailable)
        } else {
            Ok(())
        }
    }

    fn uninstall(&mut self) -> Result<(), AutostartError> {
        self.events.push("autostart_uninstall");
        self.uninstalls += 1;
        if self.fail_uninstall {
            return Err(AutostartError::Unavailable);
        }
        self.status = AutostartStatus::Absent;
        Ok(())
    }

    fn status(&mut self) -> AutostartStatus {
        self.events.push("autostart_status");
        self.status
    }
}

struct FakeRenderer {
    events: Events,
    fail: bool,
}

impl SetupRenderer for FakeRenderer {
    fn render_sample(&mut self) -> Result<SetupPreview, SetupBoundaryError> {
        self.events.push("render");
        if self.fail {
            return Err(SetupBoundaryError);
        }
        Ok(SetupPreview::new(sample_state(), PNG_SENTINEL.to_vec()))
    }
}

struct FixedClock;

impl SetupClock for FixedClock {
    fn now_epoch_seconds(&self) -> i64 {
        COMPLETION
    }
}

struct SequenceClock {
    values: Mutex<VecDeque<i64>>,
    samples: Mutex<usize>,
    events: Events,
}

impl SequenceClock {
    fn new(values: impl IntoIterator<Item = i64>, events: Events) -> Self {
        Self {
            values: Mutex::new(values.into_iter().collect()),
            samples: Mutex::new(0),
            events,
        }
    }
}

impl SetupClock for SequenceClock {
    fn now_epoch_seconds(&self) -> i64 {
        let mut samples = self.samples.lock().unwrap();
        self.events.push(match *samples {
            0 => "clock_request_start",
            1 => "clock_upload_completion",
            _ => "clock_unexpected",
        });
        *samples += 1;
        self.values
            .lock()
            .unwrap()
            .pop_front()
            .expect("the setup clock was sampled more often than expected")
    }
}

fn sample_state() -> StatusBoardState {
    StatusBoardState {
        is_sample: true,
        task_state: BoardTaskState::Empty,
        quota_remaining_percent: Some(64.0),
        quota_resets_at_epoch_seconds: Some(1_786_333_600),
        quota_stale: false,
        task_stale: false,
        uploaded_at_epoch_seconds: None,
        ..Default::default()
    }
}

struct Rig {
    events: Events,
    terminal: FakeTerminal,
    cloud: FakeCloud,
    credentials: FakeCredentials,
    files: MemoryFiles,
    autostart: FakeAutostart,
    renderer: FakeRenderer,
    control: FakeControl,
}

struct FakeControl {
    events: Events,
}
impl codex_zectrix_dashboard::SetupControl for FakeControl {
    fn begin(&mut self) -> Result<(), SetupError> {
        self.events.push("background_stopped");
        Ok(())
    }
    fn finish(
        &mut self,
        state: codex_zectrix_dashboard::SetupCommitState,
    ) -> Result<(), SetupError> {
        self.events.push(match state {
            codex_zectrix_dashboard::SetupCommitState::Configured => "new_background_started",
            codex_zectrix_dashboard::SetupCommitState::Restored => "old_background_restored",
            codex_zectrix_dashboard::SetupCommitState::Uncertain => "background_blocked",
        });
        Ok(())
    }
}

impl Rig {
    fn new(confirmation: Option<&str>) -> Self {
        let events = Events::new();
        Self {
            terminal: FakeTerminal::new(confirmation, events.clone()),
            cloud: FakeCloud::success(events.clone()),
            credentials: FakeCredentials::new(None, events.clone()),
            files: MemoryFiles::new(events.clone()),
            autostart: FakeAutostart::absent(events.clone()),
            renderer: FakeRenderer {
                events: events.clone(),
                fail: false,
            },
            control: FakeControl {
                events: events.clone(),
            },
            events,
        }
    }

    fn run(&mut self) -> Result<SetupOutcome, SetupError> {
        self.run_with_clock(&FixedClock)
    }

    fn run_with_clock(&mut self, clock: &impl SetupClock) -> Result<SetupOutcome, SetupError> {
        run_setup_transaction(
            &mut self.terminal,
            &mut self.cloud,
            &self.credentials,
            &mut self.files,
            &mut self.autostart,
            &mut self.renderer,
            clock,
            &mut self.control,
        )
    }
}

#[test]
fn autostart_failure_keeps_committed_route() {
    let mut rig = Rig::new(Some("y"));
    rig.autostart.fail_install_after_creation = true;
    assert_eq!(
        rig.run().unwrap(),
        SetupOutcome::ConfiguredWithAutostartWarning
    );
    assert_eq!(
        rig.credentials.value.lock().unwrap().as_deref(),
        Some(SECRET)
    );
    assert!(rig.files.files.contains_key(&AppFile::Settings));
    assert!(rig.events.get().contains(&"new_background_started"));
    for bytes in rig.files.files.values() {
        assert!(!String::from_utf8_lossy(bytes).contains(SECRET));
    }
}
#[test]
fn reconfigure_stops_old_route_before_upload() {
    let mut rig = Rig::new(Some("y"));
    rig.run().unwrap();
    let events = rig.events.get();
    assert!(
        events
            .iter()
            .position(|e| *e == "background_stopped")
            .unwrap()
            < events.iter().position(|e| *e == "upload").unwrap()
    );
    let mut cancelled = Rig::new(Some("n"));
    cancelled.run().unwrap();
    assert!(!cancelled.events.get().contains(&"background_stopped"));
}

#[test]
fn no_devices_is_distinct_from_network_failure() {
    let mut rig = Rig::new(Some("y"));
    rig.cloud.empty_discovery = true;
    assert_eq!(rig.run(), Err(SetupError::NoDevices));
}

#[test]
fn every_non_single_y_confirmation_cancels_and_restores_preview() {
    // Catches permissive yes parsing or cancellation leaving any durable/external mutation.
    for confirmation in [Some(""), Some("n"), Some("yes"), Some("anything"), None] {
        let mut rig = Rig::new(confirmation);
        let old_preview = b"old preview bytes".to_vec();
        let old_settings = b"old settings bytes".to_vec();
        let old_state = b"old state bytes".to_vec();
        rig.files
            .files
            .insert(AppFile::PendingPreview, old_preview.clone());
        rig.files
            .files
            .insert(AppFile::Settings, old_settings.clone());
        rig.files
            .files
            .insert(AppFile::StatusFrameState, old_state.clone());

        assert_eq!(rig.run().unwrap(), SetupOutcome::Cancelled);

        assert_eq!(rig.cloud.uploads, 0);
        assert_eq!(rig.autostart.installs, 0);
        assert_eq!(*rig.credentials.value.lock().unwrap(), None);
        assert_eq!(rig.files.files[&AppFile::PendingPreview], old_preview);
        assert_eq!(rig.files.files[&AppFile::Settings], old_settings);
        assert_eq!(rig.files.files[&AppFile::StatusFrameState], old_state);
        assert_eq!(
            rig.terminal.transcript.last().map(String::as_str),
            Some("已取消，未上传。")
        );
    }
}

#[test]
fn preconfirmation_failures_make_no_persistent_or_upload_side_effect() {
    // Catches discovery/selection/render/preview failures leaking into the confirmed transaction.
    for failure in ["discovery", "empty", "device", "page", "render", "preview"] {
        let mut rig = Rig::new(Some("y"));
        let old_preview = b"prior preview".to_vec();
        rig.files
            .files
            .insert(AppFile::PendingPreview, old_preview.clone());
        match failure {
            "discovery" => rig.cloud.fail_discovery = true,
            "empty" => rig.cloud.empty_discovery = true,
            "device" => rig.terminal.answers[0] = Some("99".into()),
            "page" => rig.terminal.answers[1] = Some("0".into()),
            "render" => rig.renderer.fail = true,
            "preview" => rig.files.fail_write_once = Some(AppFile::PendingPreview),
            _ => unreachable!(),
        }

        let error = rig.run().unwrap_err();

        assert_eq!(rig.cloud.uploads, 0, "{failure}");
        assert_eq!(rig.autostart.installs, 0, "{failure}");
        assert_eq!(*rig.credentials.value.lock().unwrap(), None, "{failure}");
        assert_eq!(
            rig.files.files.get(&AppFile::PendingPreview),
            Some(&old_preview),
            "{failure}"
        );
        let public = format!("{}\n{error:?}\n{error}", rig.terminal.transcript.join("\n"));
        assert!(!public.contains(SECRET), "{failure}");
        assert!(!public.contains(DEVICE_ID), "{failure}");
    }
}

#[test]
fn confirmed_setup_orders_one_upload_before_state_and_autostart() {
    // Catches upload-before-confirmation, repeated uploads, wrong persistence order, and leaks.
    let mut rig = Rig::new(Some(" Y "));

    assert_eq!(rig.run().unwrap(), SetupOutcome::Configured);

    assert_eq!(rig.terminal.secret_reads, 1);
    assert_eq!(rig.cloud.uploads, 1);
    assert_eq!(rig.autostart.installs, 1);
    let events = rig.events.get();
    for (left, right) in [
        ("secret", "discover"),
        ("discover", "device_selection"),
        ("device_selection", "page_selection"),
        ("page_selection", "render"),
        ("render", "preview_write"),
        ("preview_write", "confirmation"),
        ("confirmation", "credential_set"),
        ("credential_set", "settings_write"),
        ("settings_write", "upload"),
        ("upload", "state_write"),
        ("state_write", "autostart_install"),
    ] {
        assert!(
            position(&events, left) < position(&events, right),
            "{events:?}"
        );
    }
    let summary =
        "云端仅接收生成的 400×300 PNG 及路由所需的设备 ID 和页面 ID；不包含 Codex 任务标题或内容。";
    assert!(
        position_string(&rig.terminal.transcript, summary)
            < position_string(&rig.terminal.transcript, "确认首次上传到这块屏幕？[y/N]")
    );
    let public = rig.terminal.transcript.join("\n");
    for forbidden in [
        SECRET,
        DEVICE_ID,
        "PRIVATE_PNG_SENTINEL",
        "task-title-sentinel",
    ] {
        assert!(!public.contains(forbidden), "leaked {forbidden}");
    }
    let settings: serde_json::Value =
        serde_json::from_slice(&rig.files.files[&AppFile::Settings]).unwrap();
    assert_eq!(settings["deviceId"], DEVICE_ID);
    assert_eq!(settings["pageId"], 3);
    assert!(settings.get("apiKey").is_none());
}

#[test]
fn persisted_sample_defers_the_first_real_upload_without_claiming_content_time() {
    let mut rig = Rig::new(Some("y"));
    rig.run().unwrap();
    let persisted: StatusFramePublisherState =
        serde_json::from_slice(&rig.files.files[&AppFile::StatusFrameState]).unwrap();
    assert_eq!(
        persisted.last_successful_http_upload_epoch_seconds,
        Some(COMPLETION)
    );
    assert_eq!(persisted.last_successful_upload_epoch_seconds, None);
    assert_eq!(persisted.next_allowed_attempt_epoch_seconds, None);
    assert!(persisted.last_successful_frame_sha256.is_some());

    let mut coordinator = StatusFrameCoordinator::new(persisted);
    let mut live = sample_state();
    live.is_sample = false;
    assert!(coordinator.observe(live));
    let mut renderer = ReconstructedRenderer;
    let mut publisher = ReconstructedPublisher::default();
    assert_eq!(
        coordinator.try_publish(COMPLETION, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: COMPLETION + 60
        }
    );
    assert_eq!(publisher.uploads, 0);
    assert_eq!(
        coordinator.try_publish(COMPLETION + 60, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(publisher.uploads, 1);
    assert!(coordinator.has_pending());
}

#[test]
fn first_upload_uses_success_completion_time_after_long_cloud_latency() {
    // Catches sampling upload completion before the cloud call and making UPDATED due too early.
    let request_started = 1_000;
    let upload_completed = 1_121;
    let mut rig = Rig::new(Some("y"));
    let clock = SequenceClock::new([request_started, upload_completed], rig.events.clone());

    assert_eq!(
        rig.run_with_clock(&clock).unwrap(),
        SetupOutcome::Configured
    );

    let persisted: StatusFramePublisherState =
        serde_json::from_slice(rig.files.files.get(&AppFile::StatusFrameState).unwrap()).unwrap();
    assert_eq!(
        persisted.last_successful_http_upload_epoch_seconds,
        Some(upload_completed)
    );
    assert_eq!(persisted.last_successful_upload_epoch_seconds, None);
    let events = rig.events.get();
    assert!(position(&events, "clock_request_start") < position(&events, "upload"));
    assert!(position(&events, "upload") < position(&events, "clock_upload_completion"));

    let mut coordinator = StatusFrameCoordinator::new(persisted);
    let mut live = sample_state();
    live.is_sample = false;
    assert!(coordinator.observe(live));
    let mut renderer = CompletionCheckingRenderer {
        expected_upload_epoch_seconds: None,
    };
    let mut publisher = ReconstructedPublisher::default();
    assert_eq!(
        coordinator.try_publish(upload_completed + 59, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: upload_completed + 60,
        }
    );
    assert_eq!(publisher.uploads, 0);
    assert_eq!(
        coordinator.try_publish(upload_completed + 60, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(publisher.uploads, 1);
}

#[derive(Clone, Copy)]
enum FailurePoint {
    Credential,
    Settings,
    UploadRejected,
    UploadUnknown,
    State,
}

#[test]
fn every_confirmed_failure_restores_local_snapshots_and_uploads_at_most_once() {
    // Catches incomplete transaction rollback, deleting an old task, or compensating cloud upload.
    for failure in [
        FailurePoint::Credential,
        FailurePoint::Settings,
        FailurePoint::UploadRejected,
        FailurePoint::UploadUnknown,
        FailurePoint::State,
    ] {
        let mut rig = Rig::new(Some("y"));
        let old_credential = "OLD_CREDENTIAL_SENTINEL";
        *rig.credentials.value.lock().unwrap() = Some(old_credential.into());
        let snapshots = [
            (AppFile::Settings, b"old settings".to_vec()),
            (AppFile::StatusFrameState, b"old state".to_vec()),
            (AppFile::PendingPreview, b"old preview".to_vec()),
        ];
        for (file, bytes) in &snapshots {
            rig.files.files.insert(*file, bytes.clone());
        }
        match failure {
            FailurePoint::Credential => *rig.credentials.fail_next_set.lock().unwrap() = true,
            FailurePoint::Settings => rig.files.fail_write_once = Some(AppFile::Settings),
            FailurePoint::UploadRejected => {
                rig.cloud.upload_result = Err(SetupCloudError::UploadRejected)
            }
            FailurePoint::UploadUnknown => {
                rig.cloud.upload_result = Err(SetupCloudError::UploadResultUnknown)
            }
            FailurePoint::State => rig.files.fail_write_once = Some(AppFile::StatusFrameState),
        }

        let error = rig.run().unwrap_err();

        assert!(rig.cloud.uploads <= 1);
        assert_eq!(
            rig.credentials.value.lock().unwrap().as_deref(),
            Some(old_credential)
        );
        for (file, bytes) in &snapshots {
            assert_eq!(rig.files.files[file], *bytes);
        }
        assert_eq!(rig.autostart.status, AutostartStatus::Absent);
        let expected_error = match failure {
            FailurePoint::Credential => SetupError::CredentialPersistUnavailable,
            FailurePoint::Settings => SetupError::SettingsPersistUnavailable,
            FailurePoint::UploadRejected => SetupError::UploadRejected,
            FailurePoint::UploadUnknown => SetupError::UploadResultUnknown,
            FailurePoint::State => SetupError::StatePersistFailedScreenMayHaveChanged,
        };
        assert_eq!(error, expected_error);
        let safe = format!("{error:?}\n{error}");
        for forbidden in [
            SECRET,
            DEVICE_ID,
            old_credential,
            "old settings",
            "PRIVATE_PNG",
        ] {
            assert!(!safe.contains(forbidden));
        }
    }
}

#[test]
fn post_upload_rollback_failures_keep_the_irreversible_screen_warning() {
    // Catches rollback errors replacing the more important fact that the screen already changed.
    for failure in ["state"] {
        let mut rig = Rig::new(Some("y"));
        *rig.credentials.value.lock().unwrap() = Some("OLD_CREDENTIAL_SENTINEL".into());
        rig.files
            .files
            .insert(AppFile::Settings, b"old settings".to_vec());
        rig.files
            .files
            .insert(AppFile::StatusFrameState, b"old state".to_vec());
        rig.files
            .files
            .insert(AppFile::PendingPreview, b"old preview".to_vec());
        match failure {
            "state" => rig.files.fail_write_always = Some(AppFile::StatusFrameState),
            "autostart" => {
                rig.autostart.fail_install_after_creation = true;
                rig.autostart.fail_uninstall = true;
            }
            _ => unreachable!(),
        }

        let error = rig.run().unwrap_err();
        let public = format!("{error:?}\n{error}");

        assert_eq!(rig.cloud.uploads, 1, "{failure}");
        assert_eq!(
            rig.events
                .get()
                .into_iter()
                .filter(|event| *event == "upload")
                .count(),
            1,
            "{failure}"
        );
        assert!(
            public.contains("screen may already have changed"),
            "{public}"
        );
        assert!(
            public.contains("reversible local rollback may be incomplete"),
            "{public}"
        );
        for forbidden in [SECRET, DEVICE_ID, "OLD_CREDENTIAL_SENTINEL", "PRIVATE_PNG"] {
            assert!(!public.contains(forbidden), "{failure}");
        }
    }
}

#[test]
fn final_success_output_failure_does_not_misreport_a_completed_external_setup() {
    // Catches a closed console replacing successful upload/autostart with a generic local error.
    let mut rig = Rig::new(Some("y"));
    rig.terminal.fail_output_line = Some("首次状态牌已上传，自启动已配置。".into());

    let error = rig.run().unwrap_err();
    let public = error.to_string();

    assert_eq!(rig.cloud.uploads, 1);
    assert_eq!(rig.autostart.status, AutostartStatus::Present);
    assert!(rig.files.files.contains_key(&AppFile::Settings));
    assert!(rig.files.files.contains_key(&AppFile::StatusFrameState));
    assert_eq!(
        rig.credentials.value.lock().unwrap().as_deref(),
        Some(SECRET)
    );
    assert!(public.contains("setup completed"), "{public}");
    assert!(
        public.contains("screen may already have changed"),
        "{public}"
    );
    for forbidden in [SECRET, DEVICE_ID, "PRIVATE_PNG"] {
        assert!(!public.contains(forbidden));
    }
}

#[test]
fn failure_with_no_prior_state_removes_every_new_local_artifact() {
    // Catches rollback that only overwrites old values and leaves newly created files/key/task.
    let mut rig = Rig::new(Some("y"));
    rig.cloud.upload_result = Err(SetupCloudError::UploadRejected);

    assert!(rig.run().is_err());

    assert_eq!(*rig.credentials.value.lock().unwrap(), None);
    for file in [
        AppFile::Settings,
        AppFile::StatusFrameState,
        AppFile::PendingPreview,
    ] {
        assert!(!rig.files.files.contains_key(&file));
    }
    assert_eq!(rig.autostart.status, AutostartStatus::Absent);
}

#[test]
fn snapshot_failures_after_confirmation_restore_the_preexisting_preview() {
    // Catches early-`?` exits after preview generation that bypass the transaction rollback.
    for failure in ["settings", "state", "credential"] {
        let mut rig = Rig::new(Some("y"));
        let old_preview = b"prior preview must survive".to_vec();
        rig.files
            .files
            .insert(AppFile::PendingPreview, old_preview.clone());
        match failure {
            "settings" => *rig.files.fail_read_once.lock().unwrap() = Some(AppFile::Settings),
            "state" => *rig.files.fail_read_once.lock().unwrap() = Some(AppFile::StatusFrameState),
            "credential" => *rig.credentials.fail_next_get.lock().unwrap() = true,
            "autostart" => rig.autostart.status = AutostartStatus::Unavailable,
            _ => unreachable!(),
        }

        assert!(rig.run().is_err(), "{failure}");

        assert_eq!(
            rig.files.files.get(&AppFile::PendingPreview),
            Some(&old_preview),
            "{failure}"
        );
        assert_eq!(rig.cloud.uploads, 0, "{failure}");
        assert_eq!(rig.autostart.installs, 0, "{failure}");
    }
}

#[test]
fn an_existing_autostart_task_is_preserved_and_not_reinstalled() {
    // Catches overwriting or deleting a previously installed exact app task.
    let mut rig = Rig::new(Some("y"));
    rig.autostart.status = AutostartStatus::Present;

    assert_eq!(rig.run().unwrap(), SetupOutcome::Configured);

    assert_eq!(rig.autostart.installs, 0);
    assert_eq!(rig.autostart.uninstalls, 0);
    assert_eq!(rig.autostart.status, AutostartStatus::Present);
}

#[test]
fn deterministic_preview_is_a_true_one_bit_400_by_300_offline_png() {
    // Catches routing preview through legacy/network state or encoding grayscale at 8-bit depth.
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.png");
    let second = directory.path().join("second.png");

    write_deterministic_preview(&first).unwrap();
    write_deterministic_preview(&second).unwrap();

    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap()
    );
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(first).unwrap()));
    let reader = decoder.read_info().unwrap();
    let info = reader.info();
    assert_eq!((info.width, info.height), (400, 300));
    assert_eq!(info.bit_depth, png::BitDepth::One);
    assert_eq!(info.color_type, png::ColorType::Grayscale);
}

#[test]
fn preview_command_needs_only_output_and_touches_no_app_state() {
    // Catches accidentally retaining legacy input/source/credential/setup dependencies.
    let directory = tempfile::tempdir().unwrap();
    let output_path = directory.path().join("preview.png");
    let data_path = directory.path().join("must-not-exist");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args(["preview", "--output"])
        .arg(&output_path)
        .env("CODEX_ZECTRIX_DATA_DIR", &data_path)
        .env(
            "CODEX_ZECTRIX_CODEX_BIN",
            directory.path().join("must-not-run"),
        )
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "preview_written\n"
    );
    assert!(output_path.is_file());
    assert!(!data_path.exists());
}

struct FakeProbe {
    task: CodexReachability,
    quota: CodexReachability,
}

impl StatusProbe for FakeProbe {
    fn task_reachability(&mut self) -> CodexReachability {
        self.task
    }

    fn quota_reachability(&mut self) -> CodexReachability {
        self.quota
    }
}

#[test]
fn status_uses_only_fixed_categories_and_timestamps_and_fails_closed_on_corruption() {
    // Catches rendering raw settings/runtime/source errors, identifiers, paths, or task content.
    let events = Events::new();
    let mut terminal = FakeTerminal::new(Some("n"), events.clone());
    terminal.transcript.clear();
    let mut files = MemoryFiles::new(events.clone());
    files.files.insert(
        AppFile::Settings,
        serde_json::to_vec(&serde_json::json!({
            "deviceId": DEVICE_ID,
            "pageId": 3
        }))
        .unwrap(),
    );
    files.files.insert(
        AppFile::RuntimeStatus,
        serde_json::to_vec(&RuntimeStatus {
            last_successful_task_refresh_epoch_seconds: Some(101),
            last_successful_quota_refresh_epoch_seconds: Some(202),
            last_successful_upload_epoch_seconds: Some(303),
        })
        .unwrap(),
    );
    let mut autostart = FakeAutostart::absent(events);
    autostart.status = AutostartStatus::Present;
    let mut probe = FakeProbe {
        task: CodexReachability::Supported,
        quota: CodexReachability::AccountUnavailable,
    };

    run_operator_status(&mut terminal, &files, &mut autostart, &mut probe).unwrap();

    assert_eq!(
        terminal.transcript.join("\n"),
        "setup=configured\ncompanion_autostart=present\ncodex_task_method=supported\ncodex_quota_method=account_unavailable\nlast_successful_task_refresh=101\nlast_successful_quota_refresh=202\nlast_successful_upload=303"
    );
    for forbidden in [DEVICE_ID, SECRET, "task-title", r"C:\safe-data"] {
        assert!(!terminal.transcript.join("\n").contains(forbidden));
    }

    terminal.transcript.clear();
    files
        .files
        .insert(AppFile::Settings, b"SECRET_CORRUPT_SETTINGS".to_vec());
    files
        .files
        .insert(AppFile::RuntimeStatus, b"SECRET_CORRUPT_RUNTIME".to_vec());
    autostart.status = AutostartStatus::Unavailable;
    probe.task = CodexReachability::Incompatible;
    probe.quota = CodexReachability::Unavailable;
    run_operator_status(&mut terminal, &files, &mut autostart, &mut probe).unwrap();
    assert_eq!(
        terminal.transcript.join("\n"),
        "setup=not_configured\ncompanion_autostart=unavailable\ncodex_task_method=incompatible\ncodex_quota_method=unavailable\nlast_successful_task_refresh=unavailable\nlast_successful_quota_refresh=unavailable\nlast_successful_upload=unavailable"
    );
    assert!(!terminal.transcript.join("\n").contains("SECRET_CORRUPT"));
}

#[test]
fn status_reports_setup_first_upload_before_the_companion_writes_runtime_status() {
    // Catches hiding the confirmed upload timestamp during the first scheduler interval.
    let events = Events::new();
    let mut terminal = FakeTerminal::new(Some("n"), events.clone());
    terminal.transcript.clear();
    let mut files = MemoryFiles::new(events.clone());
    files.files.insert(
        AppFile::Settings,
        br#"{"deviceId":"private-route","pageId":3}"#.to_vec(),
    );
    files.files.insert(
        AppFile::StatusFrameState,
        serde_json::to_vec(&StatusFramePublisherState {
            failure_count: 0x8000_0000,
            last_successful_frame_sha256: Some("private-frame-hash".into()),
            last_successful_upload_epoch_seconds: Some(404),
            next_allowed_attempt_epoch_seconds: Some(464),
            ..Default::default()
        })
        .unwrap(),
    );
    let mut autostart = FakeAutostart::absent(events);
    let mut probe = FakeProbe {
        task: CodexReachability::Unavailable,
        quota: CodexReachability::Unavailable,
    };

    run_operator_status(&mut terminal, &files, &mut autostart, &mut probe).unwrap();

    assert!(
        terminal
            .transcript
            .contains(&"last_successful_upload=404".into())
    );
    assert!(
        !terminal
            .transcript
            .join("\n")
            .contains("private-frame-hash")
    );
    assert!(!terminal.transcript.join("\n").contains("private-route"));
}

#[test]
fn runtime_status_persists_only_the_three_approved_timestamps_atomically() {
    // Catches adding source payloads/identifiers or leaving a staging file beside runtime status.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime-status.json");
    let status = RuntimeStatus {
        last_successful_task_refresh_epoch_seconds: Some(11),
        last_successful_quota_refresh_epoch_seconds: Some(22),
        last_successful_upload_epoch_seconds: Some(33),
    };

    status.save_atomically(&path).unwrap();

    assert_eq!(RuntimeStatus::load(&path).unwrap(), Some(status));
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "lastSuccessfulQuotaRefreshEpochSeconds",
            "lastSuccessfulTaskRefreshEpochSeconds",
            "lastSuccessfulUploadEpochSeconds",
        ]
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn runtime_status_rejects_negative_and_out_of_calendar_range_timestamps() {
    // Catches syntactically valid timestamps becoming authoritative operator diagnostics.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime-status.json");
    for invalid in [-1, 253_402_300_800] {
        let status = RuntimeStatus {
            last_successful_task_refresh_epoch_seconds: Some(invalid),
            last_successful_quota_refresh_epoch_seconds: None,
            last_successful_upload_epoch_seconds: None,
        };
        assert!(status.save_atomically(&path).is_err(), "{invalid}");

        std::fs::write(&path, serde_json::to_vec(&status).unwrap()).unwrap();
        assert!(RuntimeStatus::load(&path).is_err(), "{invalid}");
    }
}

#[test]
fn status_treats_semantically_invalid_valid_json_as_unconfigured_and_unavailable() {
    // Catches valid JSON bypassing Settings/RuntimeStatus semantic validation in operator status.
    let events = Events::new();
    let mut terminal = FakeTerminal::new(Some("n"), events.clone());
    terminal.transcript.clear();
    let mut files = MemoryFiles::new(events.clone());
    files.files.insert(
        AppFile::Settings,
        br#"{"deviceId":"../PRIVATE_ROUTE_SENTINEL","pageId":3}"#.to_vec(),
    );
    files.files.insert(
        AppFile::RuntimeStatus,
        br#"{"lastSuccessfulTaskRefreshEpochSeconds":-7,"lastSuccessfulQuotaRefreshEpochSeconds":253402300800,"lastSuccessfulUploadEpochSeconds":null}"#.to_vec(),
    );
    files.files.insert(
        AppFile::StatusFrameState,
        br#"{"failureCount":2147483648,"lastSuccessfulFrameSha256":"PRIVATE_HASH","lastSuccessfulUploadEpochSeconds":-42,"nextAllowedAttemptEpochSeconds":18}"#.to_vec(),
    );
    let mut autostart = FakeAutostart::absent(events);
    let mut probe = FakeProbe {
        task: CodexReachability::Supported,
        quota: CodexReachability::Supported,
    };

    run_operator_status(&mut terminal, &files, &mut autostart, &mut probe).unwrap();

    assert_eq!(
        terminal.transcript.join("\n"),
        "setup=not_configured\ncompanion_autostart=absent\ncodex_task_method=supported\ncodex_quota_method=supported\nlast_successful_task_refresh=unavailable\nlast_successful_quota_refresh=unavailable\nlast_successful_upload=unavailable"
    );
    for forbidden in [
        "PRIVATE_ROUTE_SENTINEL",
        "PRIVATE_HASH",
        "-7",
        "-42",
        "253402300800",
    ] {
        assert!(!terminal.transcript.join("\n").contains(forbidden));
    }
}

struct UninstallTerminal {
    answer: Option<String>,
    transcript: Vec<String>,
}

impl SetupTerminal for UninstallTerminal {
    fn read_secret(&mut self, _prompt: &str) -> Result<Zeroizing<String>, SetupBoundaryError> {
        panic!("uninstall must not read a secret")
    }

    fn read_line(&mut self, prompt: &str) -> Result<Option<String>, SetupBoundaryError> {
        self.transcript.push(prompt.into());
        Ok(self.answer.take())
    }

    fn write_line(&mut self, line: &str) -> Result<(), SetupBoundaryError> {
        self.transcript.push(line.into());
        Ok(())
    }
}

#[test]
fn uninstall_removes_only_exact_app_files_and_deletes_credentials_only_for_single_y() {
    // Catches broad deletion, unrelated task deletion, and permissive credential confirmation.
    for (answer, expected_deleted) in [("y", true), ("Y", true), ("yes", false), ("n", false)] {
        let events = Events::new();
        let credentials = FakeCredentials::new(Some(SECRET), events.clone());
        let mut files = MemoryFiles::new(events.clone());
        for file in AppFile::uninstall_targets() {
            files.files.insert(*file, b"app data".to_vec());
        }
        let mut autostart = FakeAutostart::absent(events);
        autostart.status = AutostartStatus::Present;
        let mut terminal = UninstallTerminal {
            answer: Some(answer.into()),
            transcript: Vec::new(),
        };

        assert_eq!(
            run_uninstall(&mut terminal, &credentials, &mut files, &mut autostart,).unwrap(),
            if expected_deleted {
                UninstallOutcome::RemovedWithCredential
            } else {
                UninstallOutcome::RemovedCredentialRetained
            }
        );

        assert_eq!(files.removed, AppFile::uninstall_targets());
        assert!(files.files.is_empty());
        assert_eq!(autostart.uninstalls, 1);
        assert_eq!(
            credentials.value.lock().unwrap().is_none(),
            expected_deleted
        );
        assert_eq!(
            terminal.transcript[0],
            "同时删除 Windows 凭据管理器中的 API Key？[y/N]"
        );
        assert!(!terminal.transcript.join("\n").contains(SECRET));
    }
}

#[test]
fn operator_errors_are_fixed_and_private_data_free() {
    // Catches arbitrary dependency error text crossing the operator boundary.
    assert_eq!(
        OperatorError::Unavailable.to_string(),
        "operator command unavailable"
    );
    let text = format!("{:?}", OperatorError::Unavailable);
    for forbidden in [SECRET, DEVICE_ID, "raw response", "task title"] {
        assert!(!text.contains(forbidden));
    }
}

fn position(values: &[&'static str], target: &str) -> usize {
    values.iter().position(|value| *value == target).unwrap()
}

fn position_string(values: &[String], target: &str) -> usize {
    values.iter().position(|value| value == target).unwrap()
}

struct ReconstructedRenderer;

impl StatusFrameRenderer for ReconstructedRenderer {
    fn render(
        &mut self,
        state: &StatusBoardState,
        _now_epoch_seconds: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError> {
        assert_eq!(state.uploaded_at_epoch_seconds, None);
        Ok(b"updated-follow-up-png".to_vec())
    }
}

struct CompletionCheckingRenderer {
    expected_upload_epoch_seconds: Option<i64>,
}

impl StatusFrameRenderer for CompletionCheckingRenderer {
    fn render(
        &mut self,
        state: &StatusBoardState,
        _now_epoch_seconds: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError> {
        assert_eq!(
            state.uploaded_at_epoch_seconds,
            self.expected_upload_epoch_seconds
        );
        Ok(b"completion-checked-updated-follow-up-png".to_vec())
    }
}

#[derive(Default)]
struct ReconstructedPublisher {
    uploads: usize,
}

impl StatusFramePublisher for ReconstructedPublisher {
    fn publish_png(&mut self, _png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        self.uploads += 1;
        Ok(UploadCompletion {
            epoch_seconds: COMPLETION + 60,
        })
    }
}
