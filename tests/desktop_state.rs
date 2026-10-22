use codex_zectrix_dashboard::inkcards::PagePublisher;
use codex_zectrix_dashboard::{desktop_state::*, status_board::*, *};
use sha2::{Digest, Sha256};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const NOW: i64 = 1_786_330_123;
fn settings() -> Settings {
    Settings {
        device_id: "test-device".into(),
        page_id: 3,
    }
}
fn snapshot() -> DesktopSnapshot {
    DesktopSnapshot::new(
        &settings(),
        NOW,
        &RuntimeStatus {
            last_successful_quota_refresh_epoch_seconds: Some(NOW - 600),
            last_successful_task_refresh_epoch_seconds: Some(NOW - 60),
            ..Default::default()
        },
        StatusBoardState {
            task_state: BoardTaskState::Running,
            quota_remaining_percent: Some(72.0),
            quota_window_duration_mins: Some(10080),
            ..Default::default()
        },
    )
}
#[test]
fn stopped_worker_never_claims_live() {
    let freshness = snapshot().assess_freshness(NOW, false);
    assert_eq!(freshness.task, Freshness::LastKnown);
    assert_eq!(freshness.quota, Freshness::LastKnown);
}
#[test]
fn future_or_wrong_route_snapshot_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    snapshot().save_atomically(dir.path()).unwrap();
    assert!(
        DesktopSnapshot::load(dir.path(), &settings(), NOW)
            .unwrap()
            .is_some()
    );
    assert!(
        DesktopSnapshot::load(dir.path(), &settings(), NOW - 61)
            .unwrap()
            .is_none()
    );
    let mut other = settings();
    other.device_id = "another-device".into();
    assert!(
        DesktopSnapshot::load(dir.path(), &other, NOW)
            .unwrap()
            .is_none()
    );
    let mut old = snapshot();
    old.app_version = "old-build".into();
    old.save_atomically(dir.path()).unwrap();
    assert!(
        DesktopSnapshot::load(dir.path(), &settings(), NOW)
            .unwrap()
            .is_none()
    );
}
#[test]
fn quota_and_task_age_independently() {
    let mut s = snapshot();
    s.task_observed_at = Some(NOW - 181);
    assert_eq!(s.assess_freshness(NOW, true).task, Freshness::Stale);
    assert_eq!(s.assess_freshness(NOW, true).quota, Freshness::Live);
    s.quota_observed_at = Some(NOW - 1801);
    s.task_observed_at = Some(NOW);
    assert_eq!(s.assess_freshness(NOW, true).task, Freshness::Live);
    assert_eq!(s.assess_freshness(NOW, true).quota, Freshness::Stale);
    s.board.task_stale = true;
    assert_eq!(s.assess_freshness(NOW, true).task, Freshness::Stale);
}
#[test]
fn secondary_quota_has_independent_freshness_even_without_primary() {
    let mut s = snapshot();
    s.board.quota_secondary = Some(BoardQuotaWindow {
        remaining_percent: 30.0,
        stale: true,
        resets_at_epoch_seconds: None,
        window_duration_mins: Some(10080),
    });
    assert_eq!(s.assess_freshness(NOW, true).quota, Freshness::Live);
    assert_eq!(
        s.assess_freshness(NOW, true).secondary_quota,
        Freshness::Stale
    );
    s.board.quota_remaining_percent = None;
    assert_eq!(s.assess_freshness(NOW, true).quota, Freshness::Unavailable);
    assert_eq!(
        s.assess_freshness(NOW, true).secondary_quota,
        Freshness::Stale
    );
    s.board.quota_secondary.as_mut().unwrap().stale = false;
    assert_eq!(
        s.assess_freshness(NOW, true).secondary_quota,
        Freshness::Live
    );
    assert_eq!(
        s.assess_freshness(NOW, false).secondary_quota,
        Freshness::LastKnown
    );
}
#[test]
fn mismatched_png_is_not_a_successful_preview() {
    let dir = tempfile::tempdir().unwrap();
    let preview = DeterministicSetupRenderer.render_sample().unwrap();
    let png = preview.png();
    std::fs::write(dir.path().join("desktop-frame-3.png"), png).unwrap();
    assert!(
        load_uploaded_frame(dir.path(), 3, &"0".repeat(64))
            .unwrap()
            .is_none()
    );
    assert!(
        load_uploaded_frame(dir.path(), 3, &format!("{:x}", Sha256::digest(png)))
            .unwrap()
            .is_some()
    );
    std::fs::write(dir.path().join("desktop-frame-3.png"), vec![0; 131073]).unwrap();
    assert!(load_uploaded_frame(dir.path(), 3, &"0".repeat(64)).is_err());
    assert!(load_uploaded_frame(dir.path(), 6, &"0".repeat(64)).is_err());
}
#[test]
fn malformed_and_oversized_snapshots_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("desktop-status.json"), vec![b' '; 65537]).unwrap();
    assert!(DesktopSnapshot::load(dir.path(), &settings(), NOW).is_err());
    let mut s = snapshot();
    s.board.quota_remaining_percent = Some(102.0);
    s.save_atomically(dir.path()).unwrap();
    assert!(
        DesktopSnapshot::load(dir.path(), &settings(), NOW)
            .unwrap()
            .is_none()
    );
}
struct Network(Arc<AtomicUsize>);
impl PagePublisher for Network {
    fn publish_page(
        &mut self,
        page: u8,
        _: &[u8],
    ) -> Result<UploadCompletion, StatusFramePublishError> {
        assert_eq!(page, 3);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(UploadCompletion { epoch_seconds: NOW })
    }
}
impl StatusFramePublisher for Network {
    fn publish_png(&mut self, png: &[u8]) -> Result<UploadCompletion, StatusFramePublishError> {
        self.publish_page(3, png)
    }
}
#[test]
fn cache_failure_does_not_retry_an_acknowledged_upload() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("desktop-frame-3.png")).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut publisher = CachingPublisher::new(Network(calls.clone()), dir.path().into(), 3);
    let preview = DeterministicSetupRenderer.render_sample().unwrap();
    assert!(publisher.publish_png(preview.png()).is_ok());
    assert!(publisher.take_cache_error());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
