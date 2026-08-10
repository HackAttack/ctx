use std::{sync::Arc, time::Duration as StdDuration};

#[cfg(unix)]
use std::os::fd::AsRawFd as _;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::{env, fs};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(windows)]
use super::windows_security::WindowsDaemonQueryPipeSecurity;
use super::{
    AuthenticatedRequestHandler, DaemonQueryActivity, DaemonQueryService, DaemonWakePort,
    HandlerOutcome, IpcEndpointPublication, IpcEndpointStore, IpcServiceSpec, PostWriteAction,
    ServiceId, DAEMON_QUERY_REQUEST_MAX_BYTES,
};

/// The transport has bounded, parsed, and authenticated this value. Keeping
/// the parsed request move-only makes the single parse an explicit boundary
/// invariant. Its private field and constructor prevent product dispatch from
/// forging an authenticated request.
pub(in crate::semantic) struct AuthenticatedRequest(Value);

impl AuthenticatedRequest {
    fn from_authenticated_value(value: Value) -> Self {
        Self(value)
    }

    pub(in crate::semantic) fn into_value(self) -> Value {
        self.0
    }
}

#[cfg(windows)]
fn windows_named_pipe_name_is_local(pipe_name: &str) -> bool {
    pipe_name
        .strip_prefix(r"\\.\pipe\ctx-daemon-query-")
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(windows)]
fn windows_wide_null(value: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt as _;

    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
struct WindowsServerIoDeadline {
    started: std::time::Instant,
    timeout: StdDuration,
}

#[cfg(windows)]
impl WindowsServerIoDeadline {
    fn new(timeout: StdDuration) -> Self {
        Self {
            started: std::time::Instant::now(),
            timeout,
        }
    }

    fn remaining_ms(&self, operation: &str) -> std::io::Result<u32> {
        let remaining = self.timeout.saturating_sub(self.started.elapsed());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("daemon query named pipe {operation} timed out"),
            ));
        }
        Ok(remaining.as_millis().max(1).min(u128::from(u32::MAX - 1)) as u32)
    }
}

pub(in crate::semantic) fn handle_authenticated_daemon_stream<
    H: AuthenticatedRequestHandler + ?Sized,
    S: std::io::Write,
>(
    handler: &H,
    service_id: &ServiceId,
    token: &str,
    mut stream: S,
    request: Result<String>,
) -> Result<()> {
    let outcome: HandlerOutcome<H::PostWriteAction<'_>> = match request.and_then(|body| {
        let parsed: Value = serde_json::from_str(&body).context("parse daemon query request")?;
        if parsed.get("token").and_then(Value::as_str) != Some(token) {
            return Err(anyhow!("daemon query authentication failed"));
        }
        Ok(handler.handle(
            service_id,
            AuthenticatedRequest::from_authenticated_value(parsed),
        ))
    }) {
        Ok(outcome) => outcome,
        Err(error) => HandlerOutcome::response(Err(error)),
    };
    let HandlerOutcome {
        response,
        after_write_action,
    } = outcome;
    let response_write = serialize_handler_response(response)
        .and_then(|body| writeln!(stream, "{body}").context("write daemon query response"));
    after_write_action.run();
    response_write
}

fn serialize_handler_response(response: Result<Value>) -> Result<String> {
    match response {
        Ok(value) => serde_json::to_string(&value).context("serialize daemon query response"),
        Err(error) => Ok(serde_json::to_string(&json!({
            "ok": false,
            "error": format!("{error:#}"),
        }))
        .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"query failed\"}".to_owned())),
    }
}

pub(in crate::semantic) fn read_bounded_daemon_request<S: std::io::Read>(
    stream: &mut S,
    max_bytes: usize,
) -> Result<String> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    while body.len() < max_bytes {
        let read_limit = (max_bytes - body.len()).min(chunk.len());
        let read = std::io::Read::read(stream, &mut chunk[..read_limit])
            .context("read daemon query request")?;
        if read == 0 {
            break;
        }
        if let Some(newline) = chunk[..read].iter().position(|byte| *byte == b'\n') {
            body.extend_from_slice(&chunk[..newline]);
            return String::from_utf8(body).context("daemon query request is not UTF-8");
        }
        body.extend_from_slice(&chunk[..read]);
    }
    if body.len() >= max_bytes {
        return Err(anyhow!("daemon query request is too large"));
    }
    String::from_utf8(body).context("daemon query request is not UTF-8")
}

#[cfg(unix)]
fn read_daemon_query_request_unix(
    stream: &mut UnixStream,
    max_bytes: usize,
    timeout: StdDuration,
) -> Result<String> {
    struct DeadlineReader<'a> {
        stream: &'a mut UnixStream,
        started: std::time::Instant,
        timeout: StdDuration,
    }

    impl std::io::Read for DeadlineReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let remaining = self.timeout.saturating_sub(self.started.elapsed());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "daemon query request read timed out",
                ));
            }
            self.stream.set_read_timeout(Some(remaining))?;
            std::io::Read::read(self.stream, buffer).map_err(|error| {
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "daemon query request read timed out",
                    )
                } else {
                    error
                }
            })
        }
    }

    read_bounded_daemon_request(
        &mut DeadlineReader {
            stream,
            started: std::time::Instant::now(),
            timeout,
        },
        max_bytes,
    )
}

struct DaemonServiceThreadExit {
    endpoint_store: Arc<dyn IpcEndpointStore>,
    activity: Arc<DaemonQueryActivity>,
    wakeup: Option<Arc<dyn DaemonWakePort>>,
}

impl Drop for DaemonServiceThreadExit {
    fn drop(&mut self) {
        self.endpoint_store.remove();
        if !self.activity.stopping() {
            if let Some(wakeup) = self.wakeup.as_ref() {
                wakeup.signal_ipc();
            }
        }
    }
}

#[cfg(unix)]
pub(in crate::semantic) const DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES: usize = 90;

#[cfg(unix)]
pub(in crate::semantic) fn bind_daemon_service_listener(
    preferred: &Path,
) -> Result<(UnixListener, PathBuf, Option<PathBuf>)> {
    if preferred.as_os_str().as_bytes().len() <= DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
        let _ = fs::remove_file(preferred);
        let listener = UnixListener::bind(preferred)
            .with_context(|| format!("bind daemon query socket {}", preferred.display()))?;
        return Ok((listener, preferred.to_path_buf(), None));
    }

    let mut roots = vec![PathBuf::from("/tmp")];
    let env_tmp = env::temp_dir();
    if env_tmp != roots[0] {
        roots.push(env_tmp);
    }
    let mut failures = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for _ in 0..8 {
            let runtime_dir = root.join(format!("ctx-q-{}", Uuid::new_v4().simple()));
            match fs::create_dir(&runtime_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    failures.push(format!("create {}: {error}", runtime_dir.display()));
                    break;
                }
            }
            if let Err(error) = fs::set_permissions(&runtime_dir, fs::Permissions::from_mode(0o700))
            {
                let _ = fs::remove_dir(&runtime_dir);
                failures.push(format!("secure {}: {error}", runtime_dir.display()));
                continue;
            }
            let path = runtime_dir.join("q.sock");
            if path.as_os_str().as_bytes().len() > DAEMON_QUERY_SOCKET_PATH_SAFE_BYTES {
                let _ = fs::remove_dir(&runtime_dir);
                failures.push(format!(
                    "fallback socket path is still too long: {}",
                    path.display()
                ));
                continue;
            }
            match UnixListener::bind(&path) {
                Ok(listener) => return Ok((listener, path, Some(runtime_dir))),
                Err(error) => {
                    let _ = fs::remove_file(&path);
                    let _ = fs::remove_dir(&runtime_dir);
                    failures.push(format!("bind {}: {error}", path.display()));
                }
            }
        }
    }
    Err(anyhow!(
        "daemon query socket path is too long and no short private runtime directory was available: {}",
        failures.join("; ")
    ))
}

#[cfg(unix)]
pub(in crate::semantic) fn start_ipc_service_with_request_timeout<
    H: AuthenticatedRequestHandler,
>(
    spec: IpcServiceSpec,
    endpoint_store: Arc<dyn IpcEndpointStore>,
    handler: Arc<H>,
    request_read_timeout: StdDuration,
    wakeup: Option<Arc<dyn DaemonWakePort>>,
) -> Result<DaemonQueryService> {
    endpoint_store.prepare()?;
    let (shutdown_reader, shutdown_stream) =
        UnixStream::pair().context("create daemon query shutdown channel")?;
    let (listener, path, socket_runtime_dir) =
        bind_daemon_service_listener(spec.unix_socket_path())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set daemon query socket permissions {}", path.display()))?;
    let endpoint = IpcEndpointPublication {
        token: Uuid::new_v4().simple().to_string(),
        unix_socket_path: path,
    };
    let socket_path = endpoint.unix_socket_path.clone();
    if let Err(error) = endpoint_store.publish(&endpoint) {
        let _ = fs::remove_file(&socket_path);
        if let Some(dir) = socket_runtime_dir.as_ref() {
            let _ = fs::remove_dir(dir);
        }
        return Err(error);
    }
    let thread_token = endpoint.token.clone();
    let activity = Arc::new(if spec.wake_when_idle() {
        wakeup
            .as_ref()
            .map_or_else(DaemonQueryActivity::new, |wakeup| {
                DaemonQueryActivity::with_idle_wakeup_port(Arc::clone(wakeup))
            })
    } else {
        DaemonQueryActivity::new()
    });
    let thread_activity = activity.clone();
    let thread_wakeup = wakeup;
    let thread_spec = spec.clone();
    let thread_endpoint_store = Arc::clone(&endpoint_store);
    let spawn_result = std::thread::Builder::new()
        .name("ctx-daemon-query".to_owned())
        .spawn(move || {
            let _exit = DaemonServiceThreadExit {
                endpoint_store: thread_endpoint_store,
                activity: Arc::clone(&thread_activity),
                wakeup: thread_wakeup.clone(),
            };
            while !thread_activity.stopping() {
                match wait_for_unix_listener_or_shutdown(&listener, &shutdown_reader) {
                    Ok(true) => {}
                    Ok(false) | Err(_) => break,
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // Accepted Unix sockets inherit nonblocking mode on
                        // macOS. A 384-float response exceeds the default
                        // socket buffer, so restore bounded blocking writes
                        // before serving the request.
                        if configure_daemon_query_stream_unix(&stream, request_read_timeout)
                            .is_err()
                        {
                            continue;
                        }
                        let Some(_request) = thread_activity.begin_request() else {
                            continue;
                        };
                        let request = read_daemon_query_request_unix(
                            &mut stream,
                            DAEMON_QUERY_REQUEST_MAX_BYTES,
                            request_read_timeout,
                        );
                        let _ = handle_authenticated_daemon_stream(
                            handler.as_ref(),
                            thread_spec.service_id(),
                            &thread_token,
                            stream,
                            request,
                        );
                    }
                    Err(_) => break,
                }
            }
        });
    let thread = match spawn_result {
        Ok(thread) => thread,
        Err(error) => {
            endpoint_store.remove();
            let _ = fs::remove_file(socket_path);
            if let Some(dir) = socket_runtime_dir.as_ref() {
                let _ = fs::remove_dir(dir);
            }
            return Err(error).context("start daemon query service thread");
        }
    };
    Ok(DaemonQueryService {
        spec,
        activity,
        thread: Some(thread),
        socket_path: endpoint.unix_socket_path,
        socket_runtime_dir,
        shutdown_stream,
        endpoint_store,
    })
}

#[cfg(unix)]
fn wait_for_unix_listener_or_shutdown(
    listener: &UnixListener,
    shutdown: &UnixStream,
) -> std::io::Result<bool> {
    let mut poll_fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let result =
            unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as libc::nfds_t, -1) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if poll_fds[1].revents != 0 {
            return Ok(false);
        }
        if poll_fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
            != 0
        {
            return Ok(true);
        }
    }
}

#[cfg(unix)]
pub(in crate::semantic) fn configure_daemon_query_stream_unix(
    stream: &UnixStream,
    write_timeout: StdDuration,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(write_timeout))
}

#[cfg(windows)]
pub(in crate::semantic) fn start_ipc_service_with_request_timeout<
    H: AuthenticatedRequestHandler,
>(
    spec: IpcServiceSpec,
    endpoint_store: Arc<dyn IpcEndpointStore>,
    handler: Arc<H>,
    request_read_timeout: StdDuration,
    wakeup: Option<Arc<dyn DaemonWakePort>>,
) -> Result<DaemonQueryService> {
    endpoint_store.prepare()?;
    let endpoint = IpcEndpointPublication {
        windows_pipe_name: format!(r"\\.\pipe\ctx-daemon-query-{}", Uuid::new_v4().simple()),
        token: Uuid::new_v4().simple().to_string(),
    };
    let pipe_name = endpoint.windows_pipe_name.clone();
    let first_stream = create_windows_daemon_query_pipe(&pipe_name, true)?;
    if let Err(error) = endpoint_store.publish(&endpoint) {
        drop(first_stream);
        return Err(error);
    }
    let thread_token = endpoint.token.clone();
    let activity = Arc::new(if spec.wake_when_idle() {
        wakeup
            .as_ref()
            .map_or_else(DaemonQueryActivity::new, |wakeup| {
                DaemonQueryActivity::with_idle_wakeup_port(Arc::clone(wakeup))
            })
    } else {
        DaemonQueryActivity::new()
    });
    let thread_activity = activity.clone();
    let thread_wakeup = wakeup;
    let thread_pipe_name = pipe_name.clone();
    let thread_spec = spec.clone();
    let thread_endpoint_store = Arc::clone(&endpoint_store);
    let spawn_result = std::thread::Builder::new()
        .name("ctx-daemon-query".to_owned())
        .spawn(move || {
            let _exit = DaemonServiceThreadExit {
                endpoint_store: thread_endpoint_store,
                activity: Arc::clone(&thread_activity),
                wakeup: thread_wakeup.clone(),
            };
            let mut next_stream = Some(first_stream);
            while !thread_activity.stopping() {
                let stream = match next_stream.take() {
                    Some(stream) => stream,
                    None => match create_windows_daemon_query_pipe(&thread_pipe_name, false) {
                        Ok(stream) => stream,
                        Err(_) => break,
                    },
                };
                if connect_windows_daemon_query_pipe(&stream).is_err() {
                    break;
                }
                let Some(_request) = thread_activity.begin_request() else {
                    break;
                };
                let stream = stream;
                let request = read_daemon_query_request_windows(
                    &stream,
                    DAEMON_QUERY_REQUEST_MAX_BYTES,
                    request_read_timeout,
                );
                let _ = handle_authenticated_daemon_stream(
                    handler.as_ref(),
                    thread_spec.service_id(),
                    &thread_token,
                    stream,
                    request,
                );
            }
        });
    let thread = match spawn_result {
        Ok(thread) => thread,
        Err(error) => {
            endpoint_store.remove();
            return Err(error).context("start daemon query service thread");
        }
    };
    Ok(DaemonQueryService {
        spec,
        activity,
        thread: Some(thread),
        pipe_name,
        endpoint_store,
    })
}

#[cfg(windows)]
pub(in crate::semantic) struct WindowsDaemonQueryPipe {
    pub(in crate::semantic) handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WindowsDaemonQueryPipe {}

#[cfg(windows)]
impl Drop for WindowsDaemonQueryPipe {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Pipes::DisconnectNamedPipe;

        unsafe {
            let _ = DisconnectNamedPipe(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub(in crate::semantic) struct WindowsDaemonQueryRequestReader<'a> {
    pipe: &'a WindowsDaemonQueryPipe,
    deadline: WindowsServerIoDeadline,
}

#[cfg(windows)]
impl WindowsDaemonQueryRequestReader<'_> {
    pub(in crate::semantic) fn new(
        pipe: &WindowsDaemonQueryPipe,
        timeout: StdDuration,
    ) -> WindowsDaemonQueryRequestReader<'_> {
        WindowsDaemonQueryRequestReader {
            pipe,
            deadline: WindowsServerIoDeadline::new(timeout),
        }
    }
}

#[cfg(windows)]
impl std::io::Read for WindowsDaemonQueryRequestReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::Foundation::{
            GetLastError, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
        };
        use windows_sys::Win32::Storage::FileSystem::ReadFile;
        use windows_sys::Win32::System::Pipes::PeekNamedPipe;

        if buf.is_empty() {
            return Ok(0);
        }

        loop {
            let mut available = 0u32;
            let ok = unsafe {
                PeekNamedPipe(
                    self.pipe.handle,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if matches!(
                    error,
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
                ) {
                    return Ok(0);
                }
                return Err(std::io::Error::from_raw_os_error(error as i32));
            }
            if available == 0 {
                let wait_ms = self.deadline.remaining_ms("request read")?.min(10);
                std::thread::sleep(StdDuration::from_millis(u64::from(wait_ms)));
                continue;
            }

            let mut bytes_read = 0u32;
            let read_len = buf.len().min(available as usize).min(u32::MAX as usize) as u32;
            let ok = unsafe {
                ReadFile(
                    self.pipe.handle,
                    buf.as_mut_ptr(),
                    read_len,
                    &mut bytes_read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = unsafe { GetLastError() };
                if matches!(
                    error,
                    ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
                ) {
                    return Ok(0);
                }
                return Err(std::io::Error::from_raw_os_error(error as i32));
            }
            return Ok(bytes_read as usize);
        }
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn read_daemon_query_request_windows(
    pipe: &WindowsDaemonQueryPipe,
    max_bytes: usize,
    timeout: StdDuration,
) -> Result<String> {
    read_bounded_daemon_request(
        &mut WindowsDaemonQueryRequestReader::new(pipe, timeout),
        max_bytes,
    )
}

#[cfg(windows)]
impl std::io::Write for WindowsDaemonQueryPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;

        if buf.is_empty() {
            return Ok(0);
        }
        let mut bytes_written = 0u32;
        let write_len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let ok = unsafe {
            WriteFile(
                self.handle,
                buf.as_ptr(),
                write_len,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(bytes_written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // FlushFileBuffers waits for the client to drain a named pipe and lets a
        // stalled client block the single query-service thread indefinitely.
        // WriteFile has already copied the response into the pipe buffer.
        Ok(())
    }
}

#[cfg(windows)]
pub(in crate::semantic) fn create_windows_daemon_query_pipe(
    pipe_name: &str,
    first_instance: bool,
) -> Result<WindowsDaemonQueryPipe> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    if !windows_named_pipe_name_is_local(pipe_name) {
        return Err(anyhow!("daemon query pipe name is not local"));
    }
    let mut pipe_security = WindowsDaemonQueryPipeSecurity::for_current_user_and_system()
        .context("build daemon query named pipe security descriptor")?;
    let security_attributes = pipe_security
        .attributes()
        .context("build daemon query named pipe security attributes")?;
    let pipe_name_w = windows_wide_null(pipe_name);
    let access = PIPE_ACCESS_DUPLEX
        | if first_instance {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let handle = unsafe {
        CreateNamedPipeW(
            pipe_name_w.as_ptr(),
            access,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            1024 * 1024,
            256 * 1024,
            0,
            &security_attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("create daemon query named pipe {pipe_name}"));
    }
    let pipe = WindowsDaemonQueryPipe { handle };
    pipe_security
        .verify_handle(pipe.handle)
        .context("verify daemon query named pipe security descriptor")?;
    Ok(pipe)
}

#[cfg(windows)]
pub(in crate::semantic) fn connect_windows_daemon_query_pipe(
    stream: &WindowsDaemonQueryPipe,
) -> Result<()> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_PIPE_CONNECTED};
    use windows_sys::Win32::System::Pipes::ConnectNamedPipe;

    let ok = unsafe { ConnectNamedPipe(stream.handle, std::ptr::null_mut()) };
    if ok != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    Err(std::io::Error::last_os_error()).context("connect daemon query named pipe")
}

#[cfg(windows)]
pub(in crate::semantic) fn wake_windows_daemon_query_pipe(pipe_name: &str) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, OPEN_EXISTING,
    };

    let pipe_name_w = windows_wide_null(pipe_name);
    let handle = unsafe {
        CreateFileW(
            pipe_name_w.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        unsafe {
            let _ = CloseHandle(handle);
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(in crate::semantic) fn start_ipc_service_with_request_timeout<
    H: AuthenticatedRequestHandler,
>(
    _spec: IpcServiceSpec,
    _endpoint_store: Arc<dyn IpcEndpointStore>,
    _handler: Arc<H>,
    _request_read_timeout: StdDuration,
    _wakeup: Option<Arc<dyn DaemonWakePort>>,
) -> Result<DaemonQueryService> {
    Err(anyhow!("IPC service is not supported on this platform"))
}
