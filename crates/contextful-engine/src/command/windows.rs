//! A private, non-inheritable job contains a command before its first instruction.

use super::admission;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetProcessIdOfThread, OpenProcess, OpenThread, ResumeThread, WaitForSingleObject,
    CREATE_SUSPENDED, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
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
        let started = Instant::now();
        let before = self.accounting();
        let members = before
            .as_ref()
            .map_err(|e| io::Error::new(e.kind(), e.to_string()))
            .and_then(|info| self.members(info.ActiveProcesses, started));
        // Even a failed membership snapshot closes admission by terminating this
        // owned job before the error returns. No partial snapshot proves completion.
        self.terminate()?;
        let before = before?;
        let members = members?;
        if members.len() < before.ActiveProcesses as usize {
            return Err(io::Error::other(
                "job membership changed before termination",
            ));
        }
        loop {
            if started.elapsed() >= super::KILL_CEILING {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the command job exceeds its termination deadline",
                ));
            }
            let reaped = child.try_wait()?.is_some();
            let now = self.accounting()?;
            if now.TotalProcesses != before.TotalProcesses {
                return Err(io::Error::other(
                    "new job membership lacks retained completion handles",
                ));
            }
            let mut signalled = Vec::with_capacity(members.len());
            for member in &members {
                // SAFETY: each retained handle has synchronize rights and verified
                // membership in this exact job; no historical PID establishes exit.
                signalled.push(
                    match unsafe { WaitForSingleObject(member.as_raw_handle(), 0) } {
                        WAIT_OBJECT_0 => true,
                        WAIT_TIMEOUT => false,
                        _ => return Err(io::Error::last_os_error()),
                    },
                );
            }
            if admission::reaped_members(now.ActiveProcesses, reaped, signalled) {
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

    fn accounting(&self) -> io::Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
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
        Ok(accounting)
    }

    fn members(&self, count: u32, started: Instant) -> io::Result<Vec<OwnedHandle>> {
        // The kernel reports the capacity. Growing membership refuses completion,
        // rather than starting an unbounded resize loop or trusting a historical PID.
        let count = (count as usize).max(1);
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
        let bytes = header
            .checked_add(
                count
                    .checked_mul(size_of::<usize>())
                    .ok_or_else(|| io::Error::other("job membership size overflow"))?,
            )
            .ok_or_else(|| io::Error::other("job membership size overflow"))?;
        let bytes_u32 =
            u32::try_from(bytes).map_err(|_| io::Error::other("job membership size overflow"))?;
        let mut buffer = Vec::<usize>::new();
        buffer
            .try_reserve_exact(bytes.div_ceil(size_of::<usize>()))
            .map_err(io::Error::other)?;
        buffer.resize(bytes.div_ceil(size_of::<usize>()), 0);
        let pointer = buffer
            .as_mut_ptr()
            .cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
        // SAFETY: usize storage provides the C structure's alignment; initialized
        // capacity holds its eight-byte header and count variable-length PID slots.
        if unsafe {
            QueryInformationJobObject(
                self.0.as_raw_handle(),
                JobObjectBasicProcessIdList,
                pointer.cast(),
                bytes_u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful query initializes the header in this live buffer.
        let list = unsafe { &*pointer };
        let populated = list.NumberOfProcessIdsInList as usize;
        if populated > count || list.NumberOfAssignedProcesses as usize != populated {
            return Err(io::Error::other("job membership snapshot is incomplete"));
        }
        // SAFETY: the validated count fits the allocation's contiguous PID area;
        // the underlying buffer remains live for the whole iteration.
        let ids = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>().add(header).cast::<usize>(),
                populated,
            )
        };
        let mut handles = Vec::new();
        for &id in ids {
            if started.elapsed() >= super::KILL_CEILING {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "job membership capture exceeds its termination deadline",
                ));
            }
            let pid = u32::try_from(id)
                .map_err(|_| io::Error::other("job PID exceeds its native representation"))?;
            // SAFETY: lookup acquires query/synchronize rights only, never cleanup
            // rights. Membership in the exact owned job independently gates use.
            let raw = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: OpenProcess returned a fresh, checked, owned process handle.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut member = 0;
            // SAFETY: both retained handles are live; member is writable BOOL storage.
            if unsafe {
                IsProcessInJob(handle.as_raw_handle(), self.0.as_raw_handle(), &mut member)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if member == 0 {
                return Err(io::Error::other(
                    "the retained process does not belong to the command job",
                ));
            }
            handles.push(handle);
        }
        Ok(handles)
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
