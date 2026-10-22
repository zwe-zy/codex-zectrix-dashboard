use std::io::{Error, ErrorKind};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

use super::{ProcessRow, ProcessTreeSnapshot};

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("无法启动 Codex app-server：{0}")]
    Start(#[source] std::io::Error),
}

pub struct OwnedProcess {
    child: Child,
    tree: OwnedProcessTree,
    armed: bool,
}

impl OwnedProcess {
    pub fn spawn_codex(program: &Path) -> Result<Self, ProcessError> {
        let mut command = Command::new(program);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn().map_err(ProcessError::Start)?;
        let root_pid = child.id().try_into().unwrap();
        let mut tree = OwnedProcessTree::new(root_pid, root_pid);
        tree.refresh();
        Ok(Self {
            child,
            tree,
            armed: true,
        })
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let _ = self.child.try_wait();
            if !self.tree.has_live_processes() {
                self.armed = false;
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn request_termination(&mut self) {
        self.tree.signal(libc::SIGTERM);
    }

    pub fn force_termination(&mut self) {
        if self.armed {
            self.tree.signal(libc::SIGKILL);
            self.armed = false;
        }
    }

    pub fn wait(&mut self) {
        let _ = self.child.wait();
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.force_termination();
        self.wait();
    }
}

struct OwnedProcessTree {
    snapshot: ProcessTreeSnapshot,
}

impl OwnedProcessTree {
    fn new(root_pid: libc::pid_t, process_group_id: libc::pid_t) -> Self {
        Self {
            snapshot: ProcessTreeSnapshot::new(root_pid, process_group_id),
        }
    }

    fn refresh(&mut self) {
        let rows = process_rows();
        self.snapshot.observe(rows.as_deref().map_err(|_| ()));
    }

    fn has_live_processes(&mut self) -> bool {
        self.refresh();
        self.snapshot.has_live_processes(process_exists)
    }

    fn signal(&mut self, signal: libc::c_int) {
        self.refresh();
        for process_group_id in self.snapshot.signal_process_group_ids() {
            signal_process_group(*process_group_id, signal);
        }
        for process_id in self.snapshot.signal_process_ids() {
            unsafe {
                libc::kill(*process_id, signal);
            }
        }
    }
}

fn process_rows() -> std::io::Result<Vec<ProcessRow>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,pgid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(Error::other(format!(
            "/bin/ps exited with {}",
            output.status
        )));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace();
            let parse_field = |field: Option<&str>| {
                field
                    .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing /bin/ps field"))?
                    .parse()
                    .map_err(|error| Error::new(ErrorKind::InvalidData, error))
            };
            let row = ProcessRow {
                pid: parse_field(fields.next())?,
                parent_pid: parse_field(fields.next())?,
                process_group_id: parse_field(fields.next())?,
            };
            if fields.next().is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "unexpected /bin/ps field",
                ));
            }
            Ok(row)
        })
        .collect()
}

fn process_exists(process_id: libc::pid_t) -> bool {
    if unsafe { libc::kill(process_id, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn signal_process_group(process_group_id: libc::pid_t, signal: libc::c_int) {
    unsafe {
        libc::kill(-process_group_id, signal);
    }
}
