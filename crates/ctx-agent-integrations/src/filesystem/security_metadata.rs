use std::{fs::File, io};

#[cfg(unix)]
use std::os::fd::AsRawFd as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SecurityMetadata {
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    acl: Option<Vec<u8>>,
    #[cfg(windows)]
    descriptor: Vec<u8>,
}

pub(super) fn snapshot(file: &File) -> io::Result<SecurityMetadata> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = file.metadata()?;
        Ok(SecurityMetadata {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            acl: platform_acl_snapshot(file)?,
        })
    }
    #[cfg(windows)]
    {
        Ok(SecurityMetadata {
            descriptor: windows_security_descriptor(file)?,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "security metadata observation is unsupported on this platform",
        ))
    }
}

pub(super) fn copy_existing_security(source: &File, destination: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        copy_unix_security(source, destination)
    }
    #[cfg(windows)]
    {
        // ReplaceFileW preserves the replaced file's security descriptor.
        let _ = (source, destination);
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (source, destination);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "security metadata preservation is unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn copy_unix_security(source: &File, destination: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = source.metadata()?;
    let destination_fd = destination.as_raw_fd();
    if unsafe { libc::fchown(destination_fd, metadata.uid(), metadata.gid()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // File-type bits are not inputs to fchmod, and masking keeps the value
    // representable on Unix platforms whose mode_t is narrower than u32.
    let mode = (metadata.mode() & 0o7777) as libc::mode_t;
    if unsafe { libc::fchmod(destination_fd, mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    copy_platform_acl(source, destination)
}

#[cfg(target_os = "linux")]
fn copy_platform_acl(source: &File, destination: &File) -> io::Result<()> {
    let Some(acl) = linux_acl(source, b"system.posix_acl_access\0")? else {
        return Ok(());
    };
    if unsafe {
        libc::fsetxattr(
            destination.as_raw_fd(),
            c"system.posix_acl_access".as_ptr(),
            acl.as_ptr().cast(),
            acl.len(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_acl_snapshot(file: &File) -> io::Result<Option<Vec<u8>>> {
    linux_acl(file, b"system.posix_acl_access\0")
}

#[cfg(target_os = "linux")]
fn linux_acl(source: &File, name: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let size = unsafe {
        libc::fgetxattr(
            source.as_raw_fd(),
            name.as_ptr().cast(),
            std::ptr::null_mut(),
            0,
        )
    };
    if size < 0 {
        let error = io::Error::last_os_error();
        return if matches!(
            error.raw_os_error(),
            Some(libc::ENODATA) | Some(libc::ENOTSUP)
        ) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let size = usize::try_from(size).map_err(|_| io::Error::other("ACL is too large"))?;
    let mut acl = vec![0_u8; size];
    if size != 0 {
        let read = unsafe {
            libc::fgetxattr(
                source.as_raw_fd(),
                name.as_ptr().cast(),
                acl.as_mut_ptr().cast(),
                acl.len(),
            )
        };
        if usize::try_from(read).ok() != Some(size) {
            return Err(if read < 0 {
                io::Error::last_os_error()
            } else {
                io::Error::other("ACL changed while it was copied")
            });
        }
    }
    Ok(Some(acl))
}

#[cfg(target_os = "macos")]
fn copy_platform_acl(source: &File, destination: &File) -> io::Result<()> {
    use std::ffi::c_void;

    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut c_void;
        fn acl_set_fd_np(fd: libc::c_int, acl: *mut c_void, acl_type: libc::c_int) -> libc::c_int;
        fn acl_free(object: *mut c_void) -> libc::c_int;
    }
    let acl = unsafe { acl_get_fd_np(source.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let set = unsafe { acl_set_fd_np(destination.as_raw_fd(), acl, ACL_TYPE_EXTENDED) };
    let free = unsafe { acl_free(acl) };
    if set != 0 || free != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn platform_acl_snapshot(file: &File) -> io::Result<Option<Vec<u8>>> {
    use std::{ffi::c_void, slice};

    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    unsafe extern "C" {
        fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut c_void;
        fn acl_to_text(acl: *mut c_void, length: *mut libc::ssize_t) -> *mut libc::c_char;
        fn acl_free(object: *mut c_void) -> libc::c_int;
    }
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let mut length = 0;
    let text = unsafe { acl_to_text(acl, &mut length) };
    let result = if text.is_null() || length < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(Some(unsafe {
            slice::from_raw_parts(text.cast::<u8>(), length as usize).to_vec()
        }))
    };
    let text_free = if text.is_null() {
        0
    } else {
        unsafe { acl_free(text.cast()) }
    };
    let acl_free_result = unsafe { acl_free(acl) };
    if text_free != 0 || acl_free_result != 0 {
        Err(io::Error::last_os_error())
    } else {
        result
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn copy_platform_acl(source: &File, destination: &File) -> io::Result<()> {
    use std::ffi::c_void;

    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut c_void;
        fn acl_set_fd(fd: libc::c_int, acl: *mut c_void) -> libc::c_int;
        fn acl_free(object: *mut c_void) -> libc::c_int;
    }
    let acl = unsafe { acl_get_fd(source.as_raw_fd()) };
    if acl.is_null() {
        return Err(io::Error::last_os_error());
    }
    let set = unsafe { acl_set_fd(destination.as_raw_fd(), acl) };
    let free = unsafe { acl_free(acl) };
    if set != 0 || free != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn platform_acl_snapshot(file: &File) -> io::Result<Option<Vec<u8>>> {
    use std::{ffi::c_void, slice};

    unsafe extern "C" {
        fn acl_get_fd(fd: libc::c_int) -> *mut c_void;
        fn acl_to_text(acl: *mut c_void, length: *mut libc::ssize_t) -> *mut libc::c_char;
        fn acl_free(object: *mut c_void) -> libc::c_int;
    }
    let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
    if acl.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    let text = unsafe { acl_to_text(acl, &mut length) };
    let result = if text.is_null() || length < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(Some(unsafe {
            slice::from_raw_parts(text.cast::<u8>(), length as usize).to_vec()
        }))
    };
    let text_free = if text.is_null() {
        0
    } else {
        unsafe { acl_free(text.cast()) }
    };
    let acl_free_result = unsafe { acl_free(acl) };
    if text_free != 0 || acl_free_result != 0 {
        Err(io::Error::last_os_error())
    } else {
        result
    }
}

#[cfg(windows)]
fn windows_security_descriptor(file: &File) -> io::Result<Vec<u8>> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Security::{
        GetKernelObjectSecurity, DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION,
    };

    let requested =
        OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut needed = 0_u32;
    unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle().cast(),
            requested,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let size = usize::try_from(needed)
        .map_err(|_| io::Error::other("security descriptor is too large"))?;
    let mut descriptor = vec![0_u8; size];
    if unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle().cast(),
            requested,
            descriptor.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    descriptor.truncate(usize::try_from(needed).unwrap_or(size).min(size));
    Ok(descriptor)
}
