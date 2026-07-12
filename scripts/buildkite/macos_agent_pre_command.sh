#!/usr/bin/env bash
set -euo pipefail

# The shared macOS Buildkite agent invokes this repo-owned hook before each job.
# Secrets-capable public CLI lanes must pass the same trusted-ref gate before
# their command can reach the narrow Infisical signing launcher.
case "${BUILDKITE_STEP_KEY:-}" in
  public-cli-macos-arm64|public-cli-macos-x64|public-cli-macos-x64-native-smoke)
    if [[ "${CTX_PUBLIC_CLI_ARTIFACT_MATRIX:-0}" == "1" ]]; then
      root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
      "${root_dir}/scripts/check-macos-signing-trusted-ref.sh"
    fi
    ;;
esac
