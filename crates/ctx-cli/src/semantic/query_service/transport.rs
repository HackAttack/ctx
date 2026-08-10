use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};

use anyhow::Result;
use serde_json::Value;

use super::super::{
    paths_status::{daemon_lock_path, daemon_root_path},
    runtime_limits::DAEMON_QUERY_ENDPOINT_FILE,
};

#[cfg(test)]
pub(in crate::semantic) use ctx_daemon_runtime::read_bounded_daemon_request as read_daemon_query_request;
#[cfg(all(test, unix))]
pub(in crate::semantic) use ctx_daemon_runtime::read_daemon_query_response_unix;
#[cfg(windows)]
pub(in crate::semantic) use ctx_daemon_runtime::{
    daemon_query_pipe_name, open_windows_daemon_query_pipe, read_windows_daemon_query_pipe,
    windows_named_pipe_name_is_local, windows_wide_null, write_all_windows_daemon_query_pipe,
    WindowsIoDeadline, WindowsQueryHandle,
};
#[cfg(test)]
pub(in crate::semantic) use ctx_daemon_runtime::{
    daemon_query_roundtrip, daemon_query_unix_io_error_is_pre_submission_unavailable,
    daemon_query_windows_io_error_is_pre_submission_unavailable, DaemonQueryResponseTooLarge,
};
pub(in crate::semantic) use ctx_daemon_runtime::{
    remove_daemon_service_endpoint_at, write_daemon_service_endpoint_at, DaemonQueryEndpoint,
    DaemonQueryEndpointIdentity,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::semantic) enum DaemonIpcService {
    SemanticQuery,
    SourceRefresh,
}

impl DaemonIpcService {
    fn endpoint_file(self) -> &'static str {
        match self {
            Self::SemanticQuery => DAEMON_QUERY_ENDPOINT_FILE,
            Self::SourceRefresh => "source-refresh-endpoint.json",
        }
    }
}

#[derive(Debug)]
pub(in crate::semantic) struct DaemonQueryServiceUnavailable;

impl fmt::Display for DaemonQueryServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "daemon semantic query service is unavailable; run `ctx daemon run --force` in another terminal or retry with `--refresh background`",
        )
    }
}

impl std::error::Error for DaemonQueryServiceUnavailable {}

#[derive(Debug)]
pub(in crate::semantic) struct DaemonSourceRefreshServiceUnavailable;

impl fmt::Display for DaemonSourceRefreshServiceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daemon source refresh service is unavailable")
    }
}

impl std::error::Error for DaemonSourceRefreshServiceUnavailable {}

impl DaemonSourceRefreshServiceUnavailable {
    pub(in crate::semantic) fn request_may_have_been_submitted(error: &anyhow::Error) -> bool {
        ctx_daemon_runtime::IpcServiceUnavailable::request_may_have_been_submitted(error)
    }
}

#[cfg(test)]
pub(in crate::semantic) fn daemon_query_endpoint_path(data_root: &Path) -> PathBuf {
    daemon_service_endpoint_path(data_root, DaemonIpcService::SemanticQuery)
}

pub(in crate::semantic) fn daemon_service_endpoint_path(
    data_root: &Path,
    service: DaemonIpcService,
) -> PathBuf {
    daemon_root_path(data_root).join(service.endpoint_file())
}

#[cfg(test)]
pub(in crate::semantic) fn write_daemon_query_endpoint(
    data_root: &Path,
    endpoint: &DaemonQueryEndpoint,
) -> Result<()> {
    write_daemon_service_endpoint(data_root, DaemonIpcService::SemanticQuery, endpoint)
}

#[cfg(test)]
pub(in crate::semantic) fn write_daemon_service_endpoint(
    data_root: &Path,
    service: DaemonIpcService,
    endpoint: &DaemonQueryEndpoint,
) -> Result<()> {
    write_daemon_service_endpoint_at(&daemon_service_endpoint_path(data_root, service), endpoint)
}

#[cfg(test)]
pub(in crate::semantic) fn read_daemon_query_endpoint(
    data_root: &Path,
) -> Result<Option<DaemonQueryEndpoint>> {
    Ok(read_daemon_query_endpoint_identity(data_root)?.map(|identity| identity.endpoint))
}

#[cfg(test)]
pub(in crate::semantic) fn read_daemon_query_endpoint_identity(
    data_root: &Path,
) -> Result<Option<DaemonQueryEndpointIdentity>> {
    read_daemon_service_endpoint_identity(data_root, DaemonIpcService::SemanticQuery)
}

pub(in crate::semantic) fn read_daemon_service_endpoint_identity(
    data_root: &Path,
    service: DaemonIpcService,
) -> Result<Option<DaemonQueryEndpointIdentity>> {
    ctx_daemon_runtime::read_daemon_service_endpoint_identity_at(&daemon_service_endpoint_path(
        data_root, service,
    ))
}

#[cfg(test)]
pub(in crate::semantic) fn remove_daemon_query_endpoint_if_matches(
    data_root: &Path,
    expected: &DaemonQueryEndpointIdentity,
) {
    ctx_daemon_runtime::remove_daemon_service_endpoint_if_matches(
        &daemon_lock_path(data_root),
        &daemon_service_endpoint_path(data_root, DaemonIpcService::SemanticQuery),
        expected,
    );
}

pub(in crate::semantic) fn daemon_query_request(
    data_root: &Path,
    request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    daemon_service_request(
        data_root,
        DaemonIpcService::SemanticQuery,
        request,
        timeout,
        max_response_bytes,
    )
}

pub(in crate::semantic) fn daemon_source_refresh_request(
    data_root: &Path,
    request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    daemon_service_request(
        data_root,
        DaemonIpcService::SourceRefresh,
        request,
        timeout,
        max_response_bytes,
    )
}

pub(in crate::semantic) fn daemon_service_request(
    data_root: &Path,
    service: DaemonIpcService,
    request: Value,
    timeout: StdDuration,
    max_response_bytes: u64,
) -> Result<Option<Value>> {
    match ctx_daemon_runtime::daemon_service_request(
        &daemon_lock_path(data_root),
        &daemon_service_endpoint_path(data_root, service),
        request,
        timeout,
        max_response_bytes,
    ) {
        Err(error)
            if error
                .downcast_ref::<ctx_daemon_runtime::IpcServiceUnavailable>()
                .is_some() =>
        {
            match service {
                DaemonIpcService::SemanticQuery => Err(DaemonQueryServiceUnavailable.into()),
                DaemonIpcService::SourceRefresh => {
                    Err(DaemonSourceRefreshServiceUnavailable.into())
                }
            }
        }
        result => result,
    }
}
