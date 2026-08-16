use std::convert::Infallible;

use serde_json::{json, Value};

use super::*;

fn collect(
    raw: &Value,
    limit: usize,
) -> (
    Vec<(u64, FileReferenceDraft)>,
    ProviderFileReferenceVisitOutcome,
) {
    let mut drafts = Vec::new();
    let outcome = visit_provider_file_reference_drafts_with_limit(raw, limit, |draft| {
        drafts.push(draft);
        Ok::<(), Infallible>(())
    })
    .expect("an infallible draft sink cannot fail");
    (drafts, outcome)
}

#[test]
fn structured_literals_preserve_exact_values_source_order_and_duplicates() {
    let raw = json!([
        {"file_path": "./Src/../src/lib.rs"},
        {"uri": "file:///Work/CTX/src/lib.rs"},
        {"file_path": "./Src/../src/lib.rs"}
    ]);
    let (drafts, outcome) = collect(&raw, MAX_PROVIDER_FILE_REFERENCES_PER_EVENT);

    assert_eq!(outcome.emitted(), 3);
    assert!(!outcome.limit_exceeded());
    assert_eq!(drafts[0].0, 0);
    assert_eq!(drafts[0].1.kind, LiteralFactKind::File);
    assert_eq!(drafts[0].1.value, "./Src/../src/lib.rs");
    assert_eq!(drafts[0].1.native_field, "file_path");
    assert_eq!(drafts[1].1.kind, LiteralFactKind::Url);
    assert_eq!(drafts[1].1.value, "file:///Work/CTX/src/lib.rs");
    assert_eq!(drafts[0].1, drafts[2].1);
}

#[test]
fn patch_text_and_operation_words_do_not_create_or_classify_references() {
    let raw = json!({
        "command": "delete create rename modify",
        "patch": "*** Begin Patch\n*** Delete File: src/removed.rs\n*** End Patch",
        "path": "src/literal.rs"
    });
    let (drafts, outcome) = collect(&raw, MAX_PROVIDER_FILE_REFERENCES_PER_EVENT);

    assert_eq!(outcome.emitted(), 1);
    assert_eq!(drafts[0].1.value, "src/literal.rs");
    assert_eq!(drafts[0].1.kind, LiteralFactKind::File);
}

#[test]
fn invented_and_oversized_fields_are_ignored_without_path_heuristics() {
    let raw_key = format!("{}path", "!".repeat(MAX_PROVIDER_FIELD_NAME_BYTES + 1));
    let raw = json!({
        raw_key: "src/ignored.rs",
        "repository_binding": "src/not-a-file.rs",
        "confidence": "high",
        "effect": "modified",
        "filename": "literal-without-extension",
        "file": "x".repeat(MAX_PROVIDER_LITERAL_VALUE_BYTES + 1)
    });
    let (drafts, _) = collect(&raw, MAX_PROVIDER_FILE_REFERENCES_PER_EVENT);

    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].1.value, "literal-without-extension");
}

#[test]
fn traversal_stops_at_the_exact_reference_limit() {
    let raw = json!([
        {"path": "one"},
        {"path": "two"},
        {"path": "three"}
    ]);
    let (drafts, outcome) = collect(&raw, 2);

    assert_eq!(drafts.len(), 2);
    assert_eq!(outcome.emitted(), 2);
    assert!(outcome.limit_exceeded());
}
