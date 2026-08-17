use std::{path::Path, sync::Arc};

use anyhow::anyhow;
use ctx_history_core::utc_now;
use ctx_semantic_model::semantic_query_service_supported;
use serde_json::{json, Value};

use crate::{
    config::{AppConfig, DaemonLifecycle, DaemonMode},
    DaemonConfigPort, DaemonRunArgs,
};

use super::{
    super::{
        daemon_wakeup::DaemonWakeup,
        paths_status::lower_semantic_worker_priority,
        query_service::{
            ctx_authenticated_request_handler_with_lifecycle,
            daemon_query_service_transport_supported, start_daemon_query_service,
            start_daemon_source_refresh_service, DaemonLifecycleState, DaemonQueryService,
        },
    },
    DaemonRuntime,
};

#[derive(Debug, Clone)]
pub(super) struct DaemonConfigReloadState {
    pub(super) status: &'static str,
    last_attempt_at_ms: i64,
    last_applied_at_ms: Option<i64>,
    requested_daemon_lifecycle: DaemonLifecycle,
    requested_daemon_mode: DaemonMode,
    requested_semantic_enabled: bool,
    applied_daemon_lifecycle: Option<DaemonLifecycle>,
    applied_daemon_mode: Option<DaemonMode>,
    applied_semantic_enabled: Option<bool>,
    pub(super) last_error: Option<String>,
}

impl DaemonConfigReloadState {
    pub(super) fn pending(config: &AppConfig) -> Self {
        Self {
            status: "pending",
            last_attempt_at_ms: utc_now().timestamp_millis(),
            last_applied_at_ms: None,
            requested_daemon_lifecycle: config.daemon.lifecycle,
            requested_daemon_mode: config.daemon.mode,
            requested_semantic_enabled: config.semantic_search_enabled(),
            applied_daemon_lifecycle: None,
            applied_daemon_mode: None,
            applied_semantic_enabled: None,
            last_error: None,
        }
    }

    fn begin_attempt(&mut self, config: &AppConfig) {
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        self.requested_daemon_lifecycle = config.daemon.lifecycle;
        self.requested_daemon_mode = config.daemon.mode;
        self.requested_semantic_enabled = config.semantic_search_enabled();
        self.last_error = None;
    }

    fn applied(&mut self) {
        self.status = "applied";
        self.last_applied_at_ms = Some(self.last_attempt_at_ms);
        self.applied_daemon_lifecycle = Some(self.requested_daemon_lifecycle);
        self.applied_daemon_mode = Some(self.requested_daemon_mode);
        self.applied_semantic_enabled = Some(self.requested_semantic_enabled);
        self.last_error = None;
    }

    fn load_failed(&mut self, error: anyhow::Error) {
        self.status = "failed";
        self.last_attempt_at_ms = utc_now().timestamp_millis();
        self.last_error = Some(format!("{error:#}"));
    }

    fn activation_failed(&mut self, error: anyhow::Error) {
        self.status = "activation_failed";
        self.applied_daemon_lifecycle = Some(self.requested_daemon_lifecycle);
        self.last_error = Some(format!("{error:#}"));
    }

    pub(super) fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "last_attempt_at_ms": self.last_attempt_at_ms,
            "last_applied_at_ms": self.last_applied_at_ms,
            "requested": {
                "daemon_lifecycle": self.requested_daemon_lifecycle.as_str(),
                "daemon_enabled": self.requested_daemon_lifecycle.starts_implicitly(),
                "daemon_mode": self.requested_daemon_mode.as_str(),
                "semantic_enabled": self.requested_semantic_enabled,
            },
            "applied": {
                "daemon_lifecycle": self.applied_daemon_lifecycle.map(DaemonLifecycle::as_str),
                "daemon_enabled": self.applied_daemon_lifecycle.map(DaemonLifecycle::starts_implicitly),
                "daemon_mode": self.applied_daemon_mode.map(DaemonMode::as_str),
                "semantic_enabled": self.applied_semantic_enabled,
            },
            "last_error": self.last_error,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonConfigReloadOutcome {
    Continue,
    StopDisabled,
}

pub(super) struct DaemonConfigReloadTargets<'a> {
    pub(super) query_service: &'a mut Option<DaemonQueryService>,
    pub(super) refresh_service: &'a mut Option<DaemonQueryService>,
    pub(super) state: &'a mut DaemonConfigReloadState,
}

pub(super) fn reload_daemon_runtime_config(
    data_root: &Path,
    args: &DaemonRunArgs,
    runtime: &mut DaemonRuntime,
    targets: DaemonConfigReloadTargets<'_>,
    wakeup: &Arc<DaemonWakeup>,
    lifecycle: &Arc<DaemonLifecycleState>,
    config_port: &'static dyn DaemonConfigPort,
) -> DaemonConfigReloadOutcome {
    let DaemonConfigReloadTargets {
        query_service,
        refresh_service,
        state: reload,
    } = targets;
    let config = match config_port.load(data_root) {
        Ok(config) => config,
        Err(error) => {
            reload.load_failed(error);
            return DaemonConfigReloadOutcome::Continue;
        }
    };
    reload.begin_attempt(&config);
    runtime.config = config;

    if !runtime.config.daemon.lifecycle.starts_implicitly() && !args.force {
        drop(query_service.take());
        drop(refresh_service.take());
        let _ = runtime.semantic_runtime.release_if_idle();
        reload.applied();
        return DaemonConfigReloadOutcome::StopDisabled;
    }

    let semantic_runtime_requested = daemon_semantic_runtime_requested(
        &runtime.config,
        semantic_query_service_supported() && daemon_query_service_transport_supported(),
        runtime.process_is_persistent(),
    );
    if daemon_query_service_transport_supported() && refresh_service.is_none() {
        let Some(source_refresh) = runtime.source_refresh_coordinator.as_ref().cloned() else {
            reload.activation_failed(anyhow!(
                "daemon source refresh engine was not recovered before IPC activation"
            ));
            return DaemonConfigReloadOutcome::Continue;
        };
        let handler = ctx_authenticated_request_handler_with_lifecycle(
            data_root,
            runtime.semantic_runtime.clone(),
            source_refresh,
            Arc::clone(wakeup),
            config_port,
            Arc::clone(lifecycle),
        );
        let started = start_daemon_source_refresh_service(data_root, handler, Arc::clone(wakeup));
        match started {
            Ok(service) => *refresh_service = Some(service),
            Err(error) => {
                reload.activation_failed(error);
                return DaemonConfigReloadOutcome::Continue;
            }
        }
    }
    if semantic_runtime_requested && query_service.is_none() {
        let Some(source_refresh) = runtime.source_refresh_coordinator.as_ref().cloned() else {
            reload.activation_failed(anyhow!(
                "daemon source refresh engine was not recovered before IPC activation"
            ));
            return DaemonConfigReloadOutcome::Continue;
        };
        let handler = ctx_authenticated_request_handler_with_lifecycle(
            data_root,
            runtime.semantic_runtime.clone(),
            source_refresh,
            Arc::clone(wakeup),
            config_port,
            Arc::clone(lifecycle),
        );
        match start_daemon_query_service(data_root, handler, Arc::clone(wakeup)) {
            Ok(service) => {
                *query_service = Some(service);
                // The query service thread keeps normal interactive priority.
                lower_semantic_worker_priority();
            }
            Err(error) => {
                reload.activation_failed(error);
                return DaemonConfigReloadOutcome::Continue;
            }
        }
    } else if !semantic_runtime_requested && query_service.is_some() {
        drop(query_service.take());
        let _ = runtime.semantic_runtime.release_if_idle();
    }

    reload.applied();
    DaemonConfigReloadOutcome::Continue
}

pub(super) fn daemon_semantic_runtime_requested(
    config: &AppConfig,
    service_supported: bool,
    process_is_persistent: bool,
) -> bool {
    service_supported
        && process_is_persistent
        && config.semantic_search_enabled()
        && !config.daemon.mode.runs_only_source_refresh()
}

pub(super) fn daemon_semantic_runtime_active(
    runtime: &DaemonRuntime,
    query_service: Option<&DaemonQueryService>,
) -> bool {
    query_service.is_some()
        && runtime.config.semantic_search_enabled()
        && semantic_query_service_supported()
}
