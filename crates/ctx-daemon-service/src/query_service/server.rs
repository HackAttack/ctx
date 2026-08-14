mod dispatch;

// Preserve the former parent-module paths for semantic-internal callers.
#[cfg(unix)]
#[allow(unused_imports)]
pub(crate) use ctx_daemon_runtime::{
    bind_daemon_service_listener, configure_daemon_query_stream_unix,
};
#[cfg(windows)]
#[allow(unused_imports)]
pub(crate) use ctx_daemon_runtime::{
    connect_windows_daemon_query_pipe, create_windows_daemon_query_pipe,
    read_daemon_query_request_windows, wake_windows_daemon_query_pipe, WindowsDaemonQueryPipe,
};
#[allow(unused_imports)]
pub(crate) use ctx_daemon_runtime::{
    handle_authenticated_daemon_stream, read_bounded_daemon_request,
    start_ipc_service_with_request_timeout, AuthenticatedRequest, AuthenticatedRequestHandler,
    DaemonQueryActivity, DaemonQueryActivityState, DaemonQueryRequestGuard, DaemonQueryService,
    DaemonWakePort, HandlerOutcome, IpcEndpointPublication, IpcEndpointStore, IpcServiceSpec,
    NoPostWriteAction, PostWriteAction, ServiceId, DAEMON_QUERY_REQUEST_MAX_BYTES,
    DAEMON_QUERY_REQUEST_READ_TIMEOUT,
};
#[cfg(all(test, unix))]
pub(crate) use dispatch::bind_daemon_query_listener;
#[cfg(test)]
pub(crate) use dispatch::{
    ctx_authenticated_request_handler, start_daemon_query_service_with_request_timeout,
    start_daemon_source_refresh_service_with_request_timeout,
};
#[allow(unused_imports)]
pub(crate) use dispatch::{
    ctx_authenticated_request_handler_with_lifecycle, start_daemon_query_service,
    start_daemon_source_refresh_service, CtxAuthenticatedRequestHandler, DaemonLifecycleState,
};
