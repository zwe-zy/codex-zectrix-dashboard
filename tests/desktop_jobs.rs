use codex_zectrix_dashboard::desktop::*;
use std::{
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
struct Delayed {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    cleared: Arc<Mutex<Vec<DraftId>>>,
}
impl DesktopBackend for Delayed {
    fn execute(&mut self, command: DesktopCommand, _: &DraftCancellation) -> DesktopReply {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        match command {
            DesktopCommand::Discover { draft_id, .. } => DesktopReply::Devices {
                draft_id,
                labels: vec!["old device".into()],
            },
            _ => DesktopReply::Completed(OperationReport::updated()),
        }
    }
    fn clear_draft(&mut self, id: DraftId) {
        self.cleared.lock().unwrap().push(id);
    }
}
type JobFixture = (
    DesktopJobs,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    Arc<Mutex<Vec<DraftId>>>,
);
fn fixture() -> JobFixture {
    let (entered, wait) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let cleared = Arc::new(Mutex::new(vec![]));
    (
        DesktopJobs::spawn(Delayed {
            entered,
            release: released,
            cleared: cleared.clone(),
        }),
        wait,
        release,
        cleared,
    )
}
fn receive(jobs: &mut DesktopJobs) -> DesktopReply {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some((_, reply)) = jobs.try_recv() {
            return reply;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn late_discovery_after_cancel_is_discarded() {
    let (mut jobs, entered, release, _) = fixture();
    jobs.submit(DesktopCommand::Discover {
        draft_id: DraftId(1),
        credential: CredentialChoice::Saved,
    })
    .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    jobs.cancel_draft(DraftId(1)).unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        receive(&mut jobs),
        DesktopReply::Failed(DesktopError::Cancelled)
    ));
}
#[test]
fn cancel_bypasses_busy_discovery_and_clears_before_next_draft() {
    let (mut jobs, entered, release, cleared) = fixture();
    jobs.submit(DesktopCommand::Discover {
        draft_id: DraftId(1),
        credential: CredentialChoice::Saved,
    })
    .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(jobs.cancel_draft(DraftId(1)).is_ok());
    assert!(
        jobs.submit(DesktopCommand::Discover {
            draft_id: DraftId(2),
            credential: CredentialChoice::Saved
        })
        .is_err()
    );
    release.send(()).unwrap();
    receive(&mut jobs);
    jobs.submit(DesktopCommand::Discover {
        draft_id: DraftId(2),
        credential: CredentialChoice::Saved,
    })
    .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(cleared.lock().unwrap().contains(&DraftId(1)));
    release.send(()).unwrap();
    assert!(matches!(
        receive(&mut jobs),
        DesktopReply::Devices {
            draft_id: DraftId(2),
            ..
        }
    ));
}
#[test]
fn duplicate_action_is_busy() {
    let (mut jobs, _, release, _) = fixture();
    jobs.submit(DesktopCommand::Start).unwrap();
    assert!(jobs.submit(DesktopCommand::Start).is_err());
    release.send(()).unwrap();
    receive(&mut jobs);
    assert!(!jobs.is_busy());
}
#[test]
fn close_waits_for_commit_but_not_draft() {
    let (mut jobs, _, release, _) = fixture();
    jobs.submit(DesktopCommand::Commit {
        draft_id: DraftId(1),
    })
    .unwrap();
    assert_eq!(jobs.request_close(), CloseDecision::WaitForCommit);
    assert!(jobs.cancel_draft(DraftId(1)).is_err());
    release.send(()).unwrap();
    receive(&mut jobs);
    assert_eq!(jobs.request_close(), CloseDecision::CloseNow);
    jobs.submit(DesktopCommand::Discover {
        draft_id: DraftId(2),
        credential: CredentialChoice::Saved,
    })
    .unwrap();
    assert_eq!(jobs.request_close(), CloseDecision::CloseNow);
    release.send(()).unwrap();
}
