#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: test-taxonomy-policy-test.sh ROOT_BUILD" >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "$root_build")"
tmp="$(mktemp -d "${TEST_TMPDIR:-/tmp}/ctx-test-taxonomy.XXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/home"

query() {
  env -u BUILD_WORKSPACE_DIRECTORY \
    HOME="$tmp/home" \
    BAZEL_OUTPUT_USER_ROOT="$tmp/bazel-output" \
    CTX_BAZEL_SANDBOX_BASE="$tmp/bazel-sandboxes" \
    CTX_BAZEL_WORKSPACE="$repo_root" \
    "$repo_root/scripts/bazelw" query "$1" --output=label
}

all_tests='kind(".*_test rule", //...)'
query "$all_tests" | LC_ALL=C sort -u >"$tmp/all"
query "attr(\"tags\", \"manual\", $all_tests)" | LC_ALL=C sort -u >"$tmp/manual"
query "attr(\"tags\", \"tier-nightly\", $all_tests)" | LC_ALL=C sort -u >"$tmp/nightly"
query "attr(\"tags\", \"tier-release\", $all_tests)" | LC_ALL=C sort -u >"$tmp/release"
query "attr(\"tags\", \"tier-\", $all_tests)" | LC_ALL=C sort -u >"$tmp/tiered"
query 'attr("tags", "tier-", kind("test_suite rule", //...))' \
  | LC_ALL=C sort -u >"$tmp/tiered-suites"

LC_ALL=C sort -u "$tmp/nightly" "$tmp/release" >"$tmp/known-tiered"
comm -23 "$tmp/tiered" "$tmp/known-tiered" >"$tmp/unknown-tier"
comm -12 "$tmp/nightly" "$tmp/release" >"$tmp/conflicting-tier"
comm -12 "$tmp/manual" "$tmp/tiered" >"$tmp/manual-tiered"

failed=0
for pair in \
  "unknown tier tag:$tmp/unknown-tier" \
  "both tier-nightly and tier-release:$tmp/conflicting-tier" \
  "manual and tier-routed:$tmp/manual-tiered" \
  "tier tag on test_suite instead of a leaf test:$tmp/tiered-suites"; do
  reason="${pair%%:*}"
  path="${pair#*:}"
  if [[ -s "$path" ]]; then
    printf 'public test taxonomy error (%s):\n' "$reason" >&2
    sed 's/^/  /' "$path" >&2
    failed=1
  fi
done
(( failed == 0 )) || exit 1

all_count="$(wc -l <"$tmp/all")"
manual_count="$(wc -l <"$tmp/manual")"
nightly_count="$(wc -l <"$tmp/nightly")"
release_count="$(wc -l <"$tmp/release")"
printf 'public test taxonomy: OK all=%s default_ci=%s nightly_only=%s release_only=%s manual=%s\n' \
  "$all_count" "$((all_count - manual_count - nightly_count - release_count))" \
  "$nightly_count" "$release_count" "$manual_count"
