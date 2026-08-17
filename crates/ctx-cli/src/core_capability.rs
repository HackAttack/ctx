//! Fixed, process-neutral Core capability endpoint used only by a verified
//! managed companion.  This is deliberately not a general command runner.

use std::{
    collections::BTreeSet,
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::ExitCode,
};

use anyhow::{anyhow, Context as _, Result};
use ctx_companion_bridge::{
    verify_signed_managed_pair_envelope, SignedManagedPairIdentity, SignedManagedPairTarget,
};
use ctx_history_cli::HistoryConfigPort;
use ctx_upgrade_engine::{
    ManagedPairComponentIdentity, ManagedPairEngine, ManagedPairTarget,
    ManagedPairTransactionStatus, ManagedPairVerifier, VerifiedManagedPairIdentity,
};
use serde_json::{json, Value};
#[cfg(test)]
use sha2::{Digest as _, Sha256};

const INVOCATION: &str = "--ctx-core-capability-v1";
const POST_EXIT_INVOCATION: &str = "--ctx-core-managed-pair-swap-v1";
const POST_EXIT_UNINSTALL_INVOCATION: &str = "--ctx-core-managed-pair-uninstall-v1";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 48 * 1024;
#[cfg(test)]
const API_INVENTORY: &str = r#"{"operations":{"CoreDoctor":{"request_keys":[],"response_keys":["facts"]},"CoreSetup":{"request_keys":["catalog_only","no_daemon","semantic","wait"],"response_keys":["facts","generation_id"]},"CoreStatus":{"request_keys":["usage"],"response_keys":["facts"]},"LocalUsageSummary":{"request_keys":[],"response_keys":["facts"]},"ManagedPairAbort":{"request_keys":["attempt_id"],"response_keys":["aborted"]},"ManagedPairBegin":{"request_keys":[],"response_keys":["attempt_id","candidate_root"]},"ManagedPairStage":{"request_keys":["attempt_id"],"response_keys":["attempt_id","release_name","rollback_generation","status"]},"ManagedPairStatus":{"request_keys":["attempt_id"],"response_keys":["status"]},"ManagedPairUninstall":{"request_keys":[],"response_keys":["attempt_id","cleanup_mode","status"]},"RefreshAndWait":{"request_keys":[],"response_keys":["facts","generation_id"]},"WakeCompanionMaintenance":{"request_keys":[],"response_keys":["accepted"]},"WakeRefresh":{"request_keys":[],"response_keys":["accepted"]}},"protocol":"ctx-core-capability","schema_version":1}"#;
pub(crate) const API_FINGERPRINT: &str =
    "e9338bb0508f3a506655b58df5cfe75a561fe73db14a633b15d2df8635acc860";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    CoreSetup,
    CoreStatus,
    CoreDoctor,
    LocalUsageSummary,
    RefreshAndWait,
    WakeRefresh,
    WakeCompanionMaintenance,
    ManagedPairBegin,
    ManagedPairStage,
    ManagedPairAbort,
    ManagedPairStatus,
    ManagedPairUninstall,
}

impl Operation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "CoreSetup" => Ok(Self::CoreSetup),
            "CoreStatus" => Ok(Self::CoreStatus),
            "CoreDoctor" => Ok(Self::CoreDoctor),
            "LocalUsageSummary" => Ok(Self::LocalUsageSummary),
            "RefreshAndWait" => Ok(Self::RefreshAndWait),
            "WakeRefresh" => Ok(Self::WakeRefresh),
            "WakeCompanionMaintenance" => Ok(Self::WakeCompanionMaintenance),
            "ManagedPairBegin" => Ok(Self::ManagedPairBegin),
            "ManagedPairStage" => Ok(Self::ManagedPairStage),
            "ManagedPairAbort" => Ok(Self::ManagedPairAbort),
            "ManagedPairStatus" => Ok(Self::ManagedPairStatus),
            "ManagedPairUninstall" => Ok(Self::ManagedPairUninstall),
            _ => Err(anyhow!("unknown operation")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::CoreSetup => "CoreSetup",
            Self::CoreStatus => "CoreStatus",
            Self::CoreDoctor => "CoreDoctor",
            Self::LocalUsageSummary => "LocalUsageSummary",
            Self::RefreshAndWait => "RefreshAndWait",
            Self::WakeRefresh => "WakeRefresh",
            Self::WakeCompanionMaintenance => "WakeCompanionMaintenance",
            Self::ManagedPairBegin => "ManagedPairBegin",
            Self::ManagedPairStage => "ManagedPairStage",
            Self::ManagedPairAbort => "ManagedPairAbort",
            Self::ManagedPairStatus => "ManagedPairStatus",
            Self::ManagedPairUninstall => "ManagedPairUninstall",
        }
    }
}

/// Intercepts only the fixed hidden invocation. Any spelling variation stays in
/// the ordinary public parser and receives no privileged transport.
pub(crate) fn intercept(arguments: &[std::ffi::OsString]) -> Option<ExitCode> {
    if arguments
        .get(1)
        .is_some_and(|value| value == POST_EXIT_INVOCATION)
    {
        return Some(match run_post_exit(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        });
    }
    if arguments
        .get(1)
        .is_some_and(|value| value == POST_EXIT_UNINSTALL_INVOCATION)
    {
        return Some(match run_post_exit_uninstall(arguments) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        });
    }
    if arguments.len() != 2 || arguments.get(1).is_none_or(|value| value != INVOCATION) {
        return None;
    }
    Some(match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    })
}

fn run() -> Result<()> {
    let request = parse_frame(read_frame()?)?;
    let response = execute(request)?;
    let bytes = canonical(&response)?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(anyhow!("response exceeds bound"));
    }
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

struct Request {
    data_root: PathBuf,
    operation: Operation,
    options: Options,
}

enum Options {
    Setup {
        catalog_only: bool,
        no_daemon: bool,
        semantic: bool,
        wait: bool,
    },
    Status {
        usage: Option<UsageAction>,
    },
    PairAttempt {
        attempt_id: String,
    },
    Empty,
}

#[derive(Clone, Copy)]
enum UsageAction {
    Enable,
    Disable,
    Reset,
}

fn parse_frame(bytes: Vec<u8>) -> Result<Request> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME_BYTES || bytes.contains(&0) {
        return Err(anyhow!("invalid frame bound"));
    }
    let text = std::str::from_utf8(&bytes).context("frame is not UTF-8")?;
    if text.contains('\n') || text.contains('\r') {
        return Err(anyhow!("frame is not one line"));
    }
    reject_duplicate_keys(text)?;
    let value: Value = serde_json::from_str(text).context("invalid JSON")?;
    if canonical(&value)? != bytes {
        return Err(anyhow!("frame is not canonical JSON"));
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("request is not an object"))?;
    exact_keys(
        object.keys().map(String::as_str),
        [
            "api_fingerprint",
            "data_root",
            "operation",
            "options",
            "schema_version",
        ],
    )?;
    if object.get("schema_version") != Some(&json!(1))
        || object.get("api_fingerprint").and_then(Value::as_str) != Some(API_FINGERPRINT)
    {
        return Err(anyhow!("protocol version or fingerprint mismatch"));
    }
    let root = object
        .get("data_root")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing data root"))?;
    let data_root = normalized_absolute_root(root)?;
    let operation = Operation::parse(
        object
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing operation"))?,
    )?;
    let options = object
        .get("options")
        .ok_or_else(|| anyhow!("missing options"))?;
    let options = parse_options(operation, options)?;
    Ok(Request {
        data_root,
        operation,
        options,
    })
}

fn execute(request: Request) -> Result<Value> {
    if !matches!(
        request.operation,
        Operation::LocalUsageSummary
            | Operation::ManagedPairBegin
            | Operation::ManagedPairStage
            | Operation::ManagedPairAbort
            | Operation::ManagedPairStatus
            | Operation::ManagedPairUninstall
    ) {
        crate::semantic::initialize()?;
    }
    let facts = match (request.operation, request.options) {
        (
            Operation::CoreSetup,
            Options::Setup {
                catalog_only,
                no_daemon,
                semantic,
                wait,
            },
        ) => core_setup_facts(&request.data_root, catalog_only, no_daemon, semantic, wait)?,
        (Operation::CoreStatus, Options::Status { usage }) => {
            core_status_facts(&request.data_root, usage)?
        }
        (Operation::CoreDoctor, Options::Empty) => {
            crate::commands::doctor::doctor_facts(&request.data_root)?
        }
        (Operation::LocalUsageSummary, Options::Empty) => {
            local_usage_summary_facts(&request.data_root)?
        }
        (Operation::RefreshAndWait, Options::Empty) => refresh_and_facts(&request.data_root)?,
        (Operation::WakeRefresh, Options::Empty) => {
            if let Ok(config) = crate::config::AppConfig::load(&request.data_root) {
                crate::semantic::maybe_autostart_daemon(
                    &request.data_root,
                    &config,
                    crate::DaemonTriggerCommandArg::Setup,
                );
            }
            json!({"accepted": true})
        }
        (Operation::WakeCompanionMaintenance, Options::Empty) => {
            let _ = crate::companion::wake_verified_private_maintenance(&request.data_root);
            json!({"accepted": true})
        }
        (Operation::ManagedPairBegin, Options::Empty) => {
            let verifier = CoreManagedPairVerifier::new()?;
            let attempt = managed_pair_engine()?.begin(&verifier)?;
            json!({
                "attempt_id": attempt.attempt_id(),
                "candidate_root": attempt.candidate_root(),
            })
        }
        (Operation::ManagedPairStage, Options::PairAttempt { attempt_id }) => {
            let verifier = CoreManagedPairVerifier::new()?;
            let prepared = managed_pair_engine()?.stage_attempt(&attempt_id, &verifier)?;
            json!({
                "attempt_id": prepared.attempt_id(),
                "release_name": prepared.identity().release_name(),
                "rollback_generation": prepared.identity().rollback_generation(),
                "status": "staged",
            })
        }
        (Operation::ManagedPairAbort, Options::PairAttempt { attempt_id }) => {
            json!({"aborted": managed_pair_engine()?.abort(&attempt_id)?})
        }
        (Operation::ManagedPairStatus, Options::PairAttempt { attempt_id }) => {
            let status = managed_pair_engine()?.status(&attempt_id)?;
            json!({"status": managed_pair_status_name(status)})
        }
        (Operation::ManagedPairUninstall, Options::Empty) => {
            let verifier = CoreManagedPairVerifier::new()?;
            let attempt = managed_pair_engine()?.prepare_uninstall(&verifier)?;
            json!({
                "attempt_id": attempt.attempt_id(),
                "cleanup_mode": if attempt.retry_or_reboot_may_be_required() {
                    "retry_or_reboot_required_if_running_core_is_locked"
                } else {
                    "post_exit"
                },
                "status": "armed",
            })
        }
        _ => return Err(anyhow!("operation options are inconsistent")),
    };
    Ok(json!({
        "api_fingerprint": API_FINGERPRINT,
        "facts": facts,
        "ok": true,
        "operation": request.operation.name(),
        "schema_version": 1,
    }))
}

fn status_facts(data_root: &Path) -> Result<Value> {
    let config = crate::config::AppConfig::load(data_root)?;
    let storage = crate::observability_composition::local_usage_storage_authority(data_root);
    let control =
        crate::observability_composition::usage_control_snapshot(config.local_usage.enabled);
    bounded_value(
        crate::commands::status::status_read_model_authorized(
            data_root, &config, &storage, &control,
        )?
        .report,
    )
}

fn local_usage_summary_facts(data_root: &Path) -> Result<Value> {
    let storage = crate::observability_composition::local_usage_storage_authority(data_root);
    let mut control =
        crate::observability_composition::LocalUsageControlAuthority::new(data_root.to_path_buf());
    bounded_value(serde_json::to_value(
        crate::local_usage::read_report_authorized(&storage, &control.snapshot(), false),
    )?)
}

fn core_status_facts(data_root: &Path, usage: Option<UsageAction>) -> Result<Value> {
    let usage_action = match usage {
        Some(UsageAction::Enable | UsageAction::Disable) => {
            let enabled = matches!(usage, Some(UsageAction::Enable));
            crate::config::set_local_usage_enabled(data_root, enabled)?;
            let control = crate::config::read_local_usage_control(data_root)?;
            Some(json!({
                "action": if enabled { "enable" } else { "disable" },
                "effective_enabled": control.effective_enabled,
                "environment_override": control.environment_override.as_str(),
                "persisted_enabled": control.persisted_enabled,
            }))
        }
        Some(UsageAction::Reset) => {
            let storage =
                crate::observability_composition::local_usage_storage_authority(data_root);
            let cleared = crate::local_usage::reset_authorized(&storage)?;
            Some(
                json!({"action": "reset", "store_state": if cleared { "cleared" } else { "missing" }}),
            )
        }
        None => None,
    };
    bounded_value(json!({
        "status": status_facts(data_root)?,
        "usage_action": usage_action,
    }))
}

fn core_setup_facts(
    data_root: &Path,
    catalog_only: bool,
    no_daemon: bool,
    semantic: bool,
    wait: bool,
) -> Result<Value> {
    let mut config = crate::config::AppConfig::load(data_root)?;
    if semantic && (!config.automatic_indexing_enabled() || no_daemon) {
        return Err(anyhow!("semantic setup requires automatic indexing"));
    }
    if semantic {
        crate::config::set_semantic_search_enabled(data_root, true)?;
        config = crate::config::AppConfig::load(data_root)?;
    }
    if config.semantic_search_enabled() && (!config.automatic_indexing_enabled() || no_daemon) {
        return Err(anyhow!(
            "configured semantic search requires automatic indexing"
        ));
    }
    crate::history_config::CliHistoryConfigAdapter::new(data_root, &mut config)
        .write_default_config()?;

    let daemon_requested = config.automatic_indexing_enabled() && !no_daemon;
    if daemon_requested {
        let _ = crate::semantic::autostart_daemon_for_setup_and_wait(
            data_root,
            &config,
            crate::DaemonTriggerCommandArg::Setup,
        )?;
    }
    let (published_generation, refresh_request) = if daemon_requested {
        core_setup_refresh(data_root, wait)
    } else {
        (
            None,
            json!({
                "daemon_available": false,
                "mode": if wait { "wait" } else { "background" },
                "reason": if no_daemon { "explicit_opt_out" } else { "daemon_disabled" },
                "status": "unavailable",
            }),
        )
    };
    let status = status_facts(data_root)?;
    let generation_id = setup_generation_id(published_generation, &status);
    bounded_value(json!({
        "deprecated_catalog_only_ignored": catalog_only,
        "daemon_requested": daemon_requested,
        "generation_id": generation_id,
        "refresh_request": refresh_request,
        "semantic_enabled": config.semantic_search_enabled(),
        "status": status,
        "wait": wait,
    }))
}

fn core_setup_refresh(data_root: &Path, wait: bool) -> (Option<String>, Value) {
    let mut effective_wait = wait;
    let mut progress = |_status: &crate::semantic::RefreshStatus| Ok(());
    let mode = if wait {
        crate::semantic::SourceBackedRefreshMode::Wait
    } else {
        crate::semantic::SourceBackedRefreshMode::Background
    };
    let mut result = crate::semantic::coordinate_setup_source_backed_refresh_with_progress(
        data_root,
        mode,
        &mut progress,
    );
    if result
        .as_ref()
        .is_err_and(|error| should_wait_for_fresh_empty_publication(wait, error))
    {
        effective_wait = true;
        result = crate::semantic::coordinate_setup_source_backed_refresh_with_progress(
            data_root,
            crate::semantic::SourceBackedRefreshMode::Wait,
            &mut progress,
        );
    }
    match result {
        Ok(observation) => {
            let generation_id = observation.pin.generation_id().to_owned();
            let receipt = observation
                .receipt
                .as_ref()
                .map(|receipt| receipt.to_json());
            (
                Some(generation_id.clone()),
                json!({
                    "daemon_available": observation.daemon_available,
                    "mode": if effective_wait { "wait" } else { "background" },
                    "published_generation": generation_id,
                    "reason": Value::Null,
                    "receipt": receipt,
                    "request_id": observation.request_id,
                    "source_count": observation.source_count,
                    "status": observation.status,
                }),
            )
        }
        Err(error) => {
            let pending = (!effective_wait)
                .then(|| {
                    error.downcast_ref::<crate::semantic::SourceBackedRefreshPendingPublication>()
                })
                .flatten();
            (
                None,
                json!({
                    "daemon_available": true,
                    "last_error": format!("{error:#}"),
                    "mode": if effective_wait { "wait" } else { "background" },
                    "reason": if pending.is_some() {
                        "refresh_queued_without_published_generation"
                    } else {
                        "refresh_failed"
                    },
                    "request_id": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::request_id),
                    "request_state": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::request_state),
                    "source_count": pending.map(crate::semantic::SourceBackedRefreshPendingPublication::source_count),
                    "status": if pending.is_some() { "pending" } else { "unavailable" },
                }),
            )
        }
    }
}

fn should_wait_for_fresh_empty_publication(wait: bool, error: &anyhow::Error) -> bool {
    !wait
        && error
            .downcast_ref::<crate::semantic::SourceBackedRefreshPendingPublication>()
            .is_some_and(|pending| pending.source_count() == 0)
}

fn setup_generation_id(published: Option<String>, status: &Value) -> Option<String> {
    published.or_else(|| {
        status["lexical"]["generation_id"]
            .as_str()
            .map(str::to_owned)
    })
}

fn refresh_and_facts(data_root: &Path) -> Result<Value> {
    let mut progress = |_status: &crate::semantic::RefreshStatus| Ok(());
    let observation = crate::semantic::coordinate_source_backed_refresh_with_progress(
        data_root,
        crate::semantic::SourceBackedRefreshMode::Wait,
        &mut progress,
    )?;
    let generation_id = observation.pin.generation_id().to_owned();
    let status = status_facts(data_root)?;
    bounded_value(json!({"generation_id": generation_id, "status": status}))
}

fn parse_options(operation: Operation, value: &Value) -> Result<Options> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("options are not an object"))?;
    let expected: &[&str] = match operation {
        Operation::CoreSetup => &["catalog_only", "no_daemon", "semantic", "wait"],
        Operation::CoreStatus => &["usage"],
        Operation::CoreDoctor
        | Operation::LocalUsageSummary
        | Operation::RefreshAndWait
        | Operation::WakeRefresh
        | Operation::WakeCompanionMaintenance
        | Operation::ManagedPairBegin
        | Operation::ManagedPairUninstall => &[],
        Operation::ManagedPairStage
        | Operation::ManagedPairAbort
        | Operation::ManagedPairStatus => &["attempt_id"],
    };
    exact_keys(object.keys().map(String::as_str), expected.iter().copied())?;
    match operation {
        Operation::CoreSetup => Ok(Options::Setup {
            catalog_only: required_bool(object, "catalog_only")?,
            no_daemon: required_bool(object, "no_daemon")?,
            semantic: required_bool(object, "semantic")?,
            wait: required_bool(object, "wait")?,
        }),
        Operation::CoreStatus => Ok(Options::Status {
            usage: match object.get("usage") {
                Some(Value::Null) => None,
                Some(Value::String(value)) if value == "enable" => Some(UsageAction::Enable),
                Some(Value::String(value)) if value == "disable" => Some(UsageAction::Disable),
                Some(Value::String(value)) if value == "reset" => Some(UsageAction::Reset),
                _ => return Err(anyhow!("status usage option is invalid")),
            },
        }),
        Operation::ManagedPairStage
        | Operation::ManagedPairAbort
        | Operation::ManagedPairStatus => Ok(Options::PairAttempt {
            attempt_id: object
                .get("attempt_id")
                .and_then(Value::as_str)
                .filter(|value| {
                    value.len() == 32
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                })
                .ok_or_else(|| anyhow!("managed-pair attempt ID is invalid"))?
                .to_owned(),
        }),
        _ => Ok(Options::Empty),
    }
}

struct CoreManagedPairVerifier {
    expectations: ctx_companion_bridge::ManagedPairExpectations,
}

impl CoreManagedPairVerifier {
    fn new() -> Result<Self> {
        let expectations = crate::companion::managed_pair_expectations()
            .map_err(|error| anyhow!("{}", error.code()))?;
        Ok(Self { expectations })
    }
}

impl ManagedPairVerifier for CoreManagedPairVerifier {
    fn verify_signed_envelope(
        &self,
        signed_envelope: &[u8],
    ) -> Result<VerifiedManagedPairIdentity> {
        let identity = verify_signed_managed_pair_envelope(&self.expectations, signed_envelope)
            .map_err(|error| anyhow!(error.to_string()))?;
        engine_identity(&identity)
    }
}

fn engine_identity(identity: &SignedManagedPairIdentity) -> Result<VerifiedManagedPairIdentity> {
    let target = match identity.target() {
        SignedManagedPairTarget::LinuxArm64 => ManagedPairTarget::LinuxArm64,
        SignedManagedPairTarget::LinuxX64 => ManagedPairTarget::LinuxX64,
        SignedManagedPairTarget::MacosArm64 => ManagedPairTarget::MacosArm64,
        SignedManagedPairTarget::MacosX64 => ManagedPairTarget::MacosX64,
        SignedManagedPairTarget::WindowsX64 => ManagedPairTarget::WindowsX64,
    };
    VerifiedManagedPairIdentity::new(
        identity.release_name(),
        target,
        identity.rollback_generation(),
        identity.manifest_sha256().to_hex(),
        ManagedPairComponentIdentity::new(
            identity.core().sha256().to_hex(),
            identity.core().size_bytes(),
        )?,
        ManagedPairComponentIdentity::new(
            identity.companion().sha256().to_hex(),
            identity.companion().size_bytes(),
        )?,
    )
}

fn managed_pair_engine() -> Result<ManagedPairEngine> {
    if !crate::companion::managed_pair_enabled() {
        return Err(anyhow!(
            "managed-pair capability is unavailable in a Core-only build"
        ));
    }
    let root = std::env::current_dir().context("resolve managed-pair install root")?;
    ManagedPairEngine::new(root)
}

fn managed_pair_status_name(status: ManagedPairTransactionStatus) -> &'static str {
    match status {
        ManagedPairTransactionStatus::Absent => "absent",
        ManagedPairTransactionStatus::Begun => "begun",
        ManagedPairTransactionStatus::Staging => "staging",
        ManagedPairTransactionStatus::Staged => "staged",
        ManagedPairTransactionStatus::Deferred => "deferred",
        ManagedPairTransactionStatus::Activating => "activating",
        ManagedPairTransactionStatus::Committed => "committed",
        ManagedPairTransactionStatus::Aborted => "aborted",
        ManagedPairTransactionStatus::Failed => "failed",
        ManagedPairTransactionStatus::RollingBack => "rolling_back",
    }
}

fn run_post_exit(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments.len() != 5 {
        return Err(anyhow!("invalid managed-pair post-exit invocation"));
    }
    let attempt_id = arguments[2]
        .to_str()
        .filter(|value| value.len() == 32)
        .ok_or_else(|| anyhow!("invalid managed-pair attempt ID"))?;
    let parent_pid = arguments[3]
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("invalid managed-pair parent PID"))?;
    let parent_creation_time = match arguments[4].to_str() {
        Some("-") => None,
        Some(value) => Some(
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("invalid managed-pair parent identity"))?,
        ),
        None => return Err(anyhow!("invalid managed-pair parent identity")),
    };
    managed_pair_engine()?.run_post_exit_swapper_after_parent_exit(
        attempt_id,
        &CoreManagedPairVerifier::new()?,
        parent_pid,
        parent_creation_time,
    )
}

fn run_post_exit_uninstall(arguments: &[std::ffi::OsString]) -> Result<()> {
    let (attempt_id, parent_pid, parent_creation_time) = post_exit_arguments(arguments)?;
    managed_pair_engine()?.run_post_exit_uninstall_after_parent_exit(
        attempt_id,
        parent_pid,
        parent_creation_time,
    )?;
    Ok(())
}

fn post_exit_arguments(arguments: &[std::ffi::OsString]) -> Result<(&str, u32, Option<u64>)> {
    if arguments.len() != 5 {
        return Err(anyhow!("invalid managed-pair post-exit invocation"));
    }
    let attempt_id = arguments[2]
        .to_str()
        .filter(|value| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        })
        .ok_or_else(|| anyhow!("invalid managed-pair attempt ID"))?;
    let parent_pid = arguments[3]
        .to_str()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| *value > 1)
        .ok_or_else(|| anyhow!("invalid managed-pair parent PID"))?;
    let parent_creation_time = match arguments[4].to_str() {
        Some("-") => None,
        Some(value) => Some(
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value != 0)
                .ok_or_else(|| anyhow!("invalid managed-pair parent identity"))?,
        ),
        None => return Err(anyhow!("invalid managed-pair parent identity")),
    };
    Ok((attempt_id, parent_pid, parent_creation_time))
}

fn required_bool(object: &serde_json::Map<String, Value>, key: &str) -> Result<bool> {
    object
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("setup option {key} must be boolean"))
}

fn normalized_absolute_root(value: &str) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || value.len() > 16 * 1024 || value.contains('\0') {
        return Err(anyhow!("data root must be bounded and absolute"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(anyhow!("data root must be lexically normalized"));
    }
    let normalized = std::fs::canonicalize(&path).unwrap_or(path.clone());
    if normalized != path {
        return Err(anyhow!("data root must already be normalized"));
    }
    Ok(path)
}

fn read_frame() -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take((MAX_FRAME_BYTES + 2) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.len() > MAX_FRAME_BYTES || bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(anyhow!("input has multiple frames or trailing data"));
    }
    Ok(bytes)
}

fn canonical(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(value).context("canonicalize JSON")
}

fn bounded_value(value: Value) -> Result<Value> {
    if canonical(&value)?.len() > MAX_RESPONSE_BYTES.saturating_sub(256) {
        return Err(anyhow!("Core facts exceed response bound"));
    }
    Ok(value)
}

fn exact_keys<'a>(
    actual: impl IntoIterator<Item = &'a str>,
    expected: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let actual = actual.into_iter().collect::<BTreeSet<_>>();
    let expected = expected.into_iter().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(anyhow!("request keys are not exact"));
    }
    Ok(())
}

/// serde_json accepts duplicate object names, so reject them before decoding.
/// This compact scanner only recognizes JSON structure and decoded string keys;
/// all syntax remains owned by serde_json afterwards.
fn reject_duplicate_keys(input: &str) -> Result<()> {
    fn string(bytes: &[u8], index: &mut usize) -> Result<String> {
        let start = *index;
        *index += 1;
        let mut escaped = false;
        while *index < bytes.len() {
            let byte = bytes[*index];
            *index += 1;
            if escaped {
                escaped = false;
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                continue;
            }
            if byte == b'"' {
                return serde_json::from_slice(&bytes[start..*index])
                    .map_err(|_| anyhow!("invalid JSON string"));
            }
            if byte < 0x20 {
                return Err(anyhow!("invalid JSON string"));
            }
        }
        Err(anyhow!("unterminated JSON string"))
    }
    fn value(bytes: &[u8], index: &mut usize) -> Result<()> {
        while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
            *index += 1;
        }
        match bytes.get(*index) {
            Some(b'{') => {
                *index += 1;
                let mut keys = BTreeSet::new();
                while bytes.get(*index) != Some(&b'}') {
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    if bytes.get(*index) != Some(&b'"') {
                        return Err(anyhow!("object key expected"));
                    }
                    let key = string(bytes, index)?;
                    if !keys.insert(key) {
                        return Err(anyhow!("duplicate JSON key"));
                    }
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    if bytes.get(*index) != Some(&b':') {
                        return Err(anyhow!("object colon expected"));
                    }
                    *index += 1;
                    value(bytes, index)?;
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    match bytes.get(*index) {
                        Some(b',') => *index += 1,
                        Some(b'}') => (),
                        _ => return Err(anyhow!("object delimiter expected")),
                    }
                }
                *index += 1;
                Ok(())
            }
            Some(b'[') => {
                *index += 1;
                while bytes.get(*index) != Some(&b']') {
                    value(bytes, index)?;
                    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
                        *index += 1;
                    }
                    match bytes.get(*index) {
                        Some(b',') => *index += 1,
                        Some(b']') => (),
                        _ => return Err(anyhow!("array delimiter expected")),
                    }
                }
                *index += 1;
                Ok(())
            }
            Some(b'"') => string(bytes, index).map(drop),
            Some(_) => {
                while bytes.get(*index).is_some_and(|byte| {
                    !matches!(byte, b',' | b']' | b'}') && !byte.is_ascii_whitespace()
                }) {
                    *index += 1;
                }
                Ok(())
            }
            None => Err(anyhow!("unexpected end of JSON")),
        }
    }
    let bytes = input.as_bytes();
    let mut index = 0;
    value(bytes, &mut index)?;
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    if index != bytes.len() {
        return Err(anyhow!("trailing JSON data"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_is_the_sha256_of_the_canonical_inventory() {
        assert_eq!(
            format!("{:x}", Sha256::digest(API_INVENTORY.as_bytes())),
            API_FINGERPRINT
        );
    }
    #[test]
    fn duplicates_and_multiframe_input_fail_closed() {
        assert!(reject_duplicate_keys(r#"{"a":1,"a":2}"#).is_err());
        assert!(parse_frame(b"{}\n{}".to_vec()).is_err());
    }
    #[test]
    fn only_the_exact_hidden_argv_is_intercepted() {
        assert!(intercept(&["ctx".into(), INVOCATION.into()]).is_some());
        assert!(intercept(&["ctx".into(), "--ctx-core-capability-v1=x".into()]).is_none());
    }

    #[test]
    fn capability_response_is_one_exact_flushed_json_frame() {
        let mut output = Vec::new();
        write_response_frame(&mut output, br#"{"ok":true}"#).unwrap();
        assert_eq!(output, b"{\"ok\":true}\n");
    }

    #[test]
    fn local_usage_summary_returns_canonical_config_error_without_aborting() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config.toml"),
            "[local_usage]\nenabled = unavailable\n",
        )
        .unwrap();

        let response = execute(Request {
            data_root: root.path().to_path_buf(),
            operation: Operation::LocalUsageSummary,
            options: Options::Empty,
        })
        .unwrap();

        assert_eq!(response["ok"], true);
        assert_eq!(response["operation"], "LocalUsageSummary");
        assert_eq!(
            response["facts"],
            serde_json::to_value(crate::local_usage::UsageReport::config_error()).unwrap()
        );
        assert!(!root.path().join("usage.sqlite").exists());
    }

    #[test]
    fn local_usage_summary_protocol_mismatches_remain_hard_failures() {
        let root = tempfile::tempdir().unwrap();
        let request = json!({
            "api_fingerprint": API_FINGERPRINT,
            "data_root": root.path(),
            "operation": "LocalUsageSummary",
            "options": {},
            "schema_version": 1,
        });
        assert!(parse_frame(canonical(&request).unwrap()).is_ok());

        let mut wrong_fingerprint = request.clone();
        wrong_fingerprint["api_fingerprint"] = json!("0".repeat(64));
        assert!(parse_frame(canonical(&wrong_fingerprint).unwrap()).is_err());

        let mut wrong_schema = request.clone();
        wrong_schema["schema_version"] = json!(2);
        assert!(parse_frame(canonical(&wrong_schema).unwrap()).is_err());

        let mut unknown_field = request;
        unknown_field["unexpected"] = json!(true);
        assert!(parse_frame(canonical(&unknown_field).unwrap()).is_err());
    }

    #[test]
    fn managed_setup_generation_is_optional_and_prefers_publication() {
        let no_generation = json!({"lexical": {"generation_id": null}});
        assert_eq!(setup_generation_id(None, &no_generation), None);

        let current = "1".repeat(64);
        let status = json!({"lexical": {"generation_id": current}});
        assert_eq!(setup_generation_id(None, &status), Some("1".repeat(64)));
        assert_eq!(
            setup_generation_id(Some("2".repeat(64)), &status),
            Some("2".repeat(64))
        );
    }

    #[test]
    fn managed_fresh_default_preserves_core_only_empty_publication_wait() {
        let empty: anyhow::Error = crate::semantic::SourceBackedRefreshPendingPublication::new(
            "fresh-empty".to_owned(),
            "queued".to_owned(),
            0,
        )
        .into();
        let nonempty: anyhow::Error = crate::semantic::SourceBackedRefreshPendingPublication::new(
            "fresh-nonempty".to_owned(),
            "queued".to_owned(),
            1,
        )
        .into();
        assert!(should_wait_for_fresh_empty_publication(false, &empty));
        assert!(!should_wait_for_fresh_empty_publication(true, &empty));
        assert!(!should_wait_for_fresh_empty_publication(false, &nonempty));
    }
}
