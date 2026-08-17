# MCP activity attribution evidence runbook

This runbook governs exact rows in
[`mcp-tool-call-attribution-capabilities.json`](mcp-tool-call-attribution-capabilities.json).
It is an evidence contract for provider-native MCP activity, not another
provider-support table. General local history support remains authoritative in
[`provider-support-matrix.json`](provider-support-matrix.json).

## Qualification bar

An `exact` tuple must prove all of the following at the imported durable
boundary:

1. The source stores an authoritative MCP protocol marker, raw dispatch server
   alias, and advertised tool name without reconstruction.
2. The source stores exact provider call identity that can bind invocation and
   result activity without FIFO, order, or timing inference.
3. The producer, route, source format, schema, and version/generation boundary
   are explicit. Unknown generations remain `not-qualified`.
4. Current executable tests cover the nine public evidence classes: exact
   activity, terminal outcomes, malformed and duplicate/ambiguous abstention,
   exact boundaries and max-plus-one rejection, result preservation, stable
   identities across lifecycle replay, and privacy sinks.

Configuration, current server lists, record order, punctuation splitting, and
time proximity are never identity evidence. Malformed, partial, oversized,
duplicate, or ambiguous identity evidence must not become a qualifying
`activity.invocation`.

The machine authority is
`crates/ctx-history-capture/tests/mcp-attribution-conformance.manifest.json`.
Its executable suite registry is
`crates/ctx-history-capture/tests/mcp_attribution_suites.bzl`. Manifest
capability revision 6 freezes 43 providers, 45 base routes, 44 imported schema
generations, and 48 capability lanes: three `supported`, 44 `not_qualified`,
and one `excluded`. Every supported row requires
`ambiguity_duplicate_linkage`, `canonical_terminal_outcomes`, `exact_boundary`,
`exact_positive_pair`, `malformed_identity`, `max_plus_one`, `privacy_sinks`,
`result_preservation`, and `stable_ids`. The max-plus-one and privacy checks are
provider-neutral closed classes; the other seven are tuple-specific provider
evidence.

For Codex's session-tree route, only unversioned generation 1 is supported.
Producer versions 0.200.0, 0.201.0, and 0.202.0 are distinct
`not_qualified` lanes, and the prompt-history route remains `not_qualified`.

## Typed failure reasons

- `lossy_composite`: persistence normalizes, truncates, flattens, or
  non-injectively combines required identity.
- `exact_pair_transient_or_config`: required identity exists only in runtime,
  configuration, discovery, or provider-overridable state.
- `no_server_field`: durable call evidence has no authoritative server alias.
- `no_unique_terminal_link`: linking would require order, FIFO, timing, or name
  inference instead of an exact key.
- `route_mismatch`: a richer producer route is not the route ctx imports, or
  required identity is lost before the admitted boundary.
- `writer_version_unproven`: no public first-party writer/version contract
  proves the admitted durable shape.

`excluded` is separate from those failures and is used only for a hosted remote
trace outside ctx's local-history boundary.

## Public evidence hygiene

Evidence entries use public first-party source, release, artifact, or product
links and record only observed version bounds. Static binary inspection may
support a row when the official artifact and version are public, but local
paths, credentials, user transcripts, and nonpublic reports must never appear
in this contract.

Provider history and activity values are private and not share-safe by default.
Sanitized fixtures must preserve the structural ambiguity or exactness being
tested without copying arguments, results, paths, tokens, customer names, or
unrelated metadata.

## Change checklist

1. Add a new route/schema/producer row instead of broadening an older row by
   implication.
2. Record public evidence, observed pins, and fail-closed treatment of unknown
   generations.
3. For `exact`, bind current parser revision plus one executable provider test
   for each required evidence role. For `not-qualified`, choose one primary
   typed reason and explain secondary defects in `detail`.
4. Run `python3 scripts/check-mcp-tool-call-attribution-capabilities.py`, its
   mutation tests, the three focused provider suites, and the normal docs
   check.
