#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  printf 'SDK publish guard failed: %s\n' "$*" >&2
  exit 1
}

require_file_contains() {
  local file="$1"
  local pattern="$2"
  local message="$3"
  if ! grep -Eq "$pattern" "$file"; then
    fail "$message"
  fi
}

require_file_contains sdks/typescript/package.json '"private"[[:space:]]*:[[:space:]]*true' \
  'TypeScript SDK package.json must remain private'
require_file_contains sdks/python/pyproject.toml '^publish[[:space:]]*=[[:space:]]*false$' \
  'Python SDK pyproject.toml must keep [tool.ctx] publish = false'
require_file_contains crates/ctx-sdk/Cargo.toml '^publish[[:space:]]*=[[:space:]]*false$' \
  'Rust SDK crate must keep publish = false'
require_file_contains crates/ctx-protocol/Cargo.toml '^publish[[:space:]]*=[[:space:]]*false$' \
  'Rust protocol crate must keep publish = false'
require_file_contains sdks/dotnet/src/Ctx.AgentHistory/Ctx.AgentHistory.csproj '<IsPackable>false</IsPackable>' \
  '.NET SDK project must keep IsPackable=false until NuGet publishing is intentional'

if rg -n --glob '!scripts/check-sdk-no-publish.sh' \
  --glob '!contracts/agent-history-v1/README.md' \
  --glob '!docs/sdk-production-readiness.md' \
  --glob '!sdks/**/README.md' \
  --glob '!crates/ctx-sdk/README.md' \
  --glob '!target/**' \
  --glob '!bazel-*' \
  -e '(^|[[:space:]])(npm publish|twine upload|cargo publish|dotnet nuget push|gradle publish|mvn deploy|swift package-registry publish)([[:space:]]|$)' \
  .; then
  fail 'live SDK package-manager publish command found outside docs/policy text'
fi

printf 'SDK publish guard passed\n'
