# Providers

ctx imports existing agent history through conservative provider adapters. Each adapter makes a narrow, testable claim about the local source format it reads.

## Supported Local Imports

The public CLI supports these local-history harnesses:

Codex, Grok Build, DeepSeek Harness, Pi, Claude, OpenCode, Kilo Code, Kiro CLI, Crush, Goose, Lingma, Qoder, Warp, CodeBuddy, Trae, OpenClaw, Hermes Agent, NanoClaw, AstrBot, Shelley, Continue, OpenHands, Antigravity, Gemini, Tabnine, Cursor, Windsurf, Zed, Copilot CLI, Factory AI Droid, Qwen Code, Kimi Code CLI, Auggie, Junie, Firebender, ForgeCode, Deep Agents, Mistral Vibe, Mux, Rovo Dev, Cline, Roo Code, MiMo Code.

Use `ctx sources` for the truth on the current machine:

```bash
ctx sources
ctx sources --format json
ctx sources --all
```

Default `ctx sources` output keeps the common missing-location list compact. Use `--all` to inspect every recognized provider location. The CLI recognizes these provider names; recognition does not imply that every detected schema is importable:

```text
codex, grok-build, deepseek-harness, claude, cursor, pi, opencode, github-copilot, copilot-cli, antigravity, gemini, kilo, kiro-cli, crush, goose, tabnine, windsurf, zed, factory-ai-droid, qwen-code, kimi-code-cli, auggie, junie, firebender, forgecode, deepagents, mistral-vibe, mux, rovodev, openclaw, hermes, nanoclaw, astrbot, shelley, continue, openhands, cline, roo, lingma, qoder, warp, codebuddy, trae, mimocode
```

Aliases are accepted for common naming differences, for example `grok`, `dsh`, `deepseek_harness`, `claude-code`, `gemini-cli`, `github-copilot`, `droid`, `augment`, `qoder-cn`, `trae-cn`, and `roo-code`. The shorter name `deepseek` is not a DeepSeek Harness alias.

Custom history is separate: `ctx import --input-format ctx-history-jsonl-v1
--path <file>` reads an explicit JSONL interchange file from any exporter, and
history-source plugin manifests can register a durable provider-owned file.
The optional `provider_native_v1` lineage contract accepts typed relationships
and exact native copied-from selectors; legacy files and command-only plugins
remain lineage/origin unknown.

Exact MCP server/tool attribution is a separate, narrower event capability.
Supported provider import does not automatically qualify it. The complete
43-provider importable route/format partition is documented in
[`mcp-tool-call-attribution.md`](mcp-tool-call-attribution.md) and its
machine-readable
[`capability contract`](mcp-tool-call-attribution-capabilities.json).
Capability revision 4 exact providers are Codex, Warp, and Copilot CLI. Deep
Agents remains generally supported through its local SQLite import, which is
not qualified for exact attribution; its hosted trace is separately excluded
from this local-only capability boundary.

Provider activity is policy-selected content. For qualified tuples it preserves
each provider's native combined or split invocation/result event shape. See
[`mcp-exchange-capture.md`](mcp-exchange-capture.md).

## Location Selection

For each provider and product surface, ctx applies the provider's current
precedence and checks only the winning official root. A replacement environment
or persistent-config value replaces the lower-priority default; it is not added
beside it. Multiple roots are emitted only for current coexisting stores such as
installed clients, persisted profiles, or configured agents. See
[`provider-support-matrix.json`](provider-support-matrix.json) for every row.

One-shot flags, API constructor paths, old launch directories, container host
mounts, copies, and unreconstructible selectors are not automatic. Import one
with `ctx import --provider <provider> --path <path>`. That path bypasses
discovery precedence for the invocation, but not format checks, read bounds,
no-link checks, or read-only handling, and it is not remembered as a default.

Detected unsupported formats and sources marked `import_support: explicit` are
excluded from setup, `--all`, daemon refresh, and search refresh. Removing an
old automatic probe does not delete indexed history; a still-supported
compatible path can be selected explicitly.

Grok Build selects absolute `$GROK_HOME/sessions` when `GROK_HOME` is set and
`~/.grok/sessions` otherwise. The override replaces the default. A native
session requires authoritative `updates.jsonl`; derived sidecars are not
discovery or import authority. Exact `updates.jsonl` files remain importable
with `--provider grok-build --path`.

DeepSeek Harness is Supported for its exact local session format version 0
only. Discovery selects absolute `$DSH_HOME/sessions` when
`DSH_HOME` is nonempty and absolute, or `~/.dsh/sessions` otherwise. Empty or
whitespace-only values are unset; relative values are not automatically
resolved because their meaning depends on the launch working directory.
Default-encoded leaves are nested `*/*/session.jsonl.zstd`; configured raw
history uses nested `*/*/session.jsonl`. Other layouts and format versions are
not supported. Hosted/cloud history is outside this local import. General
history support does not claim exact MCP server/tool attribution for this
provider. Unknown required events and future versions fail the source.
Delegated sessions remain independent imports; the immediate parent header
does not prove the transitive root identity required for typed lineage edges.

Hermes Agent is supported through the native `hermes_state_sqlite` route. On
Linux, a non-root ctx process with the certified read-only live-WAL path makes
new sessions and appended records converge on native-watch and search refreshes.
Where that fast path is unavailable, incremental refresh defers without copying
the provider database. Structural edits, deletions, and deferred increments
reconcile in roughly 60–80 minutes with a healthy daemon, or on
`ctx import --provider hermes` or `ctx import --all`. All scans are read-only
and never modify Hermes history.

Interactive discovery captures a fresh allowlisted environment and current
working directory. A long-lived daemon uses the named environment/CWD snapshot
from its launch, so restart it after changing provider root variables. A
coordinator that evaluates project-scoped providers across multiple worktrees
must call the injected discovery context once per already observed or explicitly
authorized worktree, then apply the normal bounded de-duplication. It
must not use provider roots as repository identity, infer worktrees from those
roots, or crawl for repositories; logical repository, checkout, and worktree
identity remain a separate activity-derived concern.

## Import Rules

Provider imports should be bounded, read-only, and tied to a documented source
format. Do not document a provider as locally importable until the CLI can
discover or parse that provider's real local history and the provider support
matrix marks the shipped path as Supported. Contributor-facing content and
fixture expectations are defined in
[`provider-import-policy.md`](provider-import-policy.md).
