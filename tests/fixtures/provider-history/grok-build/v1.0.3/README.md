# Grok Build 1.0.3 native session fixture

This static smoke fixture is a deterministic sanitization of a real Grok Build
headless session created in a synthetic Pocket Calculator Git repository. No
personal workspace, existing agent history, credential, account, or production
source was used.

## Provenance

- Grok Build package: `@xai-official/grok@1.0.3`
- CLI version: `grok 1.0.3 (1a29d5bc12)`
- Platform package: `@xai-official/grok-linux-x64@1.0.3`
- Official source commit:
  `be713136d2a69080743a3f6b3c72077057e5948f`
- Sanitized `updates.jsonl` SHA-256:
  `c23d29b0f0a300b094064e0df918e8b668c44739361157ed4ce1e19739b780b8`

The task asked Grok Build to inspect two tiny calculator files, add subtraction,
add a test, and run `npm test`. The native session contains parallel reads, a
successful edit, a failed edit followed by a corrected edit, a successful
command result, and a final assistant message.

## Authority and privacy

`updates.jsonl` is Grok Build's documented authoritative restore log and is
self-identifying. Ctx does not inspect the derived `chat_history.jsonl`,
provider system prompt, summary/title sidecars, raw snapshots, or telemetry.

Temporary paths and identities, process/model/timing noise, request-local
accounting, and duplicated transport-oriented terminal payloads were removed.
Native record ordering, role/type shapes, call/result linkage, visible content,
and tool outcomes are preserved. The static file is credential-free and needs
no network access or generation step to run the public tests.
