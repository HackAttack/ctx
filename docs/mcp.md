# MCP

`ctx mcp serve` starts a local MCP server over newline-delimited stdio JSON-RPC.
It is for agents or MCP hosts that prefer tool discovery over shell commands.
The CLI remains the primary interface. MCP startup performs a bounded,
content-free health-check/wake and recovers the default-enabled persistent
daemon when needed. The MCP process never becomes a provider-history or
derived-state writer.

```bash
ctx mcp serve
ctx integrations install mcp
ctx integrations status mcp
```

`ctx integrations install mcp` can add this local server to supported
file-backed coding-agent MCP configs. Run `ctx docs show mcp-integrations` for
the support matrix, config paths, and manual snippets.

The server advertises its current tool set through MCP discovery rather than a
fixed documented count. Core tools include:

- `status`, the same structured source, upgrade, Pro, and compact local-usage
  status as `ctx status --format json`;
- `sources`, discovered local agent history sources;
- `search`, search the active Core/Tantivy generation and optional compatible
  semantic generation;
- `show_session`, read a stored Core session transcript by ctx session ID;
- `show_event`, read a stored Core event and optional surrounding window
  by ctx event ID;
- `query_events`, read one bounded deterministic page selected from normalized
  Core events.

The `search` tool accepts `content_scope` with the exact values `all`,
`transcript`, `calls`, or `outputs`. Omission resolves to `all`, and successful
search `structuredContent` always reports that resolved value in
`filters.content_scope`. `content_scope` conflicts unconditionally with the
exact `event_type` input.

The class mapping matches CLI search: `all` weights messages at 1.0, summaries
at 0.9, tool calls and command starts at 0.8, tool outputs, command outputs,
and command finishes at 0.6, and other or future searchable events at 0.8.
`transcript` keeps the message/summary weights; `calls` selects only tool calls
and command starts at ordinary lexical strength; `outputs` selects only tool
outputs, command outputs, and command finishes at ordinary lexical strength.
This selection is query-time only. It does not change complete retained/indexed
bodies, require a Core or index rebuild, infer diagnostic importance, or
collapse duplicate events.

Because the semantic projection contains transcript messages, `all` and
`transcript` retain normal semantic/hybrid behavior. `calls` and `outputs` use
lexical retrieval for a hybrid request and report the typed fallback in search
metadata; semantic-only requests for those scopes fail as unsupported.

`query_events` accepts the same typed identity, relationship, source, role,
event, workspace/file, chronology, order, and content-projection inputs as
`ctx list events`, plus an opaque continuation cursor. It returns one
`event_range_page` in `structuredContent`, including events, the pinned Core
generation, request selection, page usage, freshness/frontier state,
terminal/truncation state, and `next_cursor` when more results remain. It is
read-only after the MCP server's documented startup recovery. Its page is
additionally subject to the aggregate MCP response limit; select
`content=text` or `content=none`, or use CLI JSONL for a large stream.
Before hydration, MCP also rejects any single Core record whose indexed size
cannot fit a conservative projected response envelope. That failure is the
typed `output_limit_exceeded`; CLI JSONL remains the complete local stream.

`show_session` accepts an optional transcript mode plus resumable `limit` and
`cursor` inputs. Mode is applied before the page limit. `limit` defaults to 200
selected events and must be between 1 and 4,096. `cursor` is an opaque, nonempty
ASCII string of at most 4,096 bytes copied from the preceding page's
`next_cursor`; callers must not decode or construct it.

Successful `show_session` `structuredContent` is a `session_transcript` object
whose `events[]` contains one bounded page and whose `pagination` object has:

- `limit`, the requested or default selected-event limit;
- `returned`, the number of events in this page, at most `limit`;
- `has_more`, true only when another selected event remains;
- `next_cursor`, present exactly when `has_more` is true.

Continue with the same `ctx_session_id` and `mode`, the prior `next_cursor`, and
the desired limit. The cursor is exclusive and bound to the exact session and
active Core generation. A generation change returns `cursor_stale`; using a
cursor for another session returns `cursor_mismatch`; malformed cursor content
returns `invalid_cursor`. These are non-retryable typed tool errors. Restart
from the first page after `cursor_stale`; do not retry a mismatched or malformed
cursor unchanged.

`show_event` accepts bounded before, after, or symmetric window sizes. Both show
tools read complete policy-selected records from the active verified
Core/Tantivy generation without reopening provider history. MCP `show_session`
may return fewer than `limit` events with
`has_more: true` to stay within the response budget. After combining exact
`structuredContent` with the text fallback, every show response remains subject
to the 1 MiB MCP aggregate limit; an individually unrepresentable page fails
with `output_limit_exceeded` rather than silently clipping an event. MCP hosts
may log or forward the returned transcript.

This paging contract is MCP-specific. CLI `ctx show session` remains a
complete, unbounded stream unless the user explicitly requests terminal
`--max-events` truncation, and CLI JSONL ends with completion metadata rather
than a cursor. The in-repo Rust SDK follows the complete CLI path when both
`ShowSessionOptions.limit` and `.cursor` are absent, and uses this MCP page
contract when either is supplied.

Qualifying event rows from `show_event`, `show_session`, and `query_events`
include the same optional exact identity as camelCase
`mcpToolCall: {server, tool}`. Its presence identifies the exact source-time
MCP dispatch server and advertised tool on an observed execution; absence means
only that no qualifying exact pair was published. The complete object is
omitted rather than `null`.

Full-content event rows can separately include camelCase `mcpExchange`, with a
provider call ID and invocation and/or response. Present arguments and response
payloads stay decoded JSON, and keys inside those captured values are not
camelized. `query_events` includes the exchange only for `content: "full"`;
`content: "text"` and `content: "none"` omit it. Full `show_event` and
log-mode `show_session` rows can include it. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

Identity names, arguments, and responses are opaque private local data and can
contain sensitive identifiers or controls. `structuredContent` preserves exact
identity values; text fallback escapes them for display. Text fallback retains
the first 256 Unicode scalar values of each identity component independently,
appends `… [display truncated]` when a component is longer, and emits
`MCP identity display truncated; inspect structuredContent for exact JSON values.`

Ordinary tool results are selected by `show_session` only in `mode: "log"`.
To filter an entire session, request one bounded log-mode page, retain event
rows that contain `mcpToolCall` on the client, and repeat with the returned
`pagination.next_cursor` while `pagination.has_more` is true. Keep the session
ID and mode unchanged. For cross-session enumeration, use the existing
`query_events` cursor, optionally request `content: "none"`, and apply the same
presence filter to each page. Attribution adds no MCP selector, query input,
tool, SQL surface, or search behavior. See
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md).

The `status` tool returns the CLI JSON status read model unchanged in
`structuredContent`: the Core history report plus `upgrade`, compact
`local_usage`, and `read_only: true`. The added facts remain machine-only
and do not expand the MCP text fallback. The status read does not import,
initialize, refresh, or mutate source, upgrade, or usage state; configured
post-delivery local-usage accounting remains the independent server boundary
described below.

Local usage aggregation counts only recognized `tools/call` requests after the
complete JSON-RPC response has serialized, written, and flushed. Initialize,
ping, tool listing, malformed or invalid-ID envelopes, notifications,
pre-initialization protocol errors, unknown tools, and automatic daemon work
are not counted. The compact report’s `mcp_response_bytes` is factual
serialized transport bytes, including the newline—not tokens or savings. Local
recording has no network path, is independent of remote event reporting, and
fails silently without changing MCP output. The server re-resolves the
dedicated local control for every delivered call; an explicit `false` takes
effect before store I/O, while an unrelated config read/parse failure retains
the last known state.

MCP search sends the same bounded maintenance wake as CLI search and then
queries committed generations. It follows the CLI lexical, semantic, and
hybrid contracts, including lexical fallback for unavailable hybrid semantic
state, typed failure for semantic-only unavailability, and no vector work when
the semantic weight is zero. The MCP process does not import provider history,
initialize storage, or write provider data.

The `sources` tool returns the same bounded provider discovery `issues` as
`ctx sources --format json`, including stable issue codes and truncation markers.

MCP search defaults to primary-agent sessions only, matching `ctx search`.
Pass `include_subagents: true` when implementation details, code review notes,
test output, or failure traces from subagent sessions are relevant. When
`CODEX_THREAD_ID` is set, MCP search also excludes the active Codex session tree
by default; pass `include_current_session: true` when the active session tree is
the target.

Malformed tool arguments return `isError: true` with the existing diagnostic
`error` and stable `error_code: "invalid_request"` in `structuredContent`.
Malformed JSON-RPC framing or envelopes continue to use protocol-level parse
and invalid-params errors.

Tool results include MCP text content plus `structuredContent` JSON. Treat all
MCP output as private local history: it may include absolute paths, source
metadata, snippets, transcript text, MCP arguments, and response payloads, and
the MCP host may log or forward tool output.


Like CLI JSON status, MCP `status` can include local source, semantic, daemon,
and upgrade diagnostic path fields in `structuredContent`. They are local
troubleshooting hints for this machine, not portable contract IDs. Compact
`local_usage` contains only enablement, state, definition/retention versions,
and a stable content-free error when unavailable.
