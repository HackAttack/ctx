mod server;
mod transport;

#[cfg(all(test, unix))]
pub(crate) use server::bind_daemon_query_listener;
pub(crate) use server::{
    ctx_authenticated_request_handler, daemon_can_begin_idle_shutdown,
    observe_daemon_query_activity, start_daemon_query_service, start_daemon_source_refresh_service,
    DaemonQueryActivity, DaemonQueryService,
};
#[cfg(test)]
pub(crate) use server::{
    start_daemon_query_service_with_request_timeout,
    start_daemon_source_refresh_service_with_request_timeout,
};
#[cfg(test)]
pub(crate) use transport::{
    daemon_query_endpoint_path, read_daemon_query_endpoint, read_daemon_query_endpoint_identity,
    remove_daemon_query_endpoint_if_matches, write_daemon_query_endpoint,
    write_daemon_service_endpoint,
};
pub use transport::{
    daemon_query_request, daemon_service_endpoint_path, daemon_source_refresh_request,
    read_daemon_service_endpoint_identity, DaemonIpcService, DaemonQueryEndpoint,
    DaemonQueryServiceUnavailable, DaemonSourceRefreshServiceUnavailable,
};

pub(crate) fn daemon_query_service_transport_supported() -> bool {
    cfg!(any(unix, windows))
}
