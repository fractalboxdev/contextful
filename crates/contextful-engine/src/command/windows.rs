//! A private, non-inheritable job contains a command before its first instruction.

use super::admission;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::Instant;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
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
    pub(super) fn launch(command: &mut Command) -> io::Result<(Child, Self)> {
        // SAFETY: null attributes/name create a private, non-inheritable job.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a fresh owned handle, checked above.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Neither breakaway flag is enabled; descendants remain in this job.
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the owned job is live; limits has the required C layout and size.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // The child's first instruction waits until job admission and thread verification.
        let mut child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
        if let Err(error) = job.admit_and_resume(&child) {
            // Admission fails closed: no uncontained command executes, and the owned
            // suspended child is killed and waited on before its handles are released.
            // A successful reap proves exit even if kill reports an already-ended
            // process. A failed reap preserves its error and the job's kill-on-close.
            let _ = child.kill();
            job.reap(&mut child)?;
            return Err(error);
        }
        Ok((child, job))
    }

    fn admit_and_resume(&self, child: &Child) -> io::Result<()> {
        let thread = launch_thread(child)?;
        // SAFETY: both handles remain owned and live; std's child process handle
        // carries process admission rights. Assignment precedes the only resume.
        if unsafe {
            AssignProcessToJobObject(self.0.as_raw_handle(), child.as_handle().as_raw_handle())
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the retained handle belongs to the suspended child's sole thread,
        // checked below; the process already belongs to the private job.
        let count = unsafe { ResumeThread(thread.as_raw_handle()) };
        if count == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        admission::resume_count(count)
    }

    pub(super) fn terminate(&self) -> io::Result<()> {
        // SAFETY: this owned job is live and contains only this command's descendants.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn reap(&self, child: &mut Child) -> io::Result<()> {
        self.terminate()?;
        let started = Instant::now();
        loop {
            let reaped = child.try_wait()?.is_some();
            if self.active()? == 0 && reaped {
                return Ok(());
            }
            if started.elapsed() >= super::KILL_CEILING {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the command job still contains active processes after termination",
                ));
            }
            std::thread::sleep(super::WAIT_TICK);
        }
    }

    pub(super) fn active(&self) -> io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: accounting is initialized, writable, and exactly the requested C
        // structure. A failed query remains an error, never a proof of an empty job.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }
}

fn launch_thread(child: &Child) -> io::Result<OwnedHandle> {
    // SAFETY: Toolhelp takes integer flags and creates an owned, finite snapshot.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the snapshot is a fresh valid handle, checked above.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut threads = Vec::new();
    // SAFETY: the live snapshot and initialized C structure are valid for this API.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    loop {
        if found == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
                return Err(error);
            }
            break;
        }
        if entry.th32OwnerProcessID == child.id() {
            threads.push((entry.th32ThreadID, entry.th32OwnerProcessID));
            if threads.len() == 2 {
                break; // Two owned entries prove ambiguity; no thread is resumed.
            }
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        // SAFETY: the same retained snapshot and writable C structure remain valid.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    let id = admission::sole_thread(child.id(), threads)?;
    // SAFETY: OpenThread acquires a new handle; no inherited handle is requested.
    let raw = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            0,
            id,
        )
    };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenThread returned a new owned handle checked above.
    let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: this thread handle is retained and supports the ownership query.
    let owner = unsafe { GetProcessIdOfThread(thread.as_raw_handle()) };
    if owner == 0 {
        return Err(io::Error::last_os_error());
    }
    admission::thread_owner(child.id(), owner)?;
    Ok(thread)
}
