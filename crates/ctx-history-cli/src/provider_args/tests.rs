use std::collections::BTreeSet;

use super::*;

#[test]
fn provider_vocabulary_keeps_all_41_recognized_native_providers_importable() {
    let recognized = native_provider_cli_specs()
        .iter()
        .map(|spec| spec.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(recognized.len(), 41, "recognized provider count changed");
    let registered = ctx_history_capture::provider_source_specs()
        .iter()
        .map(|spec| spec.provider.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(recognized, registered, "provider vocabulary drifted");

    let importable = recognized
        .iter()
        .filter(|provider| parse_native_provider_name(provider).is_some_and(provider_is_importable))
        .collect::<BTreeSet<_>>();
    assert_eq!(importable.len(), 41, "importable provider count changed");
    assert!(provider_is_importable(CaptureProvider::Hermes));
    assert!(cli_supported_provider(CaptureProvider::Hermes));
}

#[test]
fn vocabulary_accepts_primary_storage_and_compatibility_names() {
    for spec in provider_cli_specs() {
        assert_eq!(
            parse_provider_name(spec.cli_name),
            Some(HistoryProvider::from(spec.provider)),
            "{} primary CLI name drifted",
            spec.cli_name
        );
        assert_eq!(
            parse_provider_name(spec.provider.as_str()),
            Some(HistoryProvider::from(spec.provider)),
            "{} storage name drifted",
            spec.provider.as_str()
        );
        for alias in spec.aliases {
            assert_eq!(
                parse_provider_name(alias),
                Some(HistoryProvider::from(spec.provider)),
                "{alias} compatibility alias drifted"
            );
        }
    }
    assert_eq!(
        parse_native_provider_name("custom"),
        None,
        "Custom remains a public-only provider"
    );
}

#[test]
fn mcp_names_include_primary_and_storage_names_without_duplicates() {
    let names = mcp_provider_names();
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(names.contains(&"kiro-cli"));
    assert!(names.contains(&"kiro_cli"));
    assert!(names.contains(&"custom"));
}

#[test]
fn unknown_provider_error_stays_compact() {
    assert_eq!(
        parse_provider("not-a-provider").unwrap_err(),
        compact_provider_error("not-a-provider")
    );
}
