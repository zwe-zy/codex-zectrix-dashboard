use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use codex_zectrix_dashboard::companion::StatusBoardPngRenderer;
use codex_zectrix_dashboard::publisher::{
    StatusFrameAttempt, StatusFrameCoordinator, StatusFramePublishError, StatusFramePublisher,
    StatusFramePublisherState, StatusFrameRenderError, StatusFrameRenderer, UploadCompletion,
    ZectrixPublisher,
};
use codex_zectrix_dashboard::status_board::{
    BoardLocalTime, BoardTaskState, StatusBoardClock, StatusBoardState,
};

#[derive(Default)]
struct TextRenderer;

impl StatusFrameRenderer for TextRenderer {
    fn render(
        &mut self,
        state: &StatusBoardState,
        _now_epoch_seconds: i64,
    ) -> Result<Vec<u8>, StatusFrameRenderError> {
        serde_json::to_vec(state).map_err(|_| StatusFrameRenderError::InvalidFrame)
    }
}

#[derive(Default)]
struct FakePublisher {
    results: VecDeque<Result<UploadCompletion, StatusFramePublishError>>,
    frames: Vec<Vec<u8>>,
}

impl StatusFramePublisher for FakePublisher {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        self.frames.push(png.to_vec());
        self.results
            .pop_front()
            .unwrap_or(Ok(UploadCompletion { epoch_seconds: 0 }))
    }
}

fn board(task: BoardTaskState) -> StatusBoardState {
    StatusBoardState {
        task_state: task,
        quota_remaining_percent: Some(80.0),
        quota_resets_at_epoch_seconds: Some(8_000),
        quota_stale: false,
        task_stale: false,
        uploaded_at_epoch_seconds: None,
        ..Default::default()
    }
}

fn success(at: i64) -> Result<UploadCompletion, StatusFramePublishError> {
    Ok(UploadCompletion { epoch_seconds: at })
}

#[test]
fn theme_only_change_is_published_after_existing_throttle_and_survives_restart() {
    use codex_zectrix_dashboard::status_board::BoardTheme;
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(100), success(160), success(220)]),
        ..Default::default()
    };
    let mut state = board(BoardTaskState::Running);
    coordinator.observe(state.clone());
    assert_eq!(
        coordinator.try_publish(100, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        coordinator.try_publish(160, &mut renderer, &mut publisher),
        StatusFrameAttempt::PublishedUpdated
    );
    let mut restored = restart(&coordinator);
    state.theme = BoardTheme::Eva;
    restored.observe(state.clone());
    assert_eq!(
        restored.try_publish(161, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 220
        }
    );
    assert_eq!(
        restored.try_publish(220, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    let value: serde_json::Value =
        serde_json::from_slice(publisher.frames.last().unwrap()).unwrap();
    assert_eq!(value["theme"], "eva");
    assert!(!restored.observe(state));
}
#[test]
fn clock_rollback_preserves_the_failure_backoff() {
    let state = StatusFramePublisherState {
        failure_count: 4,
        next_allowed_attempt_epoch_seconds: Some(2900),
        ..Default::default()
    };
    let mut coordinator = StatusFrameCoordinator::new(state);
    coordinator.observe(board(BoardTaskState::Running));
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher::default();
    coordinator.try_publish(2000, &mut renderer, &mut publisher);
    assert_eq!(
        coordinator.try_publish(1900, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 2800
        }
    );
    assert!(publisher.frames.is_empty());
}

#[test]
fn first_observation_after_restart_is_new_content() {
    let mut before = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(100), success(161), success(222)]),
        ..Default::default()
    };
    before.observe(board(BoardTaskState::Running));
    assert_eq!(
        before.try_publish(100, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    let mut after = restart(&before);
    after.observe(board(BoardTaskState::Completed));
    assert_eq!(
        after.try_publish(161, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        after.state().last_successful_upload_epoch_seconds,
        Some(161)
    );
    assert_eq!(
        after.try_publish(222, &mut renderer, &mut publisher),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(
        after.state().last_successful_upload_epoch_seconds,
        Some(161)
    );
    assert_eq!(
        after.try_publish(300, &mut renderer, &mut publisher),
        StatusFrameAttempt::Idle
    );
}
#[test]
fn new_content_after_followup_obeys_http_throttle() {
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(100), success(161)]),
        ..Default::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    coordinator.try_publish(100, &mut renderer, &mut publisher);
    coordinator.try_publish(161, &mut renderer, &mut publisher);
    coordinator.observe(board(BoardTaskState::Completed));
    assert_eq!(
        coordinator.try_publish(170, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 221
        }
    );
    assert_eq!(publisher.frames.len(), 2);
}

#[test]
fn sample_upload_does_not_claim_real_content_time() {
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(100), success(161)]),
        ..Default::default()
    };
    let mut sample = board(BoardTaskState::Running);
    sample.is_sample = true;
    coordinator.observe(sample);
    coordinator.try_publish(100, &mut renderer, &mut publisher);
    assert_eq!(
        coordinator.state().last_successful_upload_epoch_seconds,
        None
    );
    assert_eq!(
        coordinator
            .state()
            .last_successful_http_upload_epoch_seconds,
        Some(100)
    );
    let mut restarted = restart(&coordinator);
    restarted.observe(board(BoardTaskState::Running));
    assert_eq!(
        restarted.try_publish(161, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(161)
    );
}
#[test]
fn timestamp_followup_changes_http_time_only() {
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(161), success(222)]),
        ..Default::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    coordinator.try_publish(161, &mut renderer, &mut publisher);
    coordinator.try_publish(222, &mut renderer, &mut publisher);
    assert_eq!(
        coordinator.state().last_successful_upload_epoch_seconds,
        Some(161)
    );
    assert_eq!(
        coordinator
            .state()
            .last_successful_http_upload_epoch_seconds,
        Some(222)
    );
    assert_eq!(
        coordinator.try_publish(400, &mut renderer, &mut publisher),
        StatusFrameAttempt::Idle
    );
}

#[test]
fn success_sends_one_deferred_updated_follow_up_then_stabilizes() {
    // Catches an immediate double upload and the self-perpetuating UPDATED loop.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(101), success(162)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));

    assert_eq!(
        coordinator.try_publish(100, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(publisher.frames.len(), 1);
    assert_eq!(
        coordinator.state().last_successful_upload_epoch_seconds,
        Some(101)
    );
    assert_eq!(
        coordinator.try_publish(160, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 161
        }
    );
    assert_eq!(
        coordinator.try_publish(161, &mut renderer, &mut publisher),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(publisher.frames.len(), 2);
    let follow_up: serde_json::Value = serde_json::from_slice(&publisher.frames[1]).unwrap();
    assert_eq!(follow_up["uploaded_at_epoch_seconds"], 101);

    assert_eq!(
        coordinator.try_publish(222, &mut renderer, &mut publisher),
        StatusFrameAttempt::Idle
    );
    assert_eq!(
        coordinator.try_publish(10_000, &mut renderer, &mut publisher),
        StatusFrameAttempt::Idle
    );
    assert_eq!(publisher.frames.len(), 2);
    // A pure timestamp follow-up represents 101 on screen. Its own completion must not move the
    // display timestamp to 162 and start an endless chain.
    assert_eq!(
        coordinator.state().last_successful_upload_epoch_seconds,
        Some(101)
    );
}

#[test]
fn serialized_stable_state_restarts_without_any_duplicate_upload() {
    // Catches losing the relationship between the committed PNG hash and displayed UPDATED time.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(1), success(62)]),
        ..FakePublisher::default()
    };
    let visible = board(BoardTaskState::Completed);
    coordinator.observe(visible.clone());
    assert_eq!(
        coordinator.try_publish(0, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        coordinator.try_publish(61, &mut renderer, &mut publisher),
        StatusFrameAttempt::PublishedUpdated
    );

    let mut restarted = restart(&coordinator);
    let mut after_restart = FakePublisher::default();
    restarted.observe(visible);
    assert_eq!(
        restarted.try_publish(10_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Idle
    );
    assert!(after_restart.frames.is_empty());
}

#[test]
fn restart_between_source_upload_and_updated_follow_up_sends_exactly_one_remaining_frame() {
    // Catches reconstructing an outstanding follow-up as a fresh source upload.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut before_restart = FakePublisher {
        results: VecDeque::from([success(101)]),
        ..FakePublisher::default()
    };
    let visible = board(BoardTaskState::Running);
    coordinator.observe(visible.clone());
    assert_eq!(
        coordinator.try_publish(100, &mut renderer, &mut before_restart),
        StatusFrameAttempt::Published
    );

    let mut restarted = restart(&coordinator);
    let mut after_restart = FakePublisher {
        results: VecDeque::from([success(162)]),
        ..FakePublisher::default()
    };
    restarted.observe(visible);
    assert_eq!(
        restarted.try_publish(160, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 161
        }
    );
    assert_eq!(
        restarted.try_publish(161, &mut renderer, &mut after_restart),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(
        restarted.try_publish(222, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Idle
    );
    assert_eq!(after_restart.frames.len(), 1);
    let frame: serde_json::Value = serde_json::from_slice(&after_restart.frames[0]).unwrap();
    assert_eq!(frame["uploaded_at_epoch_seconds"], 101);
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(101)
    );
}

#[test]
fn legacy_state_without_fingerprint_refreshes_content_without_rewriting_on_load() {
    // A legacy hash cannot establish whether the source changed while the program was stopped.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("status-frame-state.json");
    let legacy = br#"{"failureCount":0,"lastSuccessfulFrameSha256":"0000000000000000000000000000000000000000000000000000000000000000","lastSuccessfulUploadEpochSeconds":101,"nextAllowedAttemptEpochSeconds":161}"#;
    std::fs::write(&path, legacy).unwrap();
    let loaded = StatusFramePublisherState::load(&path).unwrap().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), legacy);

    let mut coordinator = StatusFrameCoordinator::new(loaded);
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(222), success(283)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    assert_eq!(
        coordinator.try_publish(160, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 161
        }
    );
    assert_eq!(std::fs::read(&path).unwrap(), legacy);
    coordinator.state().save_atomically(&path).unwrap();
    let migrated_bytes = std::fs::read(&path).unwrap();
    assert_ne!(migrated_bytes, legacy);
    assert_eq!(
        StatusFramePublisherState::load(&path)
            .unwrap()
            .unwrap()
            .failure_count,
        0x8000_0000
    );
    assert_eq!(
        coordinator.try_publish(161, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(publisher.frames.len(), 1);
    let updated: serde_json::Value = serde_json::from_slice(&publisher.frames[0]).unwrap();
    assert_eq!(updated["uploaded_at_epoch_seconds"], 101);
    assert_eq!(
        coordinator.state().last_successful_upload_epoch_seconds,
        Some(222)
    );
    assert_eq!(
        coordinator.try_publish(282, &mut renderer, &mut publisher),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(
        coordinator.try_publish(10_000, &mut renderer, &mut publisher),
        StatusFrameAttempt::Idle
    );
    assert_eq!(std::fs::read(&path).unwrap(), migrated_bytes);
}

#[test]
fn transiently_failed_updated_follow_up_restarts_as_one_pure_follow_up() {
    // Catches reconstructing a retried UPDATED-only frame as a source-changing upload.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let visible = board(BoardTaskState::Running);
    let mut before_restart = FakePublisher {
        results: VecDeque::from([
            success(101),
            Err(StatusFramePublishError::TransientUnavailable),
        ]),
        ..FakePublisher::default()
    };
    coordinator.observe(visible.clone());
    assert_eq!(
        coordinator.try_publish(100, &mut renderer, &mut before_restart),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        coordinator.try_publish(161, &mut renderer, &mut before_restart),
        StatusFrameAttempt::TransientFailure {
            retry_at_epoch_seconds: 221
        }
    );

    let mut restarted = restart(&coordinator);
    let mut after_restart = FakePublisher {
        results: VecDeque::from([success(222)]),
        ..FakePublisher::default()
    };
    restarted.observe(visible);
    assert_eq!(
        restarted.try_publish(220, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 221
        }
    );
    assert_eq!(
        restarted.try_publish(221, &mut renderer, &mut after_restart),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(after_restart.frames.len(), 1);
    let updated: serde_json::Value = serde_json::from_slice(&after_restart.frames[0]).unwrap();
    assert_eq!(updated["uploaded_at_epoch_seconds"], 101);
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(101)
    );
    assert_eq!(
        restarted.try_publish(10_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Idle
    );
}

#[test]
fn auth_rejected_updated_follow_up_restarts_blocked_then_finishes_as_one_pure_follow_up() {
    // Catches credentials_updated erasing the durable UPDATED-only phase after restart.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let visible = board(BoardTaskState::Running);
    let mut before_restart = FakePublisher {
        results: VecDeque::from([
            success(101),
            Err(StatusFramePublishError::CredentialsRejected),
        ]),
        ..FakePublisher::default()
    };
    coordinator.observe(visible.clone());
    assert_eq!(
        coordinator.try_publish(100, &mut renderer, &mut before_restart),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        coordinator.try_publish(161, &mut renderer, &mut before_restart),
        StatusFrameAttempt::CredentialsRejected
    );

    let mut restarted = restart(&coordinator);
    let mut after_restart = FakePublisher {
        results: VecDeque::from([success(222)]),
        ..FakePublisher::default()
    };
    restarted.observe(visible);
    assert_eq!(
        restarted.try_publish(10_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::CredentialsRejected
    );
    assert!(after_restart.frames.is_empty());

    restarted.credentials_updated();
    assert_eq!(
        restarted.try_publish(10_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(after_restart.frames.len(), 1);
    let updated: serde_json::Value = serde_json::from_slice(&after_restart.frames[0]).unwrap();
    assert_eq!(updated["uploaded_at_epoch_seconds"], 101);
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(101)
    );
    assert_eq!(
        restarted.try_publish(20_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Idle
    );
}

#[test]
fn transiently_failed_source_change_restarts_as_source_change_then_gets_one_follow_up() {
    // Catches a durable phase encoding that mistakes ordinary source retries for UPDATED-only work.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let visible = board(BoardTaskState::Completed);
    let mut before_restart = FakePublisher {
        results: VecDeque::from([Err(StatusFramePublishError::TransientUnavailable)]),
        ..FakePublisher::default()
    };
    coordinator.observe(visible.clone());
    assert_eq!(
        coordinator.try_publish(100, &mut renderer, &mut before_restart),
        StatusFrameAttempt::TransientFailure {
            retry_at_epoch_seconds: 160
        }
    );

    let mut restarted = restart(&coordinator);
    let mut after_restart = FakePublisher {
        results: VecDeque::from([success(161), success(222)]),
        ..FakePublisher::default()
    };
    restarted.observe(visible);
    assert_eq!(
        restarted.try_publish(160, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(161)
    );
    assert_eq!(
        restarted.try_publish(221, &mut renderer, &mut after_restart),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(
        restarted.try_publish(1_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Idle
    );
    assert_eq!(after_restart.frames.len(), 2);
    let follow_up: serde_json::Value = serde_json::from_slice(&after_restart.frames[1]).unwrap();
    assert_eq!(follow_up["uploaded_at_epoch_seconds"], 161);
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(161)
    );
}

#[test]
fn source_change_coalesced_with_follow_up_gets_one_own_updated_frame_across_restart() {
    // Catches dropping a new source that arrives after restart reconstructs the old follow-up.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(1)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    assert_eq!(
        coordinator.try_publish(0, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );

    let mut reconstructed = restart(&coordinator);
    reconstructed.observe(board(BoardTaskState::Running));
    reconstructed.observe(board(BoardTaskState::Completed));
    let mut coalesced_publisher = FakePublisher {
        results: VecDeque::from([success(62)]),
        ..FakePublisher::default()
    };
    assert_eq!(
        reconstructed.try_publish(61, &mut renderer, &mut coalesced_publisher),
        StatusFrameAttempt::Published
    );
    let coalesced: serde_json::Value =
        serde_json::from_slice(&coalesced_publisher.frames[0]).unwrap();
    assert_eq!(coalesced["task_state"], "Completed");
    assert_eq!(coalesced["uploaded_at_epoch_seconds"], 1);

    let mut restarted = restart(&reconstructed);
    let mut after_restart = FakePublisher {
        results: VecDeque::from([success(123)]),
        ..FakePublisher::default()
    };
    restarted.observe(board(BoardTaskState::Completed));
    assert_eq!(
        restarted.try_publish(121, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: 122
        }
    );
    assert_eq!(
        restarted.try_publish(122, &mut renderer, &mut after_restart),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(
        restarted.try_publish(1_000, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Idle
    );
    assert_eq!(after_restart.frames.len(), 1);
    let updated: serde_json::Value = serde_json::from_slice(&after_restart.frames[0]).unwrap();
    assert_eq!(updated["task_state"], "Completed");
    assert_eq!(updated["uploaded_at_epoch_seconds"], 62);
    assert_eq!(
        restarted.state().last_successful_upload_epoch_seconds,
        Some(62)
    );
}

#[test]
fn each_visible_source_field_creates_one_changed_pending_frame() {
    // Catches omission of any privacy-safe visible field from change detection.
    let base = board(BoardTaskState::Completed);
    let mutations = [
        board(BoardTaskState::Failed),
        StatusBoardState {
            quota_remaining_percent: Some(79.0),
            ..base.clone()
        },
        StatusBoardState {
            quota_resets_at_epoch_seconds: Some(8_001),
            ..base.clone()
        },
        StatusBoardState {
            quota_stale: true,
            ..base.clone()
        },
        StatusBoardState {
            task_stale: true,
            ..base.clone()
        },
    ];

    for mutation in mutations {
        let mut coordinator = StatusFrameCoordinator::default();
        let mut renderer = TextRenderer;
        let mut publisher = FakePublisher {
            results: VecDeque::from([success(1), success(62), success(200)]),
            ..FakePublisher::default()
        };
        coordinator.observe(base.clone());
        assert_eq!(
            coordinator.try_publish(0, &mut renderer, &mut publisher),
            StatusFrameAttempt::Published
        );
        assert_eq!(
            coordinator.try_publish(61, &mut renderer, &mut publisher),
            StatusFrameAttempt::PublishedUpdated
        );
        coordinator.observe(mutation);
        assert_eq!(
            coordinator.try_publish(200, &mut renderer, &mut publisher),
            StatusFrameAttempt::Published
        );
        assert_eq!(publisher.frames.len(), 3);
    }
}

#[test]
fn transient_failures_use_exact_backoff_and_newest_state_replaces_pending() {
    // Catches wrong retry steps and uploading an obsolete intermediate state.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let transient = Err(StatusFramePublishError::TransientUnavailable);
    let mut publisher = FakePublisher {
        results: VecDeque::from([
            transient,
            transient,
            transient,
            transient,
            transient,
            success(2_281),
        ]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));

    for (attempt_at, next_at) in [(0, 60), (60, 180), (180, 480), (480, 1_380), (1_380, 2_280)] {
        assert_eq!(
            coordinator.try_publish(attempt_at, &mut renderer, &mut publisher),
            StatusFrameAttempt::TransientFailure {
                retry_at_epoch_seconds: next_at
            }
        );
        assert_eq!(
            coordinator.try_publish(next_at - 1, &mut renderer, &mut publisher),
            StatusFrameAttempt::Deferred {
                until_epoch_seconds: next_at
            }
        );
        if attempt_at == 60 {
            coordinator.observe(board(BoardTaskState::Completed));
        }
    }
    assert_eq!(
        coordinator.try_publish(2_280, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    let newest: serde_json::Value =
        serde_json::from_slice(publisher.frames.last().unwrap()).unwrap();
    assert_eq!(newest["task_state"], "Completed");
    assert_eq!(coordinator.state().consecutive_failure_count(), 0);
    assert_eq!(
        coordinator.state().next_allowed_attempt_epoch_seconds,
        Some(2_341)
    );
}

#[test]
fn failed_upload_never_commits_hash_or_updated_time() {
    // Catches treating an attempted frame as successfully visible.
    let initial = StatusFramePublisherState {
        last_successful_frame_sha256: Some("previous".into()),
        last_successful_upload_epoch_seconds: Some(50),
        failure_count: 0,
        next_allowed_attempt_epoch_seconds: None,
        ..Default::default()
    };
    let mut coordinator = StatusFrameCoordinator::new(initial);
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([Err(StatusFramePublishError::TransientUnavailable)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Failed));
    coordinator.try_publish(100, &mut renderer, &mut publisher);

    assert_eq!(
        coordinator.state().last_successful_frame_sha256.as_deref(),
        Some("previous")
    );
    assert_eq!(
        coordinator.state().last_successful_upload_epoch_seconds,
        Some(50)
    );
}

#[test]
fn authentication_rejection_stops_automatic_attempts_until_explicit_reset() {
    // Catches retrying a rejected credential forever.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([
            Err(StatusFramePublishError::CredentialsRejected),
            success(500),
        ]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    assert_eq!(
        coordinator.try_publish(0, &mut renderer, &mut publisher),
        StatusFrameAttempt::CredentialsRejected
    );
    assert_eq!(
        coordinator.try_publish(10_000, &mut renderer, &mut publisher),
        StatusFrameAttempt::CredentialsRejected
    );
    assert_eq!(publisher.frames.len(), 1);

    coordinator.credentials_updated();
    assert_eq!(
        coordinator.try_publish(10_000, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(publisher.frames.len(), 2);
}

#[test]
fn authentication_rejection_survives_serialization_until_credentials_are_explicitly_updated() {
    // Catches a process restart silently clearing the setup-required authentication block.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut rejecting = FakePublisher {
        results: VecDeque::from([Err(StatusFramePublishError::CredentialsRejected)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    assert_eq!(
        coordinator.try_publish(0, &mut renderer, &mut rejecting),
        StatusFrameAttempt::CredentialsRejected
    );
    assert_eq!(coordinator.state().failure_count, u32::MAX);
    assert_eq!(
        coordinator.state().next_allowed_attempt_epoch_seconds,
        Some(i64::MAX)
    );

    let mut restarted = restart(&coordinator);
    let mut after_restart = FakePublisher {
        results: VecDeque::from([success(500)]),
        ..FakePublisher::default()
    };
    restarted.observe(board(BoardTaskState::Completed));
    assert_eq!(
        restarted.try_publish(400, &mut renderer, &mut after_restart),
        StatusFrameAttempt::CredentialsRejected
    );
    assert!(after_restart.frames.is_empty());

    restarted.credentials_updated();
    assert_eq!(
        restarted.try_publish(400, &mut renderer, &mut after_restart),
        StatusFrameAttempt::Published
    );
    assert_eq!(after_restart.frames.len(), 1);
}

#[test]
fn saturated_success_deadline_reconstructs_follow_up_instead_of_authentication_rejection() {
    // Catches colliding the auth sentinel with a legitimate saturated follow-up deadline.
    let mut coordinator = StatusFrameCoordinator::default();
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([success(i64::MAX)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    assert_eq!(
        coordinator.try_publish(i64::MAX, &mut renderer, &mut publisher),
        StatusFrameAttempt::Published
    );

    let mut restarted = restart(&coordinator);
    let mut follow_up = FakePublisher {
        results: VecDeque::from([success(i64::MAX)]),
        ..FakePublisher::default()
    };
    restarted.observe(board(BoardTaskState::Running));
    assert_eq!(
        restarted.try_publish(i64::MAX, &mut renderer, &mut follow_up),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(follow_up.frames.len(), 1);
}

#[test]
fn retry_arithmetic_handles_equality_backward_time_and_i64_saturation() {
    // Catches overflow and treating equality as still deferred.
    let state = StatusFramePublisherState {
        failure_count: 1,
        next_allowed_attempt_epoch_seconds: Some(i64::MAX),
        ..StatusFramePublisherState::default()
    };
    let mut coordinator = StatusFrameCoordinator::new(state);
    let mut renderer = TextRenderer;
    let mut publisher = FakePublisher {
        results: VecDeque::from([Err(StatusFramePublishError::TransientUnavailable)]),
        ..FakePublisher::default()
    };
    coordinator.observe(board(BoardTaskState::Running));
    assert_eq!(
        coordinator.try_publish(i64::MIN, &mut renderer, &mut publisher),
        StatusFrameAttempt::Deferred {
            until_epoch_seconds: i64::MAX
        }
    );
    assert_eq!(
        coordinator.try_publish(i64::MAX, &mut renderer, &mut publisher),
        StatusFrameAttempt::TransientFailure {
            retry_at_epoch_seconds: i64::MAX
        }
    );
}

#[test]
fn reserved_phase_and_auth_values_do_not_collide_with_saturated_retries() {
    // Catches high retry counts being mistaken for auth, or follow-up retry phase for source phase.
    const MAX_DECODED_FAILURES: u32 = 0x7fff_fffe;
    let mut renderer = TextRenderer;
    let mut seed = StatusFrameCoordinator::default();
    seed.observe(board(BoardTaskState::Running));
    seed.try_publish(
        100,
        &mut renderer,
        &mut FakePublisher {
            results: VecDeque::from([success(101)]),
            ..Default::default()
        },
    );
    let known_content = seed.state().clone();

    let source_retry = StatusFramePublisherState {
        failure_count: MAX_DECODED_FAILURES,
        next_allowed_attempt_epoch_seconds: Some(i64::MAX),
        ..StatusFramePublisherState::default()
    };
    let mut source_retry = StatusFrameCoordinator::new(source_retry);
    let mut source_publisher = FakePublisher {
        results: VecDeque::from([
            Err(StatusFramePublishError::TransientUnavailable),
            success(i64::MAX),
        ]),
        ..FakePublisher::default()
    };
    source_retry.observe(board(BoardTaskState::Running));
    assert_eq!(
        source_retry.state().consecutive_failure_count(),
        MAX_DECODED_FAILURES
    );
    assert_eq!(
        source_retry.try_publish(i64::MAX, &mut renderer, &mut source_publisher),
        StatusFrameAttempt::TransientFailure {
            retry_at_epoch_seconds: i64::MAX
        }
    );
    assert_eq!(
        source_retry.state().consecutive_failure_count(),
        MAX_DECODED_FAILURES
    );
    let mut source_retry = restart(&source_retry);
    source_retry.observe(board(BoardTaskState::Running));
    assert_eq!(
        source_retry.try_publish(i64::MAX, &mut renderer, &mut source_publisher),
        StatusFrameAttempt::Published
    );
    assert_eq!(
        source_retry.state().last_successful_upload_epoch_seconds,
        Some(i64::MAX)
    );

    let follow_up_retry = StatusFramePublisherState {
        failure_count: 0xffff_fffe,
        last_successful_upload_epoch_seconds: Some(101),
        next_allowed_attempt_epoch_seconds: Some(i64::MAX),
        ..known_content.clone()
    };
    let mut follow_up_retry = StatusFrameCoordinator::new(follow_up_retry);
    let mut follow_up_publisher = FakePublisher {
        results: VecDeque::from([
            Err(StatusFramePublishError::TransientUnavailable),
            success(i64::MAX),
        ]),
        ..FakePublisher::default()
    };
    follow_up_retry.observe(board(BoardTaskState::Running));
    assert_eq!(
        follow_up_retry.state().consecutive_failure_count(),
        MAX_DECODED_FAILURES
    );
    assert_eq!(
        follow_up_retry.try_publish(i64::MAX, &mut renderer, &mut follow_up_publisher),
        StatusFrameAttempt::TransientFailure {
            retry_at_epoch_seconds: i64::MAX
        }
    );
    assert_eq!(
        follow_up_retry.state().consecutive_failure_count(),
        MAX_DECODED_FAILURES
    );
    let mut follow_up_retry = restart(&follow_up_retry);
    follow_up_retry.observe(board(BoardTaskState::Running));
    assert_eq!(
        follow_up_retry.try_publish(i64::MAX, &mut renderer, &mut follow_up_publisher),
        StatusFrameAttempt::PublishedUpdated
    );
    assert_eq!(
        follow_up_retry.state().last_successful_upload_epoch_seconds,
        Some(101)
    );

    for (encoded_auth, expected_after_reset) in [
        (u32::MAX, StatusFrameAttempt::Published),
        (0x7fff_ffff, StatusFrameAttempt::PublishedUpdated),
    ] {
        let state = StatusFramePublisherState {
            failure_count: encoded_auth,
            last_successful_upload_epoch_seconds: Some(101),
            next_allowed_attempt_epoch_seconds: Some(i64::MAX),
            ..known_content.clone()
        };
        let mut coordinator = StatusFrameCoordinator::new(state);
        let mut publisher = FakePublisher {
            results: VecDeque::from([success(222)]),
            ..FakePublisher::default()
        };
        coordinator.observe(board(BoardTaskState::Running));
        assert_eq!(
            coordinator.try_publish(i64::MAX, &mut renderer, &mut publisher),
            StatusFrameAttempt::CredentialsRejected
        );
        assert!(publisher.frames.is_empty());
        coordinator.credentials_updated();
        assert_eq!(
            coordinator.try_publish(i64::MAX, &mut renderer, &mut publisher),
            expected_after_reset
        );
    }
}

#[test]
fn publisher_state_serializes_only_approved_nonsecret_metadata() {
    // Catches persistence of pending source state, secrets, response bodies, or PNG bytes.
    let value = serde_json::to_value(StatusFramePublisherState::default()).unwrap();
    let keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec![
            "contentSource",
            "failureCount",
            "lastSuccessfulContentFingerprint",
            "lastSuccessfulFrameSha256",
            "lastSuccessfulHttpUploadEpochSeconds",
            "lastSuccessfulUploadEpochSeconds",
            "nextAllowedAttemptEpochSeconds",
            "schemaVersion"
        ]
    );
}

struct FixedClock;

impl StatusBoardClock for FixedClock {
    fn now_epoch_seconds(&self) -> i64 {
        1_000
    }

    fn local_time(&self, epoch_seconds: i64) -> Option<BoardLocalTime> {
        let minute = u8::try_from(epoch_seconds.rem_euclid(3_600) / 60).ok()?;
        BoardLocalTime::new(2026, 8, 31, 10, minute).ok()
    }
}

#[test]
fn production_renderer_boundary_emits_the_task5_png_from_only_status_board_state() {
    // Catches routing the new companion through the legacy title/radar dashboard.
    let mut renderer = StatusBoardPngRenderer::new(FixedClock);
    let png = renderer
        .render(&board(BoardTaskState::Running), 1_000)
        .unwrap();
    let image = image::load_from_memory(&png).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel.0[0], 0 | 255)));
}

#[test]
fn local_zectrix_endpoint_classifies_auth_and_transient_failures_and_uses_exact_multipart_route() {
    // Catches collapsing 403 into a retry and changing the cloud upload boundary.
    let (base_url, requests) =
        local_zectrix_responses(["403 Forbidden", "500 Internal Server Error", "200 OK"]);
    let mut transport =
        ZectrixPublisher::new("LOCAL_TEST_KEY", base_url, "LOCAL_DEVICE", 4).unwrap();
    let mut renderer = StatusBoardPngRenderer::new(FixedClock);
    let png = renderer
        .render(&board(BoardTaskState::Running), 1_000)
        .unwrap();

    assert_eq!(
        transport.publish_png(&png),
        Err(StatusFramePublishError::CredentialsRejected)
    );
    assert_eq!(
        transport.publish_png(&png),
        Err(StatusFramePublishError::TransientUnavailable)
    );
    assert!(transport.publish_png(&png).is_ok());

    let requests = requests.recv().unwrap();
    assert_eq!(requests.len(), 3);
    for (head, body) in requests {
        assert!(head.starts_with("post /open/v1/devices/local_device/display/image http/1.1"));
        assert!(head.contains("x-api-key: local_test_key"));
        assert!(head.contains("content-type: multipart/form-data; boundary="));
        assert!(contains(&body, b"name=\"images\""));
        assert!(contains(&body, b"name=\"dither\""));
        assert!(contains(&body, b"\r\n\r\nfalse\r\n"));
        assert!(contains(&body, b"name=\"pageId\""));
        assert!(contains(&body, b"\r\n\r\n4\r\n"));
        assert!(!contains(&body, b"LOCAL_TEST_KEY"));
    }
}

#[test]
fn invalid_local_frame_is_rejected_before_opening_a_connection() {
    // Catches sending malformed local bytes to any endpoint.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut transport = ZectrixPublisher::new(
        "key",
        format!("http://{}", listener.local_addr().unwrap()),
        "device",
        3,
    )
    .unwrap();
    assert_eq!(
        transport.publish_png(b"not a PNG"),
        Err(StatusFramePublishError::InvalidFrameConfiguration)
    );
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn five_inkcards_use_five_explicit_persistent_page_ids() {
    use codex_zectrix_dashboard::inkcards::PagePublisher;
    let (base, requests) = local_zectrix_responses(["200 OK"; 5]);
    let mut transport = ZectrixPublisher::new("LOCAL_TEST_KEY", base, "LOCAL_DEVICE", 3).unwrap();
    let png = StatusBoardPngRenderer::new(FixedClock)
        .render(&board(BoardTaskState::Running), 1_000)
        .unwrap();
    for page in 1..=5 {
        transport.publish_page(page, &png).unwrap();
    }
    for ((head, body), page) in requests.recv().unwrap().into_iter().zip(1..=5) {
        assert!(head.starts_with("post /open/v1/devices/local_device/display/image http/1.1"));
        assert!(contains(
            &body,
            format!("name=\"pageId\"\r\n\r\n{page}\r\n").as_bytes()
        ));
        assert!(contains(&body, &png));
    }
    assert_eq!(
        transport.publish_page(6, &png),
        Err(StatusFramePublishError::InvalidFrameConfiguration)
    );
}

#[test]
fn invalid_local_routing_configuration_is_rejected_without_echoing_values() {
    // Catches treating a malformed URL/device/page as a retryable network outage.
    for result in [
        ZectrixPublisher::new("SECRET", "not a URL", "device", 3),
        ZectrixPublisher::new("SECRET", "https://example.invalid", "", 3),
        ZectrixPublisher::new("SECRET", "https://example.invalid", "device", 0),
    ] {
        let error = result.err().expect("invalid local configuration accepted");
        let diagnostic = format!("{error:?} {error}");
        assert!(diagnostic.contains("InvalidConfiguration"));
        assert!(!diagnostic.contains("SECRET"));
        assert!(!diagnostic.contains("example.invalid"));
    }
}

#[test]
fn permanent_http_routing_failures_are_not_classified_as_transient() {
    // Catches retrying invalid device/page routes forever on 400, 404, or 422.
    let (base_url, _requests) = local_zectrix_responses([
        "400 Bad Request",
        "404 Not Found",
        "422 Unprocessable Entity",
    ]);
    let mut transport = ZectrixPublisher::new("key", base_url, "device", 3).unwrap();
    let mut renderer = StatusBoardPngRenderer::new(FixedClock);
    let png = renderer.render(&board(BoardTaskState::Running), 0).unwrap();
    for _ in 0..3 {
        assert_eq!(
            transport.publish_png(&png),
            Err(StatusFramePublishError::InvalidFrameConfiguration)
        );
    }
}

#[test]
fn publisher_state_replacement_is_atomic_and_leaves_no_staging_file() {
    // Catches truncating the only durable coordinator state during replacement.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("status-frame-state.json");
    let first = StatusFramePublisherState::default();
    first.save_atomically(&path).unwrap();
    let second = StatusFramePublisherState {
        failure_count: 3,
        last_successful_frame_sha256: Some("a".repeat(64)),
        last_successful_upload_epoch_seconds: Some(123),
        next_allowed_attempt_epoch_seconds: Some(423),
        ..Default::default()
    };
    second.save_atomically(&path).unwrap();

    assert_eq!(
        StatusFramePublisherState::load(&path).unwrap(),
        Some(second)
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

fn restart(coordinator: &StatusFrameCoordinator) -> StatusFrameCoordinator {
    let serialized = serde_json::to_vec(coordinator.state()).unwrap();
    let state = serde_json::from_slice(&serialized).unwrap();
    StatusFrameCoordinator::new(state)
}

type RecordedHttpRequest = (String, Vec<u8>);

fn local_zectrix_responses<const N: usize>(
    statuses: [&'static str; N],
) -> (String, mpsc::Receiver<Vec<RecordedHttpRequest>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut requests = Vec::new();
        for status in statuses {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            let body = if status == "200 OK" {
                r#"{"code":0}"#
            } else {
                r#"{"code":1}"#
            };
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
        sender.send(requests).unwrap();
    });
    (format!("http://{address}"), receiver)
}

fn read_http_request(stream: &mut impl Read) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4_096];
    let header_end = loop {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec())
        .unwrap()
        .to_ascii_lowercase();
    let content_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map(|value| value.trim().parse::<usize>().unwrap())
        .unwrap_or(0);
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0);
        bytes.extend_from_slice(&chunk[..read]);
    }
    (
        head,
        bytes[header_end..header_end + content_length].to_vec(),
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
