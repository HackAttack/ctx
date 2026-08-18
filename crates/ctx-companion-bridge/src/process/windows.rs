use std::{
    ffi::OsString,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStringExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
        process::CommandExt as _,
    },
    process::{Child, Command},
    ptr,
};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::{
    Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    },
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
    SystemInformation::GetSystemWindowsDirectoryW,
    Threading::{OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME},
};

use crate::BridgeError;

pub(super) struct ForegroundTerminal;

impl ForegroundTerminal {
    pub(super) fn handoff(_enabled: bool, _process_group: u32) -> Result<Self, BridgeError> {
        Ok(Self)
    }

    pub(super) fn restore(&mut self) -> Result<(), BridgeError> {
        Ok(())
    }
}

pub(super) struct ProcessTree {
    job: OwnedHandle,
}

impl ProcessTree {
    pub(super) fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
    }
}

pub(super) fn configure_required_environment(command: &mut Command) -> Result<(), BridgeError> {
    let mut buffer = vec![0_u16; 260];
    loop {
        let returned =
            unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) }
                as usize;
        if returned == 0 {
            return Err(BridgeError::Transport(io::Error::last_os_error()));
        }
        if returned >= buffer.len() {
            let capacity = returned
                .checked_add(1)
                .filter(|value| *value <= 32_768)
                .ok_or(BridgeError::Limit("Windows system-root bytes"))?;
            buffer.resize(capacity, 0);
            continue;
        }
        buffer.truncate(returned);
        command.env("SystemRoot", OsString::from_wide(&buffer));
        return Ok(());
    }
}

pub(super) fn spawn(command: &mut Command) -> Result<(Child, ProcessTree), BridgeError> {
    command.creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn().map_err(BridgeError::Spawn)?;
    match start_job(&child) {
        Ok(tree) => Ok((child, tree)),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(BridgeError::Spawn(error))
        }
    }
}

fn start_job(child: &Child) -> io::Result<ProcessTree> {
    let raw_job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if raw_job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            ptr::addr_of!(limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let tree = ProcessTree { job };
    if let Err(error) = resume_process(child.id()) {
        tree.terminate();
        return Err(error);
    }
    Ok(tree)
}

fn resume_process(process_id: u32) -> io::Result<()> {
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    if unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } == 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        if entry.th32OwnerProcessID == process_id {
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if raw_thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread) };
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "suspended companion thread not found",
            ));
        }
    }
}
