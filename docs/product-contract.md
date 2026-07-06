# Product Contract

ctx is a local search CLI for existing agent history.

## Promise

Given local provider transcripts that ctx supports, the CLI can build a local
SQLite index and return deterministic retrieval results with citations. The
product boundary is retrieval, not interpretation.

## In Scope

- `ctx setup` initializes local storage and indexes discovered supported local
  transcript formats.
- `ctx sources` reports known local provider history paths, including whether a
  native source is currently importable.
- `ctx import` indexes supported local transcript formats and selected local
  history-source plugins.
- `ctx search` can refresh a bounded batch from discovered native provider
  sources and enabled auto history-source plugins before returning ranked local
  hits from the local index, with event IDs when a hit maps to an indexed event.
- `ctx show session` and `ctx show event` render transcripts, hits, and context
  windows using ctx-owned IDs, and `ctx show session --out` writes transcript
  artifacts.
- `ctx locate session` and `ctx locate event` report provenance and resume
  metadata.
- `ctx sql` runs one read-only SQL statement against the existing local index
  for advanced inspection when normal search is not expressive enough.
- `ctx doctor` reports local storage health.
- `ctx docs` exposes embedded public documentation and generated man pages.
- `ctx upgrade` checks and applies signed CLI releases for official
  installer-managed binaries.
- `ctx daemon` is the public local coordinator surface for status,
  enable/disable config, and foreground local maintenance runs. The current
  coordinator surface is local-only and limited to bounded native
  provider-history refresh, semantic indexing/freshness status, and disabled
  cloud-sync status.
- `ctx status` and `ctx doctor` report ctx-owned daemon coordinator state.
- JSON output supports local agents and scripts.

## Out Of Scope

- hosted model inference, hidden LLM calls, or API-key-dependent inference by
  ctx; local semantic embedding is allowed only as documented search behavior;
- remote accounts or sync;
- browser UI;
- source repository modification;
- shell startup-file modification;
- write-capable SQL access;
- API-key requirements for core setup/import/search;
- provider-history daemons, hooks, cloud sync, or background collection outside
  documented ctx-owned local semantic maintenance;
- self-upgrade for unmanaged source builds, package-manager installs, or copied
  binaries;
- provider-native import claims that are not listed in the support matrix.

## Determinism

For the same database, query, filters, and result limit, search should return
the same ranked material in the same order. Timestamps such as `generated_at`
can differ between runs.

## Citation Contract

Results should preserve enough metadata for an agent to verify important
details:

- provider when known;
- ctx-owned session and event IDs;
- provider-owned session ID when known;
- event sequence when known;
- source path and cursor when available;
- source availability when checked.

Provider-owned IDs are metadata. Positional command arguments are ctx-owned
IDs unless a command explicitly accepts `--provider ... --provider-session ...`.

If raw source files move, ctx may still return indexed text from SQLite. Output
should make source availability visible when that information is known.

## Privacy Contract

The local index and JSON output are private by default. A user must review and
review copied output before sharing it outside the machine.
