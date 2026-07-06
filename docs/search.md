# Search

`ctx search` finds matching indexed history. Default results are session-diverse:
ctx shows the strongest matching span from each session, then lets you drill
into dense event-level results when needed. By default it first performs a quiet
best-effort text refresh of a bounded recent batch from discovered native
provider sources and enabled auto history-source plugins, then queries the local
SQLite store. If a recent ctx daemon native-history refresh already covered the
normal auto path, search skips the duplicate synchronous native scan and reports
`status: "read_only", reason: "daemon_recent"` in JSON freshness while serving
the existing index. If more refresh backlog remains, JSON freshness reports
`status: "budget_exhausted"` and the next search or `--refresh strict` can keep
catching up. If the existing store is readable but not writable by the current
binary, auto refresh is skipped and JSON freshness reports
`status: "read_only"` while serving the existing index.
Semantic search reads existing local sidecar coverage when it is already
available. Interactive search does not start vector backfill, download models,
create the semantic sidecar, or start a daemon. The default `auto` backend is
lexical-first: it reranks lexical candidates only when sidecar coverage is ready
for those candidates, and it can use bounded semantic rescue from a
coverage-gated partial sidecar when lexical search finds no candidates.

## Search

Examples:

```bash
ctx search "build failure"
ctx search "sqlite storage" --provider codex
ctx search "retry handling" --workspace checkout --since 60d
ctx search "tool output" --event-type tool_output
ctx search --file crates/foo/src/lib.rs
ctx search "token budget" --refresh off
ctx search "signed metadata" --term checksum --term release
ctx search "token budget" --limit 5
ctx search "token budget" --session <ctx-session-id>
ctx search "review findings" --include-subagents
ctx search "this current task" --include-current-session
ctx search "mail provider throttled bulk mailbox setup" --backend hybrid
ctx search "pricing for ctx cloud team history" --backend semantic
ctx status
ctx status --json
```

A result can include:

- `ctx_event_id`, the ctx-owned event ID for event hits;
- `ctx_session_id`, the ctx-owned session ID when known;
- `provider_session_id`, the provider-owned session ID when known;
- title or event label;
- snippet with truncation where needed;
- rank, result scope, and match reasons;
- session importance and more-matches count for default session results;
- provider;
- event sequence;
- timestamp;
- working directory when known;
- source path and cursor when available;
- source availability flag when known;
- citations;
- `suggested_next_commands`, copyable commands for `ctx show`, `ctx locate`,
  and scoped follow-up searches.

Search result IDs are ctx-owned. Commands accept full ctx IDs or unambiguous
ctx ID prefixes of at least eight hex characters. Provider-owned IDs are
exposed as metadata so humans can recognize the original provider session, but
they are not positional lookup IDs. Provider-owned lookup must be explicit, for
example `--provider codex --provider-session <provider-session-id>` on commands
that support it.

## Filters

Search filters narrow both human output and JSON:

- `--provider codex|claude|cursor|pi|opencode|github-copilot|copilot-cli|antigravity|gemini|kilo|kiro-cli|crush|goose|tabnine|windsurf|zed|factory-ai-droid|qwen-code|kimi-code-cli|auggie|junie|firebender|forgecode|deepagents|mistral-vibe|mux|rovodev|openclaw|hermes|nanoclaw|astrbot|shelley|continue|openhands|cline|roo|lingma|qoder|warp|codebuddy|trae`;
- `--history-source <plugin/source-or-provider_key/source_id>`, for custom
  history imports;
- `--provider-key <key>`, `--source-id <id>`, and
  `--source-format <format>`, for exact custom history source filters;

- `--workspace <name-or-path>`, substring match over stored workspace, cwd,
  source path, or repository-name text;
- `--since <rfc3339-or-days>d`;
- `--event-type <event-type>`, one of `message`, `tool_call`, `tool_output`,
  `command_started`, `command_output`, `command_finished`, `file_touched`,
  `vcs_change`, `artifact`, `summary`, or `notice`;
- `--file <path>`, indexed touched-file path metadata, not the current
  filesystem;
- `--session <ctx-session-id-or-prefix>`;
- `--term <query-or-keyword>`, repeatable broadening terms merged with OR-style
  semantics, not required terms;
- `--events`;
- `--include-subagents`;
- `--limit <n>`;
- `--backend auto|lexical|semantic|hybrid`;
- `--semantic-weight <0.0-1.0>`, for hybrid ranking;
- `--refresh auto|off|strict`;
- `--include-current-session`.

CLI provider filters use the kebab-case names above. JSON output and stable SQL
views use provider IDs in ctx output; multiword provider IDs may be snake_case,
such as `copilot_cli`, `factory_ai_droid`, `qwen_code`, `kimi_code_cli`, `kiro_cli`, `mistral_vibe`, or `roo_code`.

`--since` accepts RFC 3339 timestamps such as `2026-06-01T00:00:00Z` or a day
window such as `30d`.

`--file <path>` filters by normalized `files_touched` metadata when provider
transcripts expose touched paths. Use it without a query to list indexed events
for a file, or combine it with query terms to find sessions that both mention a
topic and touched that path. It searches paths recorded during import; it does
not inspect the current filesystem.

Search requires a non-empty query, at least one non-empty `--term`, or
`--file <path>`. Provider, workspace, time, session, event, source, and result
flags only narrow an actual search; by themselves they do not browse recent
history.

The default searches primary-agent sessions so human intent and decisions stay
prominent. Use `--include-subagents` when you want implementation details, code
review notes, test output, or failure analysis from subagent sessions too.

`--limit` defaults to `20` and is capped at `200`.

Default search returns diverse session-level results. Use
`--session <ctx-session-id>` after a default search has identified a session to
inspect; scoped session search returns dense event hits. Use `--events` without
`--session` when you want dense event hits across sessions.

`--backend lexical` preserves the FTS/BM25 search path. `--backend semantic`
uses local FastEmbed embeddings over v2 semantic documents: a transient metadata
header plus chunked event semantic text. The header is embedded and hashed but
not stored as plaintext in the sidecar. `--backend hybrid` blends lexical and
semantic evidence with reciprocal-rank fusion; `--semantic-weight` controls the
semantic contribution and defaults to `0.35`. Public reports name the model as
`sentence-transformers/all-MiniLM-L6-v2`; the required local FastEmbed cache is
the Qdrant ONNX artifact directory `models--Qdrant--all-MiniLM-L6-v2-onnx`.
Semantic and hybrid searches read the coverage that is already present in the
semantic sidecar; they do not perform foreground vector catch-up. They also
require the local embedding model cache to already exist. If the cache is
missing, hybrid falls back to lexical and strict semantic search fails with an
explicit local error instead of initializing or downloading a model during
search. Semantic and hybrid fall back to lexical
when explicit filters require vector prefiltering that is not implemented yet or
when repeatable `--term` would need lexical OR semantics. The default
primary-agent scope and active Codex-session exclusion use bounded overfetch
rather than disabling semantic lookup. Hybrid also falls back when semantic
coverage is too thin to justify foreground query embedding. Results still return
the normal ctx result shape with concrete indexed local evidence, and default
search remains session-diverse unless
`--events` or `--session` asks for dense event results. The displayed semantic
snippet is regenerated from the current eligible event payload and the best
matching chunk offsets. The semantic index lives in a private sidecar
`vectors.sqlite` next to the main ctx store and stores vectors, chunk hashes,
and offsets rather than plaintext chunks. It does not change the main
`work.sqlite` schema.

`--backend auto` uses lexical search unless semantic prerequisites are already
met. When the local model cache exists, sidecar coverage passes the hybrid gate,
the active filters are safe for semantic lookup, and lexical search produces a
bounded candidate pool, `auto` uses bounded hybrid reranking only when every
lexical candidate event has current vector coverage. This avoids the full
sidecar scan used by explicit semantic search and avoids biasing default results
with a partially embedded candidate pool.
When lexical search produces no candidates, `auto` can use bounded semantic
rescue from the coverage-gated sidecar and reports partial coverage in the
retrieval metadata so callers can distinguish early backfill from a fully ready
semantic index.
`CTX_SEMANTIC_THREADS` caps ONNX Runtime CPU threads,
`CTX_SEMANTIC_EMBED_BATCH` tunes embedding batch size, and
`CTX_SEMANTIC_CACHE_DIR` can point ctx at a pre-populated local model cache. When
`HF_HOME` is set, FastEmbed uses that cache location first.

When ctx is run from Codex and `CODEX_THREAD_ID` is available, search excludes
the active Codex session tree by default so the current prompt and its subagent
work do not dominate history research. Use `--include-current-session` when you
are intentionally looking for material from the active session tree.

`--refresh` defaults to `auto`. `auto` attempts a best-effort pre-search import
of discovered native provider sources and enabled auto history-source plugins,
then serves the existing text index if that refresh fails. When the daemon has
recently refreshed normal native sources, search can serve the existing index
without repeating that native scan. Auto plugin refresh is still run from the
search path and is capped for interactive search; use `--refresh strict` or
`ctx import` for a long-running plugin catch-up. On large discovered sources or
already-cataloged indexes, `auto` imports a bounded recent batch and then serves
results, leaving remaining backlog for later searches,
`--refresh strict`, or `ctx import --all`. Search reports existing semantic
coverage through retrieval JSON and `ctx status`, but `strict` does not wait for
full semantic coverage. `off` skips the pre-search refresh, never runs plugin
commands, and does not schedule or run semantic indexing. Explicit-only native
sources such as NanoClaw, plus search-only sources without native import
support, are searched from the existing index until they are explicitly imported
through a supported path. Supported AstrBot `data_v4.db` locations participate in
bounded native discovery and may also be imported with an explicit `--path`.

Use `--refresh off` for a search that does not import providers, execute
plugins, schedule semantic indexing, or update either the main ctx SQLite store
or semantic sidecar. In this mode default `auto` stays lexical. Explicit
semantic or hybrid requests may still initialize an already-cached local
embedding model to embed the query and read existing sidecar coverage; they do
not download a model or write semantic catch-up work during search.

## Semantic Freshness

Semantic freshness is part of the normal search/status surface rather than a
separate CLI namespace. `ctx search` reads the sidecar coverage that already
exists and reports semantic coverage and worker state in JSON. `ctx status`
reports whether a worker is running, whether the model cache is available,
recent heartbeat/error timestamps, counts for searchable, embedded, and queued
items, and dirty/stale items. Raw CLI status can include private local
sidecar/lock/status paths for troubleshooting; treat them as current-machine
diagnostics, not portable API identifiers. Status also includes a `daemon` block
for the ctx-owned background coordinator, including whether daemon runs are
enabled in config. `ctx daemon run` can perform bounded native provider-history
refresh and local semantic catch-up; managed default startup is still being
implemented.
A long-lived daemon keeps the local embedding model resident after the first
worker pass, uses a low-memory default embedding batch, and performs recent-work
freshness checks before it settles into idle loops. Cloud sync currently reports disabled and
`network_allowed: false`. `ctx doctor` is the place for semantic and daemon
diagnostics when local status needs troubleshooting.

Search never starts the daemon or waits for full semantic coverage. Explicit
semantic or hybrid queries read the sidecar coverage that already exists.
Default `auto` either uses bounded hybrid reranking when the prerequisites above
are met or stays on the lexical path.

## History Reports

Use the agent history-search skill when a topic needs a cited report instead of
a ranked hit list. The skill should run several `ctx search` queries, inspect the
best cited events or sessions with `ctx show`, and write the report itself. ctx
only retrieves indexed local evidence; it does not synthesize conclusions.

## Machine Output

Use default text output for agent reading. Use `ctx search <query> --json` or a
term/file search with `--json` for scripts, `jq`, or exact field extraction.
JSON results include the same result metadata and citations as the human output,
plus a top-level `freshness` object describing the pre-search text refresh mode
and outcome and a top-level `retrieval` object describing the requested/effective
backend, semantic status/fallback, embedding model, sidecar coverage, background
worker status, and semantic diagnostics when vector retrieval runs or when auto
evaluates bounded-hybrid gates. Raw CLI retrieval may include a private local
vector sidecar path as additive diagnostic metadata; SDK consumers should not
depend on it as contract state. Diagnostics include query
embedding time, vector scan time, chunks scanned, vector bytes read, events
scored, hydration time, stale-vector drops, semantic candidate count, bounded
auto candidate counts, and an auto skip reason when applicable. A citation with
`source_exists: false` means ctx can return indexed text, but the raw provider
file was not available at the stored path when the result was built.

Search output is local/private by default.
Review copied snippets, JSON, or transcripts before sending them outside the
machine.
