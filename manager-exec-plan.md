# Semantic Search / Daemon Readiness Plan

Updated: 2026-07-06 after sqlite-vec/default-autostart hardening

## Objective

Coordinate subagents and verification to harden `codex/semantic-hybrid-search` until semantic search and the background daemon are credible as the default ctx experience.

## Subagent / Coordination State

- Cleaned stale Codex child edges in app state: closed 72 recent open child edges without process-manager entries; left live-associated edges alone. Backup: `/home/daddy/.codex/state_5.sqlite.backup.cleanup-subagents-20260705T234600-0500`.
- Closed the current manager thread's stale child edges after recovering available reports. Backup: `/home/daddy/.codex/state_5.sqlite.backup.cleanup-current-semantic-subagents-20260706T001534-0500`.
- Usage limits stopped new implementation subagents after the runtime-fix worker was spawned. Existing review transcripts were recovered from local Codex rollout files.
- Readiness reviewer result: no-ship yet, mainly because the branch is dirty/uncommitted, behind `origin/main`, and the first-class daemon/default background story is not yet a managed default.
- Contract worker patch is present in the worktree and verified by SDK/contract checks.
- Latest review agents:
  - Noether: no-ship until default daemon cannot do expensive cached-model semantic work, default `auto` cannot rescue from tiny partial coverage, and eval/soak gates are stricter.
  - Zeno: sqlite-vec promising but not default-safe unless derived vec0 tables cannot become active while partial/stale and deletes/prunes maintain vec0.
  - Dalton: setup/import autostart should be history-refresh/status only; semantic catch-up should require explicit `ctx daemon run`.

## Evidence Collected

- Isolated real-corpus setup:
  - Data root: `/home/daddy/.cache/ctx-dogfood/semantic-hybrid-20260705T235002`.
  - Initial setup from real `/home/daddy/.codex` indexed 146,755 events / 3,711 sessions before returning, with 28,781 catalog sessions still pending.
  - Store size after setup: about 719 MB before moving out of `/tmp`.
- Search dogfood on isolated corpus:
  - `auto`, `lexical`, and `hybrid` with missing semantic model cache were fast, about 0.3-0.6s on representative indexed queries.
  - `hybrid` fell back to lexical with `semantic_fallback_code=semantic_index_missing`.
  - strict `semantic` failed fast and local-only when model cache/sidecar were unavailable.
- Bounded daemon dogfood:
  - `ctx daemon run --once --max-runtime-seconds 30 --max-seconds 10 --max-chunks 128 --json` completed in 3.60s.
  - Imported 2,895 more events / 67 sessions.
  - Semantic job skipped with `model_cache_missing`; cloud sync disabled; daemon status completed.
- Cached-model semantic dogfood:
  - `ctx daemon run --once --max-runtime-seconds 180 --max-seconds 120 --max-chunks 5000 --json` completed in 2:05.75 with max RSS about 1.25 GiB.
  - Imported 8,166 events / 124 sessions and embedded 1,610 items / 1,974 chunks before `budget_exhausted`.
  - Coverage after the pass was about 1.02% of the isolated corpus; explicit hybrid/semantic worked, while default `auto` correctly stayed lexical because candidate coverage was not ready.

## Verification Passed

- `git diff --check`
- `cargo test -p ctx --bin ctx semantic_vector_store -- --nocapture`
- `cargo test -p ctx --bin ctx daemon_ -- --nocapture`
- `cargo test -p ctx --test cli status_reports_ -- --nocapture`
- `python3 scripts/check-agent-history-contract.py`
- `bash scripts/check-docs.sh`
- `bash scripts/check-sdks.sh`
- `python3 -m unittest discover -s scripts/search_eval -p 'test_*.py'`

## Current Gates Before Ship

1. Completed: rebased/replayed branch onto current `origin/main`; new WIP commit is `a01899e8`, with backup ref `backup/semantic-hybrid-before-rebase-20260706`.
2. Completed: daemon default rollout is now a short one-pass setup/import autostart after successful full `ctx setup` and native `ctx import` when `[daemon].enabled` is true. It skips catalog-only setup, JSON output, disabled config/env, CI, search, and live daemon locks.
3. Completed: daemon lifecycle JSON now reports `start_mode` and `trigger_command`; `doctor --json` includes the daemon block.
4. Completed: setup/import autostart is semantic-status-only and reports `autostart_semantic_disabled`; it does not create/update `vectors.sqlite` even with a warm local model cache.
5. Completed: default `auto` only does no-candidate semantic rescue when semantic coverage is fully ready and dirty queue is empty; partial/dirty coverage stays lexical.
6. Completed: sqlite-vec vec0 exact search is only active when derived vec0 rows are in parity with canonical BLOB chunks, and prune/delete paths maintain vec0 rows.
7. In progress: rebase onto latest `origin/main` again before merge because this branch is still behind upstream.
8. Run longer soak on the hardened default behavior after latest rebase and review.

## Post-Rebase Notes

- Rebase conflicts were limited to `.gitignore`, `crates/ctx-cli/src/main.rs`, `crates/ctx-cli/src/mcp.rs`, `crates/ctx-history-store/src/lib.rs`, `docs/cli-reference.md`, and `docs/search.md`.
- CLI/MCP and docs conflict resolution was done by subagents. The final `.gitignore` and schema constant cleanup was a tiny manager unblock after two core workers stalled.
- Branch is now `0 1` against `origin/main`; `git diff --check` and conflict-marker scan pass.

## Active 2026-07-06 Manager Pass

- Hubble completed installer/docs product-surface cleanup for daemon autostart opt-out and local-only background maintenance wording.
- Beauvoir's CLI behavior/test patch was integrated: native-only import autostart, `--no-daemon` coverage, custom/history-source no-autostart, and daemon status start metadata.
- Descartes review finding fixed: background auto-upgrade now takes precedence over daemon autostart for that foreground command to avoid Windows executable replacement races.
- Integration gates completed:
  1. Focused autostart/daemon/doctor/search-refresh tests passed.
  2. Formatting, locked check, full CLI integration tests, docs/SDK/contract checks passed.
  3. Rebased release binary dogfood completed with isolated data root and daemon autostart defaults.
  4. Longer soak can proceed after this basic gate; no new correctness blocker found.

## Fresh Dogfood Evidence 2026-07-06

- Release build: `cargo build --release -p ctx --bin ctx` completed.
- Isolated data root: `/home/daddy/.cache/ctx-dogfood/semantic-default-20260706T025515`.
- Real-corpus setup against `/home/daddy/.codex/sessions`:
  - Source size: 32,514 files / 13.4 GiB.
  - Wall time: 3:19.44, max RSS 700,804 KB.
  - Indexed: 32,520 sessions, 445,873 events, 4,301 edges, 478,393 items.
  - Store size: about 2.4 GiB.
- Setup autostart:
  - Daemon status `completed`, `start_mode: auto`, `trigger_command: setup`.
  - Cloud sync `disabled`, `network_allowed: false`.
  - Semantic skipped locally with `model_cache_missing`; no runaway process remained.
- Full-corpus searches:
  - Representative auto searches returned in about 0.6-0.7s with refresh enabled.
  - Search auto imported a tiny live delta after setup and correctly stayed lexical with `auto_hybrid_skipped: model_cache_missing`.
  - Explicit hybrid with no semantic index/model fell back to lexical in 0.75s.
  - Strict semantic failed fast in 0.73s with an actionable no-download local-cache message.
- Incremental real-corpus import after setup:
  - Same 32,514-file / 13.4 GiB Codex tree.
  - Imported 1 new event, skipped 1 unchanged session.
  - Wall time: 3.20s, max RSS 194,220 KB.
  - Import autostart daemon completed with `start_mode: auto`, `trigger_command: import`, semantic skipped with `model_cache_missing`.

## Vector Backend Experiment 2026-07-06

- Implemented a branch-only `sqlite-vec` vec0 prototype alongside the existing Rust BLOB scan.
  - Current canonical sidecar remains `event_embedding_chunks.embedding_f32`.
  - Derived tables: `event_embedding_vec0` plus `event_embedding_vec0_meta`.
  - Hidden selector: `CTX_SEMANTIC_VECTOR_BACKEND=auto|rust|sqlite-vec`.
  - Hidden one-time derivation: `CTX_SEMANTIC_SQLITE_VEC_SYNC=1`.
- Crate/package finding:
  - `sqlite-vec 0.1.10-alpha.4` failed to compile from crates.io because the published package references missing `sqlite-vec-diskann.c`.
  - Pinned prototype to `sqlite-vec = "=0.1.9"`, which compiles and supports vec0 cosine search.
  - SQLite Vec1 remains promising but too new/high-complexity for this branch's first shippable vector path because it introduces ANN training/recall tuning and newer release risk.
- Real sidecar comparison on `/home/daddy/.cache/ctx-dogfood/semantic-default-20260706T025515`:
  - Coverage was still tiny: 387 chunks / 344 events.
  - Rust BLOB scan: 9 ms vector scan, 2.32s wall, same top 5 results.
  - sqlite-vec vec0: 6 ms vector scan, 1.50s wall, same top 5 results.
- Synthetic release benchmark with exact 384-dim vectors:
  - 10k chunks, bulk vec0 sync: ingest+sync 0.84s; Rust scan 13 ms; sqlite-vec 6 ms; identical top hit.
  - 50k chunks, bulk vec0 sync: ingest+sync 4.24s; Rust scan 63 ms; sqlite-vec 31 ms; identical top hit.
  - 200k chunks, bulk vec0 sync: ingest+sync 18.81s; Rust scan 255 ms; sqlite-vec 119 ms; identical top hit.
  - Initial per-event vec0 maintenance was unacceptable: 10k chunks took 14.2s.
  - Batched incremental maintenance fixed that shape: 10k chunks 0.61s; 50k chunks 5.84s; 200k chunks 45.92s; query results stayed identical.
- Recommendation:
  - Continue with exactly one primary vector backend for this branch: `sqlite-vec` vec0 exact search, not SQLite Vec1 yet.
  - Keep Rust BLOB scan as the mandatory compatibility/fallback path for unsupported platforms, filtered candidate lookup, and any vec0 parity drift.
  - Do not adopt Vec1 until sqlite-vec exact search is insufficient or Vec1 matures enough to justify ANN recall/training complexity.

## Latest Hardening Pass 2026-07-06

- Default daemon/autostart:
  - Added explicit `daemon_semantic_allowed_for_run`; setup/import autostart returns semantic job `skipped` with reason `autostart_semantic_disabled`.
  - Autostart no longer reserves time for semantic work, queues dirty semantic work, opens/creates `vectors.sqlite`, or initializes the embedding model.
  - `queue_recent_semantic_work` is sidecar-existing-only, so imports with a warm model cache do not introduce semantic storage.
- Default auto search:
  - No-candidate semantic rescue now requires `semantic_worker_coverage_ready` (full coverage and `dirty_items == 0`).
  - Candidate rerank remains allowed only when every lexical candidate has vector coverage.
- sqlite-vec:
  - `sqlite_vec_ready` requires canonical chunk count parity, not just non-empty vec0 metadata.
  - Incremental vec0 sync batches event IDs in chunks of 500.
  - Prune/delete paths delete matching vec0 rows, and plaintext scrub/VACUUM rebuilds vec0 from canonical chunks.
- Verification:
  - `cargo check -p ctx --bin ctx --locked`
  - `cargo test -p ctx --test cli -- --nocapture` passed: 208/208.
  - `cargo test -p ctx-history-store --lib` passed: 62/62.
  - `cargo test -p ctx semantic_vector_store -- --nocapture` passed: 6/6.
  - `cargo check -p ctx --bin ctx --target x86_64-pc-windows-gnu --locked` passed with pre-existing upgrade dead-code warnings.
  - `cargo check -p ctx --bin ctx --target x86_64-unknown-freebsd --locked` passed.
  - `cargo fmt --check` and `git diff --check` passed.
  - `cargo build -p ctx --release --locked` passed.
  - 10k release vector benchmark: ingest+bulk vec0 sync 208 ms, Rust scan 12 ms, sqlite-vec scan 6 ms, identical top hit.
- Real-corpus dogfood on `/home/daddy/.cache/ctx-dogfood/semantic-default-20260706T025515`:
  - Status after latest explicit daemon pass: 451,661 searchable semantic items, 357 embedded items, 403 chunks, 954 dirty items.
  - Default `auto` on representative and no-lexical queries stayed lexical with `auto_hybrid_skipped: coverage_not_ready`.
  - Explicit semantic search used `sqlite_vec0`; after the small daemon pass it scanned 403 chunks in 2 ms, hydrated in 7 ms, returned 5 results.
  - Explicit `ctx daemon run --once --max-chunks 16 --max-seconds 30 --json` indexed 16 semantic chunks, wall 26.62s, max RSS 691,300 KB, no error.

## Remaining Before Merge/Default Rollout

- Rebase this large branch onto current `origin/main` and rerun the full gate set.
- Run a longer soak with the hardened default behavior, focusing on:
  - setup/import autostart never creating semantic sidecars;
  - search p50/p95 for default `auto`;
  - daemon status transitions and lock recovery;
  - explicit daemon RSS/CPU on a warm model;
  - sqlite-vec parity staying clean across prune/delete/VACUUM.
- Consider modularizing semantic sidecar code out of `main.rs` before merge if the rebase is painful or reviewability blocks the PR.
