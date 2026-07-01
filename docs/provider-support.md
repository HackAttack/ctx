# Provider Support

Provider support is intentionally conservative. A provider is documented as
locally importable only when the public CLI can read existing local history for
that provider.

## Status Meanings

| Status | Meaning |
| --- | --- |
| `local_import` | The CLI can import an existing local history source for this provider. |
| `local_import_when_supported` | The CLI has an importer for a specific local format, but support depends on that file existing and matching the documented format. |
| `fixture_only` | The repository has sanitized fixture coverage, but the public CLI does not discover or import native local history for that provider. |
| `detected_unsupported` | The CLI can detect something about the provider but intentionally does not import it. |
| `blocked` | No shipped discovery or import path exists. |

## Current Matrix

Machine-readable provider metadata lives in
[provider-support-matrix.json](provider-support-matrix.json). The public truth
is:

| Provider | Status | Public import path | Public smoke |
| --- | --- | --- | --- |
| Codex | `local_import` | `~/.codex/sessions`, `~/.codex/history.jsonl`, or an explicit Codex path. | Static local-history fixture smoke. |
| Pi | `local_import_when_supported` | `~/.pi/sessions.jsonl` or an explicit Pi JSONL path. | Static local-history fixture smoke. |
| Claude | `local_import_when_supported` | `~/.claude/projects` or an explicit Claude projects JSONL tree. | Static local-history fixture smoke. |
| OpenCode | `local_import_when_supported` | `~/.local/share/opencode/opencode.db` or an explicit OpenCode SQLite DB. | Static local-history fixture smoke. |
| Antigravity | `local_import_when_supported` | Antigravity `transcript_full.jsonl` or `transcript.jsonl` files under `~/.gemini/antigravity-cli/brain`, or an explicit Antigravity transcript JSONL tree. | Static local-history fixture smoke. |
| Gemini | `local_import_when_supported` | Gemini chat JSONL files under `~/.gemini/tmp/**/chats`, or an explicit Gemini CLI history tree. | Static local-history fixture smoke. |
| Cursor | `local_import_when_supported` | Cursor agent transcript JSONL files under `~/.cursor/projects/**/agent-transcripts`, or an explicit Cursor agent transcript path. | Static local-history fixture smoke. |
| Copilot CLI | `local_import_when_supported` | Copilot CLI `events.jsonl` files under `~/.copilot/session-state`, or an explicit Copilot CLI session-state tree. | Static local-history fixture smoke. |
| Factory AI Droid | `local_import_when_supported` | `~/.factory/sessions` or an explicit Factory AI Droid sessions tree. | Static local-history fixture smoke. |

Fidelity fields in the machine-readable matrix describe the default public CLI
import behavior and normalized ctx storage fields. Supported adapters record
normalized `files_touched` metadata when provider transcripts expose file paths
in tool calls, command output, patches, or native provider fields. Command
output, tool output, and token details remain skipped unless lower-level adapter
modes import them explicitly.

## Provider Smoke

Provider smoke coverage uses public fixture data and generated local-history
trees. The public smoke target exercises supported imports, blocked unsupported
providers, provider filtering, citations, and deterministic search without
executing provider CLIs, reading real user history, requiring API keys, or
making network calls:

```bash
bazel test //:provider_fixture_e2e --config=ci
```

## Required Evidence For Promotion

Before a provider moves beyond `fixture_only`, `detected_unsupported`, or
`blocked` into native local-history support, the change needs:

- a documented local source format;
- read-only source discovery or an explicit `--path` contract;
- malformed-input tests;
- idempotent re-import tests;
- source citation fields in search output;
- storage/privacy notes for provider-specific sensitive fields;
- docs updates in this file and `provider-support-matrix.json`.
