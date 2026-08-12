#!/usr/bin/env bash
set -euo pipefail

[[ $# -ge 1 ]] || {
  printf 'usage: %s GLOB...\n' "$0" >&2
  exit 2
}
mkdir -p target/public-cli-artifacts
for pattern in "$@"; do
  buildkite-agent artifact download \
    "target/public-cli-artifacts/${pattern}" . \
    --step public-cli-linux-factory
done
