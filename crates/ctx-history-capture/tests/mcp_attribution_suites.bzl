"""Authoritative public MCP attribution executable capabilities."""

MCP_ATTRIBUTION_EVIDENCE_CLASSES = [
    "ambiguity_duplicate_linkage",
    "canonical_terminal_outcomes",
    "exact_boundary",
    "exact_positive_pair",
    "malformed_identity",
    "max_plus_one",
    "privacy_sinks",
    "result_preservation",
    "search_nonindexing",
    "stable_ids",
]

# Every public suite alias is bound to one physical Bazel target and the exact
# test functions/classes it may claim. Target identity is passed into the
# runner so aliases cannot manufacture additional evidence identities.
# Closed suites must claim the binary's complete `--list` inventory. Selected
# aliases bind named tests in a larger existing Rust target; the runner proves
# each name exists and executes every claimed test with libtest `--exact`.
MCP_ATTRIBUTION_PUBLIC_SUITES = {
    "mcp_attribution_codex_provider_units": struct(
        target = "//crates/ctx-history-provider-codex:unit_tests",
        selected_inventory = True,
        tests = {
            "codex::nativepath::rows::tests::duplicate_selectors_withhold_linkage_and_preserve_raw_fact_order": ["ambiguity_duplicate_linkage"],
            "codex::nativepath::rows::tests::empty_result_string_is_absent_text_with_exact_structured_capture": ["result_preservation"],
            "codex::nativepath::rows::tests::mcp_terminal_activity_preserves_exact_server_tool_and_linkage": ["exact_positive_pair"],
        },
    ),
    "mcp_attribution_native_jsonl_provider_units": struct(
        target = "//crates/ctx-history-provider-native-jsonl:unit_tests",
        selected_inventory = True,
        tests = {
            "native_path::source_backed::copilot_tests::absent_and_ambiguous_capture_states_are_explicit": ["ambiguity_duplicate_linkage"],
            "native_path::source_backed::copilot_tests::completion_preserves_literal_result_without_inferred_status": ["result_preservation"],
            "native_path::source_backed::copilot_tests::invocation_preserves_exact_native_identity_and_arguments": ["exact_positive_pair"],
        },
    ),
    "mcp_attribution_selected_sqlite_provider_units": struct(
        target = "//crates/ctx-history-providers-sqlite-selected:unit_tests",
        selected_inventory = True,
        tests = {
            "providers::warp::nativepath::decode::tests::invalid_duplicate_orphan_and_ambiguous_mcp_relations_abstain": ["ambiguity_duplicate_linkage"],
            "providers::warp::nativepath::decode::tests::qualified_mcp_success_error_cancellation_and_nontext_results_link_exactly": ["exact_positive_pair"],
            "providers::warp::source_backed::result_tests::core_projection_keeps_success_failure_unknown_and_large_result_bodies_once": ["result_preservation"],
        },
    ),
}

def _checked_public_suite(suite_id):
    suite = MCP_ATTRIBUTION_PUBLIC_SUITES[suite_id]
    if not suite.target.startswith("//") or ":" not in suite.target:
        fail("public MCP attribution suite %s must name an absolute Bazel target" % suite_id)
    if type(suite.selected_inventory) != "bool":
        fail("public MCP attribution suite %s must declare selected_inventory as a bool" % suite_id)
    if not suite.tests:
        fail("public MCP attribution suite %s has zero tests" % suite_id)
    for test_name in suite.tests:
        if not test_name:
            fail("public MCP attribution suite %s has an empty test name" % suite_id)
        classes = suite.tests[test_name]
        if len(classes) != 1:
            fail("public MCP attribution test %s::%s must claim exactly one capability" % (suite_id, test_name))
        for evidence_class in classes:
            if evidence_class not in MCP_ATTRIBUTION_EVIDENCE_CLASSES:
                fail("public MCP attribution test %s::%s has unknown capability %s" % (suite_id, test_name, evidence_class))
    return suite

def mcp_attribution_suite_args():
    args = ["--mode", "public-validation"]
    targets = {}
    for suite_id in sorted(MCP_ATTRIBUTION_PUBLIC_SUITES):
        suite = _checked_public_suite(suite_id)
        if suite.target in targets:
            fail("public MCP attribution suites %s and %s reuse physical target %s" % (targets[suite.target], suite_id, suite.target))
        targets[suite.target] = suite_id
        args.extend([
            "--suite-alias" if suite.selected_inventory else "--suite",
            "%s=%s=$(rootpath %s)" % (suite_id, suite.target, suite.target),
        ])
        for test_name in sorted(suite.tests):
            args.extend([
                "--test-capability",
                "%s::%s=%s" % (suite_id, test_name, ",".join(sorted(suite.tests[test_name]))),
            ])
    return args

def mcp_attribution_suite_data():
    return sorted([
        _checked_public_suite(suite_id).target
        for suite_id in MCP_ATTRIBUTION_PUBLIC_SUITES
    ])
