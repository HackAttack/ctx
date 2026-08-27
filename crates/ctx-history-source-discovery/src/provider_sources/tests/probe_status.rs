use ctx_history_core::CaptureProvider;

use super::super::probes::BoundedProbe;
#[cfg(unix)]
use super::super::ProviderSource;
use super::super::{ProviderDefaultLocation, ProviderSourceKind, ProviderSourceStatus};
use super::support::{tempdir, EnvGuard, ENV_LOCK};

fn default_location_import_probe(
    data_root: Option<&std::path::Path>,
    provider: CaptureProvider,
    location: &ProviderDefaultLocation,
    path: &std::path::Path,
) -> BoundedProbe {
    super::super::probes::default_location_import_probe(
        &super::super::TEST_PROVIDER_PROBES,
        data_root,
        provider,
        location,
        path,
    )
}

#[cfg(unix)]
fn discover_provider_sources(home: &std::path::Path) -> Vec<ProviderSource> {
    super::super::discover_provider_sources(&super::super::TEST_PROVIDER_PROBES, home)
}

#[cfg(unix)]
fn discover_provider_sources_for_provider_report(
    home: &std::path::Path,
    provider: CaptureProvider,
) -> super::super::DiscoveryReport {
    super::super::discover_provider_sources_for_provider_report(
        &super::super::TEST_PROVIDER_PROBES,
        home,
        provider,
    )
}

#[test]
fn codex_nested_probe_reports_budget_exhaustion_as_explicit_unknown() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
    let sessions = temp.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    for index in 0..10_001 {
        std::fs::create_dir(sessions.join(format!("partition-{index:05}"))).unwrap();
    }

    let report = super::super::discover_provider_sources_for_provider_report(
        &super::super::TEST_PROVIDER_PROBES,
        temp.path(),
        CaptureProvider::Codex,
    );
    let source = report
        .sources
        .iter()
        .find(|source| source.path == sessions)
        .expect("bounded Codex source remains visible as incomplete");
    assert_eq!(source.status, ProviderSourceStatus::Unknown);
    assert_eq!(
        source.unsupported_reason,
        Some("path exists but the Codex session transcript probe hit its scan budget")
    );
}

#[test]
fn default_location_probe_does_not_fallback_to_path_existence_for_unhandled_providers() {
    let temp = tempdir();
    let existing = temp.path().join("shell-history");
    std::fs::write(&existing, "{}\n").unwrap();
    let location = ProviderDefaultLocation {
        path_components: &["shell-history"],
        source_format: "shell_history",
        source_kind: ProviderSourceKind::NativeHistory,
    };

    assert_eq!(
        default_location_import_probe(None, CaptureProvider::Shell, &location, &existing),
        BoundedProbe::NotFound
    );
}

#[cfg(unix)]
#[test]
fn default_source_probe_reports_unreadable_directory_as_unknown() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
    let sessions = temp.path().join(".codex/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let original_permissions = std::fs::metadata(&sessions).unwrap().permissions();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000)).unwrap();

    if std::fs::read_dir(&sessions).is_ok() {
        std::fs::set_permissions(&sessions, original_permissions).unwrap();
        return;
    }

    let report = discover_provider_sources_for_provider_report(temp.path(), CaptureProvider::Codex);
    std::fs::set_permissions(&sessions, original_permissions).unwrap();

    assert!(!report.sources.iter().any(|source| source.path == sessions));
    assert!(report.issues.iter().any(|issue| {
        issue.provider == CaptureProvider::Codex
            && issue.path.as_deref() == Some(sessions.as_path())
            && issue.reason.contains("access was denied")
    }));
}

#[cfg(unix)]
#[test]
fn default_source_probe_skips_unreadable_child_directory() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir();
    let _codex_home = EnvGuard::remove("CODEX_HOME");
    let sessions = temp.path().join(".codex/sessions");
    let readable = sessions.join("readable");
    let unreadable = sessions.join("unreadable");
    std::fs::create_dir_all(&readable).unwrap();
    std::fs::create_dir_all(&unreadable).unwrap();
    std::fs::write(readable.join("session.jsonl"), "{}\n").unwrap();

    let original_permissions = std::fs::metadata(&unreadable).unwrap().permissions();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    if std::fs::read_dir(&unreadable).is_ok() {
        std::fs::set_permissions(&unreadable, original_permissions).unwrap();
        return;
    }

    let source = discover_provider_sources(temp.path())
        .into_iter()
        .find(|source| {
            source.provider == CaptureProvider::Codex
                && source.source_format == "codex_session_jsonl_tree"
        });
    std::fs::set_permissions(&unreadable, original_permissions).unwrap();

    let source = source.unwrap();
    assert_eq!(source.status, ProviderSourceStatus::Available);
    assert_eq!(source.unsupported_reason, None);
}
