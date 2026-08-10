use std::{fs::File, io, path::Path};

#[cfg(unix)]
use std::os::fd::AsRawFd as _;

pub(super) fn copy_existing_security(source: &Path, destination: &File) -> io::Result<()> {
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
fn copy_unix_security(source: &Path, destination: &File) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let source = options.open(source)?;
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
    copy_platform_acl(&source, destination)
}

#[cfg(target_os = "linux")]
fn copy_platform_acl(source: &File, destination: &File) -> io::Result<()> {
    copy_linux_acl(source, destination, b"system.posix_acl_access\0")
}

#[cfg(target_os = "linux")]
fn copy_linux_acl(source: &File, destination: &File, name: &[u8]) -> io::Result<()> {
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
            Ok(())
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
    if unsafe {
        libc::fsetxattr(
            destination.as_raw_fd(),
            name.as_ptr().cast(),
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
