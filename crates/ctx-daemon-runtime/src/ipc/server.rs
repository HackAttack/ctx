use std::{
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant},
};

#[cfg(unix)]
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

#[cfg(unix)]
use std::{fs, net::Shutdown, os::unix::net::UnixStream};

mod transport;
pub use transport::AuthenticatedRequest;
#[cfg(windows)]
mod windows_security;

pub use transport::*;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ServiceId(String);

impl ServiceId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && !value.starts_with('-')
            && !value.ends_with('-')
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            anyhow::bail!(
                "IPC service id must be 1-64 lowercase ASCII letters, digits, or interior hyphens"
            );
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcServiceSpec {
    service_id: ServiceId,
    wake_when_idle: bool,
    #[cfg(unix)]
    unix_socket_path: PathBuf,
}

impl IpcServiceSpec {
    #[cfg(unix)]
    pub fn new(
        service_id: ServiceId,
        unix_socket_path: PathBuf,
        wake_when_idle: bool,
    ) -> Result<Self> {
        if unix_socket_path.file_name().is_none() {
            anyhow::bail!("IPC service Unix socket path must name a socket");
        }
        Ok(Self {
            service_id,
            wake_when_idle,
            unix_socket_path,
        })
    }

    #[cfg(not(unix))]
    pub fn new(service_id: ServiceId, wake_when_idle: bool) -> Result<Self> {
        Ok(Self {
            service_id,
            wake_when_idle,
        })
    }

    pub fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    pub fn wake_when_idle(&self) -> bool {
        self.wake_when_idle
    }

    #[cfg(unix)]
    pub fn unix_socket_path(&self) -> &Path {
        &self.unix_socket_path
    }
}

pub trait AuthenticatedRequestHandler: Send + Sync + 'static {
    type PostWriteAction<'a>: PostWriteAction + Default
    where
        Self: 'a;

    fn handle<'a>(
        &'a self,
        service: &ServiceId,
        request: AuthenticatedRequest,
    ) -> HandlerOutcome<Self::PostWriteAction<'a>>;
}

pub trait DaemonWakePort: Send + Sync + 'static {
    fn signal_ipc(&self);
}

/// A bounded, move-only product action that runs after the response-write
/// attempt. The transport knows only this neutral contract; composition owns
/// concrete release and wake-up mechanics.
pub trait PostWriteAction {
    fn run(self);
}

/// The zero-sized default action keeps ordinary daemon requests allocation-
/// and refcount-free after their response-write attempt.
#[derive(Default)]
pub struct NoPostWriteAction;

impl PostWriteAction for NoPostWriteAction {
    fn run(self) {}
}

pub struct HandlerOutcome<A> {
    pub response: Result<Value>,
    pub after_write_action: A,
}

impl<A: Default> HandlerOutcome<A> {
    pub fn response(response: Result<Value>) -> Self {
        Self {
            response,
            after_write_action: A::default(),
        }
    }
}

impl<A> HandlerOutcome<A> {
    pub fn with_post_write_action(response: Result<Value>, after_write_action: A) -> Self {
        Self {
            response,
            after_write_action,
        }
    }
}

pub struct DaemonQueryService {
    pub spec: IpcServiceSpec,
    pub activity: Arc<DaemonQueryActivity>,
    pub thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    pub socket_path: PathBuf,
    #[cfg(unix)]
    pub socket_runtime_dir: Option<PathBuf>,
    #[cfg(unix)]
    pub shutdown_stream: UnixStream,
    #[cfg(windows)]
    pub pipe_name: String,
    pub endpoint_store: Arc<dyn IpcEndpointStore>,
}

pub const DAEMON_QUERY_REQUEST_MAX_BYTES: usize = 256 * 1024;
pub const DAEMON_QUERY_REQUEST_READ_TIMEOUT: StdDuration = StdDuration::from_secs(2);

impl DaemonQueryService {
    pub fn service_id(&self) -> &ServiceId {
        self.spec.service_id()
    }

    pub fn listener_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
    }

    #[cfg(unix)]
    #[doc(hidden)]
    pub fn terminate_listener_for_test(&self) {
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
    }
}

impl Drop for DaemonQueryService {
    fn drop(&mut self) {
        self.activity.stop();
        #[cfg(unix)]
        {
            let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        }
        #[cfg(windows)]
        transport::wake_windows_daemon_query_pipe(&self.pipe_name);
        self.endpoint_store.remove();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(unix)]
        {
            let _ = fs::remove_file(&self.socket_path);
            if let Some(dir) = self.socket_runtime_dir.as_ref() {
                let _ = fs::remove_dir(dir);
            }
        }
    }
}

#[derive(Default)]
pub struct DaemonQueryActivity {
    pub state: Mutex<DaemonQueryActivityState>,
    idle_wakeup: Option<Arc<dyn DaemonWakePort>>,
}

#[derive(Default)]
pub struct DaemonQueryActivityState {
    pub accepting: bool,
    pub stopping: bool,
    pub active_requests: usize,
    pub generation: u64,
    wake_when_idle: bool,
}

pub struct DaemonQueryRequestGuard {
    pub activity: Arc<DaemonQueryActivity>,
}

impl DaemonQueryActivity {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DaemonQueryActivityState {
                accepting: true,
                ..DaemonQueryActivityState::default()
            }),
            idle_wakeup: None,
        }
    }

    pub fn with_idle_wakeup<W: DaemonWakePort>(idle_wakeup: Arc<W>) -> Self {
        let mut activity = Self::new();
        activity.idle_wakeup = Some(idle_wakeup);
        activity
    }

    pub fn with_idle_wakeup_port(idle_wakeup: Arc<dyn DaemonWakePort>) -> Self {
        let mut activity = Self::new();
        activity.idle_wakeup = Some(idle_wakeup);
        activity
    }

    pub fn state(&self) -> std::sync::MutexGuard<'_, DaemonQueryActivityState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn begin_request(self: &Arc<Self>) -> Option<DaemonQueryRequestGuard> {
        let mut state = self.state();
        if !state.accepting || state.stopping {
            return None;
        }
        state.active_requests = state.active_requests.saturating_add(1);
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        Some(DaemonQueryRequestGuard {
            activity: self.clone(),
        })
    }

    pub fn snapshot(&self) -> (usize, u64) {
        let state = self.state();
        (state.active_requests, state.generation)
    }

    pub fn wake_daemon_when_idle(&self) {
        let should_signal = {
            let mut state = self.state();
            if state.active_requests == 0 {
                true
            } else {
                state.wake_when_idle = true;
                false
            }
        };
        if should_signal {
            if let Some(wakeup) = self.idle_wakeup.as_ref() {
                wakeup.signal_ipc();
            }
        }
    }

    pub fn cancel_idle_wakeup(&self) {
        self.state().wake_when_idle = false;
    }

    pub fn try_stop_accepting_if_idle(&self, observed_generation: u64) -> bool {
        let mut state = self.state();
        if state.active_requests != 0 || state.generation != observed_generation {
            return false;
        }
        state.accepting = false;
        true
    }

    pub fn resume_accepting(&self) {
        let mut state = self.state();
        if !state.stopping {
            state.accepting = true;
        }
    }

    pub fn stop(&self) {
        let mut state = self.state();
        state.accepting = false;
        state.stopping = true;
    }

    pub fn stopping(&self) -> bool {
        self.state().stopping
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpcEndpointPublication {
    pub token: String,
    #[cfg(unix)]
    pub unix_socket_path: PathBuf,
    #[cfg(windows)]
    pub windows_pipe_name: String,
}

pub trait IpcEndpointStore: Send + Sync + 'static {
    fn prepare(&self) -> Result<()>;
    fn publish(&self, endpoint: &IpcEndpointPublication) -> Result<()>;
    fn remove(&self);
}

impl Drop for DaemonQueryRequestGuard {
    fn drop(&mut self) {
        let mut state = self.activity.state();
        state.active_requests = state.active_requests.saturating_sub(1);
        state.generation = state.generation.wrapping_add(1);
        let should_signal = state.active_requests == 0 && state.wake_when_idle;
        if should_signal {
            state.wake_when_idle = false;
        }
        drop(state);
        if should_signal {
            if let Some(wakeup) = self.activity.idle_wakeup.as_ref() {
                wakeup.signal_ipc();
            }
        }
    }
}

pub fn observe_daemon_query_activity(
    activity: Option<&DaemonQueryActivity>,
    idle_since: &mut Option<Instant>,
    observed_generation: &mut u64,
) {
    let Some(activity) = activity else {
        return;
    };
    let (active_requests, generation) = activity.snapshot();
    if active_requests != 0 || generation != *observed_generation {
        *idle_since = None;
        *observed_generation = generation;
    }
}

pub fn daemon_can_begin_idle_shutdown(
    activity: Option<&DaemonQueryActivity>,
    observed_generation: u64,
) -> bool {
    activity.is_none_or(|activity| activity.try_stop_accepting_if_idle(observed_generation))
}
