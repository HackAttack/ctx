# MCP provider activity capture

ctx stores provider-native invocation and result evidence in the optional
`activity` field of policy-selected Core events. The field is repository
neutral: it preserves literal provider data and explicit capture states without
creating a separate MCP entity or interpreting a provider's status, effects, or
causal meaning.

The filename of this page is retained for stable documentation links. The
current wire field is `activity`.

## Activity shape

`activity` has revision `1` and may contain:

- `provider_call_id`: exact typed provider-native key material;
- `invocation`: optional protocol, optional server, required tool, arguments,
  and optional start time;
- `result`: optional provider status, completion time, duration, text capture,
  and structured-content capture; and
- `facts`: ordered, non-deduplicated provider-declared literal facts from the
  closed public vocabulary.

Invocation or result content requires `provider_call_id`. A facts-only activity
does not. Separate invocation and result events use the same exact key; a
combined provider event may carry both members.

Arguments and structured results have four exhaustive capture states:

- `present`: a complete JSON value was retained;
- `absent`: the source represented no value;
- `unavailable`: the source had a value but it could not be captured exactly;
- `omitted`: ctx intentionally omitted the complete value and records a reason
  plus optional observed encoded size.

Text results use the same states plus `normalized_body`, which points to the
event's complete normalized text instead of storing it twice. Capture states do
not imply success or failure. Codex preserves an empty provider result string
as an exact empty structured-content value while marking the text channel
`absent`.

## Exact MCP invocation

An invocation is exact MCP attribution only when `protocol` is `mcp` and the
source supplies separate exact `server` and `tool` strings. ctx does not split a
combined function name, consult current configuration, infer a server from a
transport URL, or link records by order or timing. See
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md) for qualified
provider tuples and access examples.

## Projection and bounds

Show event/session output includes complete selected activity. `ctx list
events` and MCP `query_events` include it only for the `full` content
projection; `text` and `none` omit it.

Activity, normalized text, and structured event content share one aggregate
16 MiB selected-content budget. The JSONL fitting path replaces oversized
complete argument/result channels with explicit `omitted` captures when that
makes the record fit. It does not truncate a value into a `present` capture.

Machine JSON/JSONL and MCP `structuredContent` preserve the admitted activity
exactly. Human output escapes terminal controls and may bound the rendered
event. Activity values are private local content and require review before
sharing.
