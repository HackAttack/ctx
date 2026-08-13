# DeepSeek Harness session fixtures

These public fixtures are synthetic constructions of DeepSeek Harness's native
per-session persistence schema at upstream commit
`47f943859bef60e4160492346772ded9b24f765a` and logical session format version
`0`. They are static, sanitized, and credential-free, with fixed IDs, timestamps,
sequence numbers, working paths, messages, and model metadata.

The parent session covers a user message, an assistant message with
`source.kind = "model"`, a successful file-edit tool call/result with native
`meta.diffs` path evidence, a failed read tool call/result, and a final
assistant message. The child session carries `parentSession`,
`origin = "subagent"`, and `delegationDepth = 1` in its native header.

`raw/` and `zstd/` are separate DeepSeek Harness home roots. A root therefore
contains only one physical encoding:

- `raw/sessions/**/session.jsonl` contains plaintext JSONL.
- `zstd/sessions/**/session.jsonl.zstd` contains the same logical JSONL as two
  concatenated, XXH64-checksummed Zstandard frames: the header line in the
  first frame and the complete event segment in the second frame. Zstandard is
  the upstream default encoding.

## Verification

`zstd --list --verbose` reports two frames and `Check: XXH64` for each binary.
`zstd --decompress --stdout <binary> | cmp - <raw>` verifies logical equality.

## SHA-256

```text
9cdd5b48e8d8d4e504a0ef6c8da2b48d16aa01bc8435bddd287e856b23902297  raw/sessions/--workspace-deepseek-harness-fixture--/11111111-2222-4333-8444-555555555555/session.jsonl
23674a98ddc3b6c0cded395f84e3105055319a32dce9dd8620046437c6046ae5  raw/sessions/--workspace-deepseek-harness-fixture--/66666666-7777-4888-8999-aaaaaaaaaaaa/session.jsonl
3ca00e7dbcb0b1a2a504f986f269cf4f38ae22d78d67ebc76a232a5336084c8b  zstd/sessions/--workspace-deepseek-harness-fixture--/11111111-2222-4333-8444-555555555555/session.jsonl.zstd
52baeed92a33cbe9d210f4e81513c102bc3d9b32f4a9e45d90f138ea2b0f23a6  zstd/sessions/--workspace-deepseek-harness-fixture--/66666666-7777-4888-8999-aaaaaaaaaaaa/session.jsonl.zstd
```
