mod dispatch;

// Preserve the former parent-module paths for semantic-internal callers.
#[cfg(unix)]
#[allow(unused_imports)]
pub(in crate::semantic) use ctx_daemon_runtime::{
    bind_daemon_service_listener, configure_daemon_query_stream_unix,
};
#[cfg(windows)]
pub(in crate::semantic) use ctx_daemon_runtime::{
    connect_windows_daemon_query_pipe, create_windows_daemon_query_pipe,
    read_daemon_query_request_windows, wake_windows_daemon_query_pipe, WindowsDaemonQueryPipe,
};
#[allow(unused_imports)]
pub(in crate::semantic) use ctx_daemon_runtime::{
    daemon_can_begin_idle_shutdown, handle_authenticated_daemon_stream,
    observe_daemon_query_activity, read_bounded_daemon_request,
    start_ipc_service_with_request_timeout, AuthenticatedRequest, AuthenticatedRequestHandler,
    DaemonQueryActivity, DaemonQueryActivityState, DaemonQueryRequestGuard, DaemonQueryService,
    DaemonWakePort, HandlerOutcome, IpcEndpointPublication, IpcEndpointStore, IpcServiceSpec,
    NoPostWriteAction, PostWriteAction, ServiceId, DAEMON_QUERY_REQUEST_MAX_BYTES,
    DAEMON_QUERY_REQUEST_READ_TIMEOUT,
};
#[cfg(all(test, unix))]
pub(in crate::semantic) use dispatch::bind_daemon_query_listener;
#[allow(unused_imports)]
pub(in crate::semantic) use dispatch::{
    ctx_authenticated_request_handler, start_daemon_query_service,
    start_daemon_source_refresh_service, CtxAuthenticatedRequestHandler,
};
#[cfg(test)]
pub(in crate::semantic) use dispatch::{
    start_daemon_query_service_with_request_timeout,
    start_daemon_source_refresh_service_with_request_timeout,
};
