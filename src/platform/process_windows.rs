use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessIdOfThread, OpenThread, ResumeThread,
    THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
};

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("无法创建 Codex app-server Job Object：{0}")]
    CreateJob(#[source] std::io::Error),
    #[error("无法配置 Codex app-server Job Object：{0}")]
    ConfigureJob(#[source] std::io::Error),
    #[error("无法启动 Codex app-server：{0}")]
    Start(#[source] std::io::Error),
    #[error("无法将 Codex app-server 分配到 Job Object：{0}")]
    AssignJob(#[source] std::io::Error),
    #[error("无法定位 Codex app-server 的挂起主线程：{0}")]
    PrimaryThread(#[source] std::io::Error),
    #[error("无法恢复 Codex app-server 的挂起主线程：{0}")]
    ResumeThread(#[source] std::io::Error),
}

pub struct OwnedProcess {
    child: Child,
    job: Option<OwnedHandle>,
}

impl OwnedProcess {
    pub fn spawn_codex(program: &Path) -> Result<Self, ProcessError> {
        let job = create_kill_on_close_job()?;
        let mut command = Command::new(program);
        command
            .args(["app-server", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn().map_err(ProcessError::Start)?;
        delay_pre_assignment_for_test(program);
        // Both borrowed raw handles remain valid for the duration of this call. Ownership stays
        // with `job` and `child`; AssignProcessToJobObject does not consume either handle.
        let assigned = unsafe {
            AssignProcessToJobObject(job.as_raw_handle().cast(), child.as_raw_handle().cast())
        };
        if assigned == 0 {
            let error = std::io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProcessError::AssignJob(error));
        }
        let primary_thread = match suspended_primary_thread(child.id()) {
            Ok(thread) => thread,
            Err(error) => {
                cleanup_failed_child(&mut child, job);
                return Err(ProcessError::PrimaryThread(error));
            }
        };
        // The verified handle belongs to the sole thread in this still-suspended process. A
        // previous suspend count of one proves this call performs the one matching resume.
        let previous_suspend_count = unsafe { ResumeThread(primary_thread.as_raw_handle().cast()) };
        if previous_suspend_count != 1 {
            let error = if previous_suspend_count == u32::MAX {
                std::io::Error::last_os_error()
            } else {
                std::io::Error::other(format!(
                    "unexpected primary-thread suspend count {previous_suspend_count}"
                ))
            };
            cleanup_failed_child(&mut child, job);
            return Err(ProcessError::ResumeThread(error));
        }
        drop(primary_thread);
        Ok(Self {
            child,
            job: Some(job),
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
            if self.child.try_wait().ok().flatten().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn request_termination(&mut self) {
        let _ = self.child.kill();
    }

    pub fn force_termination(&mut self) {
        let Some(job) = self.job.take() else {
            return;
        };
        // Closing the sole Job handle is the termination mechanism. This deliberately relies on
        // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE so a mutation removing that limit leaks descendants.
        drop(job);
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

fn suspended_primary_thread(process_id: u32) -> std::io::Result<OwnedHandle> {
    // A live CREATE_SUSPENDED process has not executed user code and therefore has exactly its
    // initial thread. We require one matching owner PID, then revalidate ownership on the opened
    // thread handle before resuming it.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.cast()) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>().try_into().unwrap(),
        ..Default::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut matching_thread = None;
    loop {
        if entry.th32OwnerProcessID == process_id
            && matching_thread.replace(entry.th32ThreadID).is_some()
        {
            return Err(std::io::Error::other(
                "suspended app-server has more than one thread",
            ));
        }
        if unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error);
        }
    }
    let thread_id = matching_thread
        .ok_or_else(|| std::io::Error::other("suspended app-server primary thread not found"))?;
    let thread = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            0,
            thread_id,
        )
    };
    if thread.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let thread = unsafe { OwnedHandle::from_raw_handle(thread.cast()) };
    if unsafe { GetProcessIdOfThread(thread.as_raw_handle().cast()) } != process_id {
        return Err(std::io::Error::other(
            "opened primary thread belongs to a different process",
        ));
    }
    Ok(thread)
}

fn cleanup_failed_child(child: &mut Child, job: OwnedHandle) {
    let _ = child.kill();
    drop(job);
    let _ = child.wait();
}

#[cfg(feature = "test-support")]
fn delay_pre_assignment_for_test(program: &Path) {
    if program.with_extension("started-in-job").exists() {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(not(feature = "test-support"))]
fn delay_pre_assignment_for_test(_program: &Path) {}

fn create_kill_on_close_job() -> Result<OwnedHandle, ProcessError> {
    // Null security attributes request defaults and a null name requests an unnamed Job. A
    // non-null result is an owned handle that must be closed exactly once.
    let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw_job.is_null() {
        return Err(ProcessError::CreateJob(std::io::Error::last_os_error()));
    }
    // `raw_job` was freshly returned and has not been wrapped or closed elsewhere.
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job.cast()) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // `limits` remains initialized and immovable for the call, and its exact byte length is
    // supplied. SetInformationJobObject only borrows both the Job and structure pointers.
    let configured = unsafe {
        SetInformationJobObject(
            job.as_raw_handle().cast(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast::<c_void>(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
                .try_into()
                .unwrap(),
        )
    };
    if configured == 0 {
        return Err(ProcessError::ConfigureJob(std::io::Error::last_os_error()));
    }
    Ok(job)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(
        job_attributes: *const c_void,
        name: *const u16,
    ) -> windows_sys::Win32::Foundation::HANDLE;
}
