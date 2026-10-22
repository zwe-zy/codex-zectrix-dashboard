use codex_zectrix_dashboard::*;
use std::sync::Mutex;
use zeroize::Zeroizing;

#[derive(Default)]
struct MemoryKey(Mutex<Option<String>>);
impl CredentialStore for MemoryKey {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError> {
        Ok(self.0.lock().unwrap().clone().map(Zeroizing::new))
    }
    fn set(&self, key: &str) -> Result<(), CredentialError> {
        *self.0.lock().unwrap() = Some(key.into());
        Ok(())
    }
    fn delete(&self) -> Result<(), CredentialError> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}
struct Cloud {
    reject: bool,
    uploads: usize,
}
impl SetupCloud for Cloud {
    fn discover(&mut self, _: &str) -> Result<Vec<SetupDevice>, SetupCloudError> {
        unreachable!()
    }
    fn upload_png(
        &mut self,
        key: &str,
        device: &str,
        page: u8,
        png: &[u8],
    ) -> Result<(), SetupCloudError> {
        assert_eq!((key, device, page), ("new-key", "new-device", 3));
        assert!(png.starts_with(b"\x89PNG"));
        self.uploads += 1;
        if self.reject {
            Err(SetupCloudError::UploadRejected)
        } else {
            Ok(())
        }
    }
}
struct Startup;
impl AutostartControl for Startup {
    fn status(&mut self) -> AutostartStatus {
        AutostartStatus::Present
    }
    fn install(&mut self) -> Result<(), AutostartError> {
        Ok(())
    }
    fn uninstall(&mut self) -> Result<(), AutostartError> {
        Ok(())
    }
}
struct Clock;
impl SetupClock for Clock {
    fn now_epoch_seconds(&self) -> i64 {
        1_786_330_123
    }
}
#[derive(Default)]
struct Control {
    changed: bool,
    start_failed: bool,
    finished: Option<SetupCommitState>,
}
impl SetupControl for Control {
    fn begin(&mut self) -> Result<(), SetupError> {
        if self.changed {
            Err(SetupError::InstallationChanged)
        } else {
            Ok(())
        }
    }
    fn finish(&mut self, state: SetupCommitState) -> Result<(), SetupError> {
        self.finished = Some(state);
        if self.start_failed {
            Err(SetupError::BackgroundStartUnavailable)
        } else {
            Ok(())
        }
    }
}
fn draft() -> PreparedSetup {
    PreparedSetup::new(
        Settings {
            device_id: "new-device".into(),
            page_id: 3,
        },
        DeterministicSetupRenderer.render_sample().unwrap(),
        Zeroizing::new("new-key".into()),
        Some(b"old-preview".to_vec()),
    )
    .unwrap()
}

#[test]
fn prepared_commit_matches_console_rollback() {
    // Catches a GUI path committing the new route/key after a rejected upload.
    let dir = tempfile::tempdir().unwrap();
    let mut files = FilesystemSetupStore::new(dir.path()).unwrap();
    files.write(AppFile::Settings, b"old-settings").unwrap();
    files
        .write(AppFile::PendingPreview, b"old-preview")
        .unwrap();
    let keys = MemoryKey::default();
    keys.set("old-key").unwrap();
    let mut cloud = Cloud {
        reject: true,
        uploads: 0,
    };
    let mut control = Control::default();
    let result = commit_prepared_setup(
        draft(),
        &mut cloud,
        &keys,
        &mut files,
        &mut Startup,
        &Clock,
        &mut control,
    );
    assert!(
        matches!(result, Err(SetupError::UploadRejected)),
        "{result:?}"
    );
    assert_eq!(
        files.read(AppFile::Settings).unwrap().unwrap(),
        b"old-settings"
    );
    assert_eq!(
        files.read(AppFile::PendingPreview).unwrap().unwrap(),
        b"old-preview"
    );
    assert_eq!(keys.get().unwrap().unwrap().as_str(), "old-key");
    assert_eq!(control.finished, Some(SetupCommitState::Restored));
}

#[test]
fn configured_but_start_failed_is_reported_separately() {
    // Catches treating a saved route and acknowledged upload as a failed setup.
    let dir = tempfile::tempdir().unwrap();
    let mut files = FilesystemSetupStore::new(dir.path()).unwrap();
    let keys = MemoryKey::default();
    let mut cloud = Cloud {
        reject: false,
        uploads: 0,
    };
    let mut control = Control {
        start_failed: true,
        ..Default::default()
    };
    let report = commit_prepared_setup(
        draft(),
        &mut cloud,
        &keys,
        &mut files,
        &mut Startup,
        &Clock,
        &mut control,
    )
    .unwrap();
    assert_eq!(report.outcome, SetupOutcome::Configured);
    assert_eq!(
        report.background_warning,
        Some(SetupError::BackgroundStartUnavailable)
    );
    assert_eq!(
        Settings::load(dir.path().join("settings.json"))
            .unwrap()
            .unwrap()
            .page_id,
        3
    );
    assert_eq!(cloud.uploads, 1);
    assert_eq!(keys.get().unwrap().unwrap().as_str(), "new-key");
}

#[test]
fn cancelled_preview_keeps_previous_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = FilesystemSetupStore::new(dir.path()).unwrap();
    files
        .write(AppFile::PendingPreview, b"old-preview")
        .unwrap();
    drop(draft());
    assert_eq!(
        files.read(AppFile::PendingPreview).unwrap().unwrap(),
        b"old-preview"
    );
    assert!(!dir.path().join("settings.json").exists());
}

#[test]
fn filesystem_setup_store_can_atomically_replace_an_existing_preview() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = FilesystemSetupStore::new(dir.path()).unwrap();
    files.write(AppFile::PendingPreview, b"first").unwrap();
    files.write(AppFile::PendingPreview, b"second").unwrap();
    assert_eq!(
        files.read(AppFile::PendingPreview).unwrap().unwrap(),
        b"second"
    );
}

#[test]
fn prepared_commit_refuses_changed_installation_before_key_or_upload() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = FilesystemSetupStore::new(dir.path()).unwrap();
    let keys = MemoryKey::default();
    keys.set("old-key").unwrap();
    let mut cloud = Cloud {
        reject: false,
        uploads: 0,
    };
    let mut control = Control {
        changed: true,
        ..Default::default()
    };
    let result = commit_prepared_setup(
        draft(),
        &mut cloud,
        &keys,
        &mut files,
        &mut Startup,
        &Clock,
        &mut control,
    );
    assert!(matches!(result, Err(SetupError::InstallationChanged)));
    assert_eq!(cloud.uploads, 0);
    assert_eq!(keys.get().unwrap().unwrap().as_str(), "old-key");
    assert!(!dir.path().join("settings.json").exists());
}
