#[cfg(unix)]
#[path = "platform/unix.rs"]
mod imp;
#[cfg(windows)]
#[path = "platform/windows.rs"]
mod imp;

#[cfg(not(any(unix, windows)))]
compile_error!("generation read leases require Unix or Windows byte-range locking");

pub(super) use imp::OpenedCoordinator;

#[derive(Debug, Clone, Copy)]
pub(super) enum LockKind {
    Shared,
    Exclusive,
}
