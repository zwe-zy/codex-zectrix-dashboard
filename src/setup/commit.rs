use super::*;

/// A user-approved preview and route. Secrets never implement Debug or serialization.
pub struct PreparedSetup {
    settings: Settings,
    preview: SetupPreview,
    secret: Zeroizing<String>,
    previous_preview: Option<Vec<u8>>,
}
impl PreparedSetup {
    pub fn new(
        settings: Settings,
        preview: SetupPreview,
        secret: Zeroizing<String>,
        previous_preview: Option<Vec<u8>>,
    ) -> Result<Self, SetupError> {
        settings
            .validate()
            .map_err(|_| SetupError::SettingsPersistUnavailable)?;
        if secret.trim().is_empty() {
            return Err(SetupError::SecretUnavailable);
        }
        Ok(Self {
            settings,
            preview,
            secret,
            previous_preview,
        })
    }
}
#[derive(Debug)]
pub struct SetupCommitReport {
    pub outcome: SetupOutcome,
    pub background_warning: Option<SetupError>,
}

pub fn commit_prepared_setup<
    C: SetupCloud,
    S: SetupFileStore,
    A: AutostartControl,
    K: SetupClock,
>(
    draft: PreparedSetup,
    cloud: &mut C,
    credentials: &impl CredentialStore,
    files: &mut S,
    autostart: &mut A,
    clock: &K,
    control: &mut impl SetupControl,
) -> Result<SetupCommitReport, SetupError> {
    let PreparedSetup {
        settings,
        preview,
        secret: api_key,
        previous_preview,
    } = draft;
    if let Err(error) = control.begin() {
        restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
            .map_err(|_| SetupError::LocalStateUnavailable)?;
        return Err(error);
    }
    let result = (|| {
        let settings_snapshot = match files.read(AppFile::Settings) {
            Ok(value) => value,
            Err(SetupBoundaryError) => {
                restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
                    .map_err(|_| SetupError::LocalStateUnavailable)?;
                return Err(SetupError::LocalStateUnavailable);
            }
        };
        let state_snapshot = match files.read(AppFile::StatusFrameState) {
            Ok(value) => value,
            Err(SetupBoundaryError) => {
                restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
                    .map_err(|_| SetupError::LocalStateUnavailable)?;
                return Err(SetupError::LocalStateUnavailable);
            }
        };
        let credential_snapshot = match credentials.get() {
            Ok(value) => value,
            Err(_) => {
                restore_file(files, AppFile::PendingPreview, previous_preview.as_deref())
                    .map_err(|_| SetupError::LocalStateUnavailable)?;
                return Err(SetupError::CredentialPersistUnavailable);
            }
        };
        let snapshots = Snapshots {
            settings: settings_snapshot,
            state: state_snapshot,
            preview: previous_preview,
            credential: credential_snapshot,
            autostart: autostart.status(),
        };

        if settings.validate().is_err() {
            rollback(files, credentials, autostart, &snapshots, false)?;
            return Err(SetupError::SettingsPersistUnavailable);
        }
        if credentials.set(api_key.as_str()).is_err() {
            rollback(files, credentials, autostart, &snapshots, false)?;
            return Err(SetupError::CredentialPersistUnavailable);
        }
        let settings_bytes = serde_json::to_vec_pretty(&settings)
            .map_err(|_| SetupError::SettingsPersistUnavailable)?;
        if files.write(AppFile::Settings, &settings_bytes).is_err() {
            rollback(files, credentials, autostart, &snapshots, false)?;
            return Err(SetupError::SettingsPersistUnavailable);
        }

        let request_started_epoch_seconds = clock.now_epoch_seconds();
        let mut coordinator = StatusFrameCoordinator::default();
        coordinator.observe(preview.state);
        let mut exact_renderer = ExactPreviewRenderer { png: &preview.png };
        let (attempt, upload_error) = {
            let mut first_upload = FirstUploadPublisher {
                cloud,
                secret: api_key.as_str(),
                device_id: &settings.device_id,
                page_id: settings.page_id,
                clock,
                last_error: None,
            };
            let attempt = coordinator.try_publish(
                request_started_epoch_seconds,
                &mut exact_renderer,
                &mut first_upload,
            );
            (attempt, first_upload.last_error)
        };
        drop(api_key);
        if attempt != StatusFrameAttempt::Published {
            rollback(files, credentials, autostart, &snapshots, false)?;
            return Err(match upload_error {
                Some(SetupCloudError::UploadRejected | SetupCloudError::KeyRejected) => {
                    SetupError::UploadRejected
                }
                Some(
                    SetupCloudError::UploadResultUnknown | SetupCloudError::DiscoveryUnavailable,
                )
                | None => SetupError::UploadResultUnknown,
            });
        }

        let state_bytes = serde_json::to_vec(coordinator.state())
            .map_err(|_| SetupError::StatePersistFailedScreenMayHaveChanged)?;
        if files
            .write(AppFile::StatusFrameState, &state_bytes)
            .is_err()
        {
            return Err(
                if rollback(files, credentials, autostart, &snapshots, false).is_err() {
                    SetupError::StatePersistFailedScreenMayHaveChangedRollbackIncomplete
                } else {
                    SetupError::StatePersistFailedScreenMayHaveChanged
                },
            );
        }
        if snapshots.autostart == AutostartStatus::Unavailable
            || (snapshots.autostart == AutostartStatus::Absent && autostart.install().is_err())
        {
            return Ok(SetupOutcome::ConfiguredWithAutostartWarning);
        }

        Ok(SetupOutcome::Configured)
    })();
    let commit = match &result {
        Ok(SetupOutcome::Configured | SetupOutcome::ConfiguredWithAutostartWarning)
        | Err(SetupError::CompletionNoticeFailedScreenMayHaveChanged) => {
            SetupCommitState::Configured
        }
        Err(
            SetupError::RollbackUnavailable
            | SetupError::StatePersistFailedScreenMayHaveChangedRollbackIncomplete
            | SetupError::AutostartFailedScreenMayHaveChangedRollbackIncomplete,
        ) => SetupCommitState::Uncertain,
        _ => SetupCommitState::Restored,
    };
    let finish = control.finish(commit);
    match result {
        Ok(outcome) => Ok(SetupCommitReport {
            outcome,
            background_warning: finish.err(),
        }),
        Err(error) => {
            finish?;
            Err(error)
        }
    }
}
