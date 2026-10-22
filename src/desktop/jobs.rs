use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc::{self, Receiver, Sender},
};

#[derive(Default, Clone)]
pub struct DraftCancellation(Arc<AtomicU64>);
impl DraftCancellation {
    pub fn is_cancelled(&self, draft: DraftId) -> bool {
        draft.0 <= self.0.load(Ordering::Acquire)
    }
    fn cancel(&self, draft: DraftId) {
        self.0.fetch_max(draft.0, Ordering::AcqRel);
    }
}
pub trait DesktopBackend: Send {
    fn execute(
        &mut self,
        command: DesktopCommand,
        cancellation: &DraftCancellation,
    ) -> DesktopReply;
    fn clear_draft(&mut self, draft: DraftId);
}
#[derive(Debug, PartialEq, Eq)]
pub struct Busy;
#[derive(Debug, PartialEq, Eq)]
pub enum CloseDecision {
    CloseNow,
    WaitForCommit,
}
enum Request {
    Execute(JobId, DesktopCommand),
    Clear(DraftId),
}
struct Active {
    id: JobId,
    transaction: bool,
    refresh: bool,
    draft: Option<DraftId>,
}
pub struct DesktopJobs {
    sender: Sender<Request>,
    receiver: Receiver<(JobId, DesktopReply)>,
    cancellation: DraftCancellation,
    active: Option<Active>,
    sequence: u64,
    last_draft: Option<DraftId>,
}
impl DesktopJobs {
    pub fn spawn(mut backend: impl DesktopBackend + 'static) -> Self {
        let (sender, request) = mpsc::channel();
        let (reply, receiver) = mpsc::channel();
        let cancellation = DraftCancellation::default();
        let worker_cancel = cancellation.clone();
        std::thread::spawn(move || {
            while let Ok(request) = request.recv() {
                match request {
                    Request::Clear(id) => backend.clear_draft(id),
                    Request::Execute(job, command) => {
                        let refresh = matches!(command, DesktopCommand::Refresh);
                        let draft = command.draft_id();
                        let mut result = if draft.is_some_and(|id| worker_cancel.is_cancelled(id)) {
                            DesktopReply::Failed(DesktopError::Cancelled)
                        } else {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                backend.execute(command, &worker_cancel)
                            }))
                            .unwrap_or(DesktopReply::Failed(DesktopError::WorkerUnavailable))
                        };
                        if let Some(id) = draft.filter(|id| worker_cancel.is_cancelled(*id)) {
                            backend.clear_draft(id);
                            result = DesktopReply::Failed(DesktopError::Cancelled);
                        }
                        if refresh && let DesktopReply::Failed(error) = result {
                            result = DesktopReply::RefreshFailed(error);
                        }
                        if reply.send((job, result)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            sender,
            receiver,
            cancellation,
            active: None,
            sequence: 0,
            last_draft: None,
        }
    }
    pub fn is_busy(&self) -> bool {
        self.active.is_some()
    }
    pub fn is_operation_busy(&self) -> bool {
        self.active.as_ref().is_some_and(|a| !a.refresh)
    }
    pub fn submit(&mut self, command: DesktopCommand) -> Result<JobId, Busy> {
        if self.is_busy() {
            return Err(Busy);
        }
        self.sequence += 1;
        let id = JobId(self.sequence);
        let draft = command.draft_id();
        if draft.is_some() {
            self.last_draft = draft;
        }
        let active = Active {
            id,
            transaction: command.is_transaction(),
            refresh: matches!(command, DesktopCommand::Refresh),
            draft,
        };
        self.sender
            .send(Request::Execute(id, command))
            .map_err(|_| Busy)?;
        self.active = Some(active);
        Ok(id)
    }
    pub fn try_recv(&mut self) -> Option<(JobId, DesktopReply)> {
        match self.receiver.try_recv() {
            Ok(result) => {
                if self.active.as_ref().is_some_and(|a| a.id == result.0) {
                    self.active = None;
                }
                Some(result)
            }
            Err(mpsc::TryRecvError::Disconnected) => self.active.take().map(|a| {
                (
                    a.id,
                    if a.refresh {
                        DesktopReply::RefreshFailed(DesktopError::WorkerUnavailable)
                    } else {
                        DesktopReply::Failed(DesktopError::WorkerUnavailable)
                    },
                )
            }),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
    pub fn cancel_draft(&mut self, draft: DraftId) -> Result<(), Busy> {
        if self.active.as_ref().is_some_and(|a| a.transaction) {
            return Err(Busy);
        }
        self.cancellation.cancel(draft);
        self.sender.send(Request::Clear(draft)).map_err(|_| Busy)
    }
    pub fn request_close(&mut self) -> CloseDecision {
        if self.active.as_ref().is_some_and(|a| a.transaction) {
            return CloseDecision::WaitForCommit;
        }
        if let Some(id) = self
            .active
            .as_ref()
            .and_then(|a| a.draft)
            .or(self.last_draft)
        {
            let _ = self.cancel_draft(id);
        }
        CloseDecision::CloseNow
    }
}
