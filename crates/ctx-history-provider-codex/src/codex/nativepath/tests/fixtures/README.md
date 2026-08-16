# Codex NativePath MCP fixtures

`mcp_tool_call_attribution_adversarial.jsonl` is a synthetic, hand-authored
fixture. It combines exact-name, malformed-evidence, duplicate-key, duplicate
terminal, conflicting-pair, and sequential-call-ID-reuse cases; it is not a
captured producer transcript.

`mcp_tool_call_end_direct_result.jsonl` is a sanitized structural fixture
derived from a producer transcript. It retains only the fields needed to test
direct terminal-result linkage; unrelated events and payload values are
omitted or replaced.

`current_exec_repository_evidence.jsonl` is a sanitized structural fixture
derived from current Codex commit-producing orchestration. It covers the exact
nullish-template continuation and JSON-stringified direct-result shapes while
using placeholders for the temporary repository and commit receipt.
