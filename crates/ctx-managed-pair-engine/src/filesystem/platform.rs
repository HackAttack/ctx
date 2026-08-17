use std::fs::File;

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{anyhow, Context, Result};

#[cfg(windows)]
use super::{open_owner_regular_for_delete, require_file_identity};
use super::{require_stamp, Entry, FileStamp};

#[cfg(unix)]
pub(super) fn file_information(file: &File, _label: &str) -> Result<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino(), metadata.len()))
}

#[cfg(windows)]
pub(super) fn file_information(file: &File, label: &str) -> Result<(u64, u64, u64)> {
    let (device, identity, _) = windows_file_information(file, label)?;
    Ok((device, identity, file.metadata()?.len()))
}

#[cfg(windows)]
fn windows_file_information(file: &File, label: &str) -> Result<(u64, u64, u32)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("inspect {label}"));
    }
    let information = unsafe { information.assume_init() };
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        information.nNumberOfLinks,
    ))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn file_information(_file: &File, _label: &str) -> Result<(u64, u64, u64)> {
    bail!("managed-pair file identity is unsupported on this platform")
}

#[cfg(unix)]
pub(super) fn durable_rename(
    source_entry: &Entry,
    target_entry: &Entry,
    _expected: &FileStamp,
    _label: &str,
    _replace: bool,
) -> Result<()> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt as _, io::AsRawFd as _},
    };
    let source = CString::new(source_entry.name.as_bytes())
        .map_err(|_| anyhow!("managed-pair source name contains a NUL"))?;
    let target = CString::new(target_entry.name.as_bytes())
        .map_err(|_| anyhow!("managed-pair target name contains a NUL"))?;
    if unsafe {
        libc::renameat(
            source_entry.directory.file.as_raw_fd(),
            source.as_ptr(),
            target_entry.directory.file.as_raw_fd(),
            target.as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("rename managed-pair file");
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn durable_rename(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    label: &str,
    replace: bool,
) -> Result<()> {
    use std::{
        mem::size_of,
        os::windows::{ffi::OsStrExt as _, io::AsRawHandle as _},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfo, SetFileInformationByHandle, FILE_RENAME_INFO,
    };

    let file = open_owner_regular_for_delete(source, label)?;
    require_file_identity(&file, expected, label)?;
    let name: Vec<u16> = target.name.encode_wide().collect();
    if name.is_empty() || name.contains(&0) {
        bail!("managed-pair target name is invalid");
    }
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| anyhow!("managed-pair target name is too long"))?;
    // Windows documents FileNameLength without the terminator, while the
    // FILE_RENAME_INFO buffer itself must include its trailing WCHAR storage.
    // The zero-filled tail therefore supplies the required terminator.
    let total_bytes = size_of::<FILE_RENAME_INFO>()
        .checked_add(name_bytes)
        .ok_or_else(|| anyhow!("managed-pair rename buffer is too large"))?;
    let words = total_bytes.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*information).Anonymous.ReplaceIfExists = replace;
        (*information).RootDirectory = target.directory.file.as_raw_handle().cast();
        (*information).FileNameLength = u32::try_from(name_bytes)?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            name.len(),
        );
    }
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileRenameInfo,
            information.cast(),
            u32::try_from(total_bytes)?,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("rename managed-pair file by handle");
    }
    file.sync_all()?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn durable_replace(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, true)?;
    require_stamp(target, expected, max, label)
}

#[cfg(unix)]
pub(crate) fn durable_replace(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, true)?;
    target.directory.sync()?;
    require_stamp(target, expected, max, label)
}

pub(super) fn sync_parent(entry: &Entry) -> Result<()> {
    entry.directory.sync()
}

#[cfg(windows)]
pub(crate) fn current_process_creation_identity() -> Result<u64> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    process_creation_identity(unsafe { GetCurrentProcess() })
}

#[cfg(windows)]
pub(crate) fn wait_for_parent_exit(parent_pid: u32, parent_creation_time: u64) -> Result<()> {
    wait_for_parent_exit_with_timeout(parent_pid, parent_creation_time, 5 * 60 * 1_000)
}

#[cfg(windows)]
fn wait_for_parent_exit_with_timeout(
    parent_pid: u32,
    parent_creation_time: u64,
    timeout_ms: u32,
) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_INVALID_PARAMETER, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };
    if parent_pid == 0 || parent_pid == std::process::id() || parent_creation_time == 0 {
        bail!("managed-pair swapper has an invalid parent identity");
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            parent_pid,
        )
    };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(error).context("open managed-pair parent process");
    }
    let observed_creation_time = process_creation_identity(handle);
    if observed_creation_time
        .as_ref()
        .is_ok_and(|observed| *observed != parent_creation_time)
    {
        unsafe { CloseHandle(handle) };
        return Ok(());
    }
    observed_creation_time?;
    let status = unsafe { WaitForSingleObject(handle, timeout_ms) };
    unsafe { CloseHandle(handle) };
    match status {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => bail!("timed out waiting for the managed-pair parent process to exit"),
        WAIT_FAILED => {
            Err(std::io::Error::last_os_error()).context("wait for managed-pair parent process")
        }
        other => bail!("unexpected managed-pair parent wait status {other}"),
    }
}

#[cfg(windows)]
fn process_creation_identity(handle: windows_sys::Win32::Foundation::HANDLE) -> Result<u64> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};

    let mut creation = MaybeUninit::<FILETIME>::zeroed();
    let mut exit = MaybeUninit::<FILETIME>::zeroed();
    let mut kernel = MaybeUninit::<FILETIME>::zeroed();
    let mut user = MaybeUninit::<FILETIME>::zeroed();
    if unsafe {
        GetProcessTimes(
            handle,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("read managed-pair parent creation identity");
    }
    let creation = unsafe { creation.assume_init() };
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[cfg(all(test, windows))]
pub(crate) fn process_creation_identity_for_test(parent_pid: u32) -> Result<u64> {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent_pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error()).context("open test parent process");
    }
    let identity = process_creation_identity(handle);
    unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
    identity
}

#[cfg(all(test, windows))]
pub(crate) fn wait_for_parent_exit_for_test(
    parent_pid: u32,
    parent_creation_time: u64,
    timeout_ms: u32,
) -> Result<()> {
    wait_for_parent_exit_with_timeout(parent_pid, parent_creation_time, timeout_ms)
}
