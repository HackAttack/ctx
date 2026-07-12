#!/usr/bin/env bash
set -euo pipefail

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The infrastructure trust boundary is that Buildkite and Infisical restrict
# privileged host authentication to trusted jobs. This defense-in-depth gate
# verifies the checked-out bytes inside every secrets-capable command.

if [[ "${CTX_LOCAL_MACOS_SIGNING_LIVE_TEST:-0}" == "1" ]]; then
  for ci_name in BUILDKITE CI GITHUB_ACTIONS; do
    [[ -z "${!ci_name:-}" ]] || \
      die "CTX_LOCAL_MACOS_SIGNING_LIVE_TEST is forbidden when ${ci_name} is set"
  done
  printf 'macOS signing trust gate ok: explicit local live-test override\n'
  exit 0
fi

[[ "${BUILDKITE:-}" == "true" || "${BUILDKITE:-}" == "1" ]] || \
  die "real macOS signing requires trusted Buildkite or the explicit local live-test override"
[[ "${BUILDKITE_PULL_REQUEST:-false}" == "false" ]] || \
  die "macOS signing is forbidden for Buildkite pull requests"
[[ "${BUILDKITE_BRANCH:-}" == "main" ]] || \
  die "macOS signing is restricted to the Buildkite main branch"
[[ -z "${BUILDKITE_TAG:-}" ]] || \
  die "macOS signing does not accept an implicit tag trust path"
case "${BUILDKITE_REPO:-}" in
  https://github.com/ctxrs/ctx|https://github.com/ctxrs/ctx.git|git@github.com:ctxrs/ctx|git@github.com:ctxrs/ctx.git) ;;
  *) die "macOS signing requires the canonical ctxrs/ctx Buildkite repository" ;;
esac

head_commit="$(git -C "${root_dir}" rev-parse --verify HEAD)"
origin_main="$(git -C "${root_dir}" rev-parse --verify refs/remotes/origin/main 2>/dev/null || true)"
[[ "${BUILDKITE_COMMIT:-}" == "${head_commit}" ]] || \
  die "BUILDKITE_COMMIT does not match the checked out commit"
[[ -n "${origin_main}" && "${origin_main}" == "${head_commit}" ]] || \
  die "checked out commit is not the exact trusted origin/main commit"
git -C "${root_dir}" diff --quiet --ignore-submodules -- || \
  die "tracked source files changed before macOS signing"
git -C "${root_dir}" diff --cached --quiet --ignore-submodules -- || \
  die "staged source files changed before macOS signing"

printf 'macOS signing trust gate ok: Buildkite main at %s\n' "${head_commit}"
