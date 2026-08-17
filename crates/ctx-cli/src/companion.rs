use std::{
    ffi::{OsStr, OsString},
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::OnceLock,
    time::Duration,
};

use ctx_companion_bridge::{
    BridgeError, BridgeLimits, CancellationToken, CompanionBridge, CompanionRequest,
    CompatibilityIdentity, CoreBuildIdentity, EnvironmentKey, ExitClass, LimitConfiguration,
    ManagedPairExpectations, ReleaseChannel, Sha256Digest, TerminationReason, MAX_ADMISSION_WAIT,
    MAX_ARGUMENTS, MAX_CAPTURED_WALL_TIME, MAX_CONCURRENT_PROCESSES, MAX_CONTROL_BYTES,
    MAX_ENVIRONMENT_ENTRIES, MAX_STDERR_BYTES,
};
use serde_json::json;

const MCP_PROXY_ARGUMENTS: [&str; 2] = ["mcp", "serve"];
const MAINTENANCE_ARGUMENT: &str = "--ctx-pro-maintenance-v1";
const MAINTENANCE_RECEIPT: &[u8] = b"{\"accepted\":true,\"schema_version\":1}\n";
const MCP_PROXY_MAX_BYTES: usize = 1024 * 1024;
const FORWARDED_ENVIRONMENT: [(EnvironmentKey, &str); 6] = [
    (EnvironmentKey::Lang, "LANG"),
    (EnvironmentKey::LcAll, "LC_ALL"),
    (EnvironmentKey::TimeZone, "TZ"),
    (
        EnvironmentKey::DbusSessionBusAddress,
        "DBUS_SESSION_BUS_ADDRESS",
    ),
    (EnvironmentKey::XdgRuntimeDir, "XDG_RUNTIME_DIR"),
    (EnvironmentKey::LocalUsageEnabled, "CTX_LOCAL_USAGE_ENABLED"),
];
static COMPANION_CANCELLATION: OnceLock<Result<CancellationToken, ()>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompanionRouteError {
    Unavailable,
    Incompatible,
}

impl CompanionRouteError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "companion_unavailable",
            Self::Incompatible => "companion_incompatible",
        }
    }

    pub(crate) const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

pub(crate) fn forward_paid_cli_if_selected(arguments: Vec<OsString>) -> Option<ExitCode> {
    let mut forwarded = paid_family_arguments(&arguments)?;
    let result = effective_data_root(&arguments).and_then(|root| {
        bind_forwarded_data_root(&mut forwarded, &root);
        forward_paid_cli(forwarded, root)
    });
    Some(match result {
        Ok(exit) => exit,
        Err(error) => {
            write_cli_route_error(error);
            ExitCode::FAILURE
        }
    })
}

pub(crate) fn proxy_paid_mcp(
    request_line: &[u8],
    data_root: &Path,
) -> Result<Vec<u8>, CompanionRouteError> {
    if request_line.len() > MCP_PROXY_MAX_BYTES {
        return Err(CompanionRouteError::Unavailable);
    }
    let output = launch_companion_captured(
        MCP_PROXY_ARGUMENTS.iter().map(OsString::from).collect(),
        request_line.to_vec(),
        absolute_data_root(data_root.to_path_buf())?,
        mcp_limits()?,
    )?;
    write_companion_stderr(output.stderr()).map_err(|_| CompanionRouteError::Unavailable)?;
    if matches!(
        output.exit_class(),
        ExitClass::Terminated(TerminationReason::Cancelled)
    ) {
        return Err(CompanionRouteError::Unavailable);
    }
    if output.stdout_truncated()
        || output.stderr_truncated()
        || output.exit_class() != ExitClass::Success
        || !is_one_framed_line(output.stdout())
    {
        return Err(CompanionRouteError::Incompatible);
    }
    Ok(output.stdout().to_vec())
}

pub(crate) fn wake_verified_private_maintenance(
    data_root: &Path,
) -> Result<(), CompanionRouteError> {
    let limits = BridgeLimits::new(LimitConfiguration {
        control_bytes: MAX_CONTROL_BYTES,
        input_bytes: 1,
        stdout_bytes: 256,
        stderr_bytes: 4 * 1024,
        arguments: 1,
        environment_entries: 1,
        concurrent_processes: MAX_CONCURRENT_PROCESSES,
        admission_wait: Duration::from_secs(5),
        captured_wall_time: Duration::from_secs(30),
    })
    .map_err(|_| CompanionRouteError::Incompatible)?;
    let expectations = managed_pair_expectations()?;
    let mut request = CompanionRequest::new(absolute_data_root(data_root.to_path_buf())?);
    request.push_argument(MAINTENANCE_ARGUMENT);
    let request = request.capture(Vec::new());
    let output = CompanionBridge::new(limits)
        .launch_captured(&expectations, request, companion_cancellation()?)
        .map_err(classify_bridge_error)?;
    if output.exit_class() != ExitClass::Success
        || output.stdout_truncated()
        || output.stderr_truncated()
        || output.stdout() != MAINTENANCE_RECEIPT
        || !output.stderr().is_empty()
    {
        return Err(CompanionRouteError::Incompatible);
    }
    Ok(())
}

fn forward_paid_cli(
    arguments: Vec<OsString>,
    data_root: PathBuf,
) -> Result<ExitCode, CompanionRouteError> {
    let expectations = managed_pair_expectations()?;
    let request = companion_request(arguments, data_root);
    let exit = CompanionBridge::new(BridgeLimits::default())
        .launch_streaming(&expectations, request, companion_cancellation()?)
        .map_err(classify_bridge_error)?;
    Ok(exit_code(exit.exit_class()))
}

fn launch_companion_captured(
    arguments: Vec<OsString>,
    stdin: Vec<u8>,
    data_root: PathBuf,
    limits: BridgeLimits,
) -> Result<ctx_companion_bridge::CompanionOutput, CompanionRouteError> {
    let expectations = managed_pair_expectations()?;
    let request = companion_request(arguments, data_root).capture(stdin);
    CompanionBridge::new(limits)
        .launch_captured(&expectations, request, companion_cancellation()?)
        .map_err(classify_bridge_error)
}

fn companion_request(arguments: Vec<OsString>, data_root: PathBuf) -> CompanionRequest {
    let mut request = CompanionRequest::new(data_root);
    for argument in arguments {
        request.push_argument(argument);
    }
    for (key, name) in FORWARDED_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            request.environment_mut().set(key, value);
        }
    }
    request
}

fn companion_cancellation() -> Result<&'static CancellationToken, CompanionRouteError> {
    COMPANION_CANCELLATION
        .get_or_init(|| {
            let cancellation = CancellationToken::new();
            let trigger = cancellation.clone();
            ctrlc::set_handler(move || trigger.cancel())
                .map(|()| cancellation)
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|()| CompanionRouteError::Unavailable)
}

pub(crate) fn managed_pair_expectations() -> Result<ManagedPairExpectations, CompanionRouteError> {
    let channel = match option_env!("CTX_MANAGED_PAIR_CHANNEL") {
        Some("stable") => ReleaseChannel::Stable,
        Some("staging") => ReleaseChannel::Staging,
        Some(_) => return Err(CompanionRouteError::Incompatible),
        None => return Err(CompanionRouteError::Unavailable),
    };
    let source_revision = required_build_value(option_env!("CTX_RELEASE_BUILD_SOURCE_COMMIT"))?;
    let invocation_fingerprint =
        required_digest(option_env!("CTX_MANAGED_PAIR_INVOCATION_FINGERPRINT"))?;
    let core_capability_fingerprint =
        required_digest(option_env!("CTX_MANAGED_PAIR_CORE_CAPABILITY_FINGERPRINT"))?;
    let core =
        CoreBuildIdentity::new(source_revision).map_err(|_| CompanionRouteError::Incompatible)?;
    Ok(ManagedPairExpectations::new(
        channel,
        core,
        CompatibilityIdentity::new(invocation_fingerprint, core_capability_fingerprint),
    ))
}

fn required_build_value(value: Option<&'static str>) -> Result<&'static str, CompanionRouteError> {
    value.ok_or(CompanionRouteError::Unavailable)
}

fn required_digest(value: Option<&'static str>) -> Result<Sha256Digest, CompanionRouteError> {
    let value = required_build_value(value)?;
    Sha256Digest::from_hex(value).map_err(|_| CompanionRouteError::Incompatible)
}

fn mcp_limits() -> Result<BridgeLimits, CompanionRouteError> {
    BridgeLimits::new(LimitConfiguration {
        control_bytes: MAX_CONTROL_BYTES,
        input_bytes: MCP_PROXY_MAX_BYTES,
        stdout_bytes: MCP_PROXY_MAX_BYTES,
        stderr_bytes: MAX_STDERR_BYTES,
        arguments: MAX_ARGUMENTS,
        environment_entries: MAX_ENVIRONMENT_ENTRIES,
        concurrent_processes: MAX_CONCURRENT_PROCESSES,
        admission_wait: MAX_ADMISSION_WAIT,
        captured_wall_time: MAX_CAPTURED_WALL_TIME,
    })
    .map_err(|_| CompanionRouteError::Incompatible)
}

fn paid_family_arguments(arguments: &[OsString]) -> Option<Vec<OsString>> {
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        if is_global_help_or_version(argument) {
            return None;
        }
        if argument == "--" {
            index += 1;
            break;
        }
        if argument == "--data-root" || argument == "--color" {
            index = index.saturating_add(2);
            continue;
        }
        if argument == "--quiet"
            || has_attached_global_value(argument)
            || starts_with_dash(argument)
        {
            index += 1;
            continue;
        }
        if ["pro", "blame", "referral"]
            .iter()
            .any(|family| argument == family)
        {
            return Some(arguments[1..].to_vec());
        }
        if managed_pair_enabled()
            && ["setup", "status", "doctor", "upgrade", "uninstall"]
                .iter()
                .any(|family| argument == family)
        {
            if argument == "upgrade"
                && has_hosted_transaction_control(&arguments[index.saturating_add(1)..])
            {
                return None;
            }
            return Some(arguments[1..].to_vec());
        }
        if argument == "help"
            && arguments.get(index + 1).is_some_and(|candidate| {
                [
                    "pro",
                    "blame",
                    "referral",
                    "setup",
                    "status",
                    "doctor",
                    "upgrade",
                    "uninstall",
                ]
                .iter()
                .any(|family| {
                    candidate == family
                        && (managed_pair_enabled()
                            || !matches!(
                                *family,
                                "setup" | "status" | "doctor" | "upgrade" | "uninstall"
                            ))
                })
            })
        {
            return Some(arguments[1..].to_vec());
        }
        return None;
    }
    arguments.get(index).and_then(|argument| {
        ["pro", "blame", "referral"]
            .iter()
            .any(|family| argument == family)
            .then(|| arguments[1..].to_vec())
    })
}

fn has_hosted_transaction_control(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .take_while(|argument| argument.as_os_str() != OsStr::new("--"))
        .any(|argument| {
            argument == "--hosted-transaction"
                || argument
                    .as_encoded_bytes()
                    .starts_with(b"--hosted-transaction=")
        })
}

pub(crate) fn managed_pair_enabled() -> bool {
    matches!(
        option_env!("CTX_MANAGED_PAIR_CHANNEL"),
        Some("stable" | "staging")
    )
}

fn is_global_help_or_version(value: &OsStr) -> bool {
    matches!(value.to_str(), Some("-h" | "--help" | "-V" | "--version"))
}

fn effective_data_root(arguments: &[OsString]) -> Result<PathBuf, CompanionRouteError> {
    let explicit = explicit_data_root(arguments);
    let selected = match explicit {
        Some(path) if !path.as_os_str().is_empty() => path,
        _ => match std::env::var_os("CTX_DATA_ROOT").filter(|value| !value.is_empty()) {
            Some(path) => PathBuf::from(path),
            None => ctx_history_platform::default_data_root()
                .map_err(|_| CompanionRouteError::Unavailable)?,
        },
    };
    absolute_data_root(selected)
}

fn explicit_data_root(arguments: &[OsString]) -> Option<PathBuf> {
    let mut index = 1;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            return None;
        }
        if argument == "--data-root" {
            return arguments.get(index + 1).cloned().map(PathBuf::from);
        }
        if let Some(value) = data_root_equals_value(argument) {
            return Some(PathBuf::from(value));
        }
        index += 1;
    }
    None
}

fn absolute_data_root(path: PathBuf) -> Result<PathBuf, CompanionRouteError> {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|_| CompanionRouteError::Unavailable)?
    };
    if absolute.as_os_str().as_encoded_bytes().len() > 16 * 1024
        || absolute.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(CompanionRouteError::Incompatible);
    }
    match std::fs::canonicalize(&absolute) {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(absolute),
        Err(_) => Err(CompanionRouteError::Unavailable),
    }
}

fn bind_forwarded_data_root(arguments: &mut Vec<OsString>, data_root: &Path) {
    let mut bound = Vec::with_capacity(arguments.len().saturating_add(1));
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            bound.extend(arguments[index..].iter().cloned());
            break;
        }
        if argument == "--data-root" {
            bound.push(argument.clone());
            if arguments.get(index + 1).is_none() {
                index += 1;
                continue;
            }
            bound.push(data_root.as_os_str().to_owned());
            index = index.saturating_add(2);
            continue;
        }
        if data_root_equals_value(argument).is_some() {
            bound.push(OsString::from("--data-root"));
            bound.push(data_root.as_os_str().to_owned());
        } else {
            bound.push(argument.clone());
        }
        index += 1;
    }
    *arguments = bound;
}

#[cfg(unix)]
fn data_root_equals_value(argument: &OsStr) -> Option<OsString> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    argument
        .as_bytes()
        .strip_prefix(b"--data-root=")
        .map(|value| OsString::from_vec(value.to_vec()))
}

#[cfg(windows)]
fn data_root_equals_value(argument: &OsStr) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

    let encoded = argument.encode_wide().collect::<Vec<_>>();
    let prefix = "--data-root=".encode_utf16().collect::<Vec<_>>();
    encoded
        .strip_prefix(prefix.as_slice())
        .map(OsString::from_wide)
}

#[cfg(not(any(unix, windows)))]
fn data_root_equals_value(argument: &OsStr) -> Option<OsString> {
    argument
        .to_str()
        .and_then(|value| value.strip_prefix("--data-root="))
        .map(OsString::from)
}

fn has_attached_global_value(value: &OsStr) -> bool {
    let bytes = value.as_encoded_bytes();
    bytes.starts_with(b"--data-root=") || bytes.starts_with(b"--color=")
}

fn starts_with_dash(value: &OsStr) -> bool {
    value.as_encoded_bytes().starts_with(b"-")
}

fn is_one_framed_line(bytes: &[u8]) -> bool {
    bytes.last() == Some(&b'\n') && !bytes[..bytes.len().saturating_sub(1)].contains(&b'\n')
}

fn classify_bridge_error(error: BridgeError) -> CompanionRouteError {
    match error {
        BridgeError::InvalidSlot(_) | BridgeError::Verification(_) => {
            CompanionRouteError::Incompatible
        }
        BridgeError::Filesystem { .. }
        | BridgeError::Limit(_)
        | BridgeError::InvalidDataRoot
        | BridgeError::QueueTimeout
        | BridgeError::CancelledBeforeSpawn
        | BridgeError::Spawn(_)
        | BridgeError::Transport(_)
        | BridgeError::WorkerFailed
        | BridgeError::UnsupportedPlatform => CompanionRouteError::Unavailable,
    }
}

fn exit_code(exit: ExitClass) -> ExitCode {
    let code = match exit {
        ExitClass::Success => 0,
        ExitClass::Code(code) => u8::try_from(code).unwrap_or(1),
        #[cfg(unix)]
        ExitClass::Signal(signal) => u8::try_from(128_i32.saturating_add(signal)).unwrap_or(1),
        ExitClass::Terminated(TerminationReason::Cancelled) => 130,
        ExitClass::UnknownFailure | ExitClass::Terminated(_) => 1,
    };
    ExitCode::from(code)
}

fn write_companion_stderr(bytes: &[u8]) -> std::io::Result<()> {
    std::io::stderr().write_all(bytes)?;
    std::io::stderr().flush()
}

fn write_cli_route_error(error: CompanionRouteError) {
    let code = error.code();
    let document = json!({
        "error": code,
        "error_code": code,
        "retryable": error.retryable(),
    });
    let _ = writeln!(std::io::stderr(), "{document}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paid_gate_forwards_the_original_arguments_without_paid_parsing() {
        let arguments = [
            OsString::from("ctx"),
            OsString::from("--data-root"),
            OsString::from("opaque-root"),
            OsString::from("blame"),
            OsString::from("--private-option"),
            OsString::from("opaque-value"),
        ];
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec())
        );
    }

    #[test]
    fn core_routes_never_enter_the_companion_gate() {
        for family in ["setup", "status", "doctor", "search", "show", "mcp"] {
            let arguments = [OsString::from("ctx"), OsString::from(family)];
            assert!(paid_family_arguments(&arguments).is_none(), "{family}");
        }
    }

    #[test]
    fn lifecycle_routing_is_managed_build_only() {
        // Source and cargo-install builds must keep Core lifecycle commands.
        if !managed_pair_enabled() {
            for family in ["setup", "status", "doctor"] {
                assert!(
                    paid_family_arguments(&[OsString::from("ctx"), OsString::from(family)])
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn hosted_install_transactions_remain_core_owned() {
        for arguments in [
            vec!["--hosted-transaction", "install"],
            vec!["--hosted-transaction=install"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert!(has_hosted_transaction_control(&arguments));

            if managed_pair_enabled() {
                let full = [
                    vec![OsString::from("ctx"), OsString::from("upgrade")],
                    arguments,
                ]
                .concat();
                assert!(paid_family_arguments(&full).is_none());
            }
        }
        assert!(!has_hosted_transaction_control(&[
            OsString::from("--"),
            OsString::from("--hosted-transaction=install"),
        ]));
    }

    #[test]
    fn forwarded_environment_is_the_complete_typed_allowlist() {
        assert_eq!(FORWARDED_ENVIRONMENT.len(), MAX_ENVIRONMENT_ENTRIES);
        assert!(FORWARDED_ENVIRONMENT
            .contains(&(EnvironmentKey::LocalUsageEnabled, "CTX_LOCAL_USAGE_ENABLED")));
    }

    #[test]
    fn global_help_and_version_never_enter_the_companion_gate() {
        for option in ["-h", "--help", "-V", "--version"] {
            for family in ["pro", "blame", "referral"] {
                let arguments = [
                    OsString::from("ctx"),
                    OsString::from(option),
                    OsString::from(family),
                ];
                assert!(
                    paid_family_arguments(&arguments).is_none(),
                    "{option} {family}"
                );
            }
        }
    }

    #[test]
    fn subcommand_help_and_help_alias_route_to_the_companion() {
        for arguments in [
            vec!["ctx", "pro", "--help"],
            vec!["ctx", "blame", "--help"],
            vec!["ctx", "referral", "--help"],
            vec!["ctx", "help", "pro"],
        ] {
            let arguments = arguments
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(
                paid_family_arguments(&arguments),
                Some(arguments[1..].to_vec())
            );
        }
    }

    #[test]
    fn explicit_relative_data_root_is_made_absolute_without_changing_argv() {
        let arguments = [
            OsString::from("ctx"),
            OsString::from("--data-root=relative-root"),
            OsString::from("pro"),
        ];
        let root = effective_data_root(&arguments).unwrap();
        assert!(root.is_absolute());
        assert!(root.ends_with("relative-root"));
        assert_eq!(
            paid_family_arguments(&arguments),
            Some(arguments[1..].to_vec())
        );
    }

    #[test]
    fn data_root_options_after_delimiter_are_positional() {
        for trailing in [
            vec!["--data-root", "/private/positional"],
            vec!["--data-root=/private/positional"],
        ] {
            let mut arguments = vec![
                OsString::from("ctx"),
                OsString::from("pro"),
                OsString::from("--"),
            ];
            arguments.extend(trailing.into_iter().map(OsString::from));
            assert_eq!(explicit_data_root(&arguments), None, "{arguments:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn opaque_paid_arguments_are_preserved_byte_for_byte() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let opaque = OsString::from_vec(vec![b'v', 0xff, b'x']);
        let arguments = [
            OsString::from("ctx"),
            OsString::from("referral"),
            opaque.clone(),
        ];
        let forwarded = paid_family_arguments(&arguments).unwrap();
        assert_eq!(
            forwarded[1].as_os_str().as_bytes(),
            opaque.as_os_str().as_bytes()
        );
    }

    #[test]
    fn mcp_response_must_be_one_opaque_framed_line() {
        assert!(is_one_framed_line(b"{\"jsonrpc\":\"2.0\"}\n"));
        assert!(!is_one_framed_line(b"{}"));
        assert!(!is_one_framed_line(b"{}\n{}\n"));
    }
}
