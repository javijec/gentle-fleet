//! Win32 process containment using stable Rust Command and documented APIs.
//! The initial thread is found while the new process is suspended. Its
//! process identity is verified on the opened thread handle before resume.
//! No process is located or terminated by a stale, persisted PID.

use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, THREADENTRY32, TH32CS_SNAPTHREAD,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetProcessIdOfThread, OpenThread, ResumeThread, CREATE_SUSPENDED,
    THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
};

pub(super) struct Job(OwnedHandle);

impl Job {
    fn new() -> io::Result<Self> {
        // Null security attributes => non-inheritable handle. No name and no
        // BREAKAWAY_OK flags: ordinary child processes remain in this job.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub(super) fn active(&self) -> io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let ok = unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        if self.active()? == 0 {
            return Ok(());
        }
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            let error = io::Error::last_os_error();
            if self.active()? != 0 {
                return Err(error);
            }
        }
        Ok(())
    }
}

pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Job)> {
    let job = Job::new()?;
    let mut child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
    let setup = (|| {
        // The actual Child handle is held throughout; assignment is not by PID.
        if unsafe { AssignProcessToJobObject(job.0.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        resume_initial_thread(&mut child)?;
        Ok(())
    })();
    if let Err(error) = setup {
        // Even if job assignment failed, the newly created root is still
        // ours, suspended, and accessible through the original Child handle.
        let _ = job.terminate();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    Ok((child, job))
}

fn resume_initial_thread(child: &mut Child) -> io::Result<()> {
    // std::process::Child owns the process handle but not the primary thread
    // handle. A CREATE_SUSPENDED process has one initial suspended thread
    // before provider code runs. Fail closed if this invariant is not seen.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }

    let mut tid = None;
    loop {
        if entry.dwSize < size_of::<THREADENTRY32>() as u32 {
            return Err(io::Error::other("incomplete thread snapshot entry"));
        }
        if entry.th32OwnerProcessID == child.id() && tid.replace(entry.th32ThreadID).is_some() {
            return Err(io::Error::other("suspended provider has multiple threads"));
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            break;
        }
    }
    let tid = tid.ok_or_else(|| io::Error::other("suspended provider thread not found"))?;
    let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION, 0, tid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
    if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != child.id()
        || child.try_wait()?.is_some()
    {
        return Err(io::Error::other("suspended provider identity changed"));
    }
    let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
    if previous == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    if previous != 1 {
        return Err(io::Error::other("unexpected initial thread suspend count"));
    }
    Ok(())
}
