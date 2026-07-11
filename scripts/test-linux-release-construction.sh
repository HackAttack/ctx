#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

printf 'artifact\n' > "${tmp_dir}/artifact"
printf 'lock\n' > "${tmp_dir}/Cargo.lock"
build_info_args=(
  --output "${tmp_dir}/artifact.build-info.json"
  --artifact "${tmp_dir}/artifact"
  --cargo-lock "${tmp_dir}/Cargo.lock"
  --platform linux-x64
  --target x86_64-unknown-linux-gnu
  --source-commit 0123456789abcdef
  --source-clean true
  --rust-version "rustc test"
  --expected-builder-base sha256:expected
  --actual-builder-base sha256:expected
  --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
  --static-status passed
  --local-runtime-status passed
  --local-runtime-authority authoritative
)
python3 scripts/write-public-cli-build-info.py "${build_info_args[@]}"
first_build_info_sha="$(sha256sum "${tmp_dir}/artifact.build-info.json")"
python3 scripts/write-public-cli-build-info.py "${build_info_args[@]}"
test "${first_build_info_sha}" = "$(sha256sum "${tmp_dir}/artifact.build-info.json")"
python3 - "${tmp_dir}/artifact.build-info.json" <<'PY'
import json
import sys

document = json.load(open(sys.argv[1], encoding="utf-8"))
assert document["builder"]["base_image"] == {
    "actual": "sha256:expected",
    "expected": "sha256:expected",
}
assert document["builder"]["image_id"] == "sha256:" + "a" * 64
assert document["runtime"]["image_id"] == "sha256:" + "b" * 64
assert document["inspector"]["image_id"] == "sha256:" + "c" * 64
PY

python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/cross-artifact.build-info.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform windows-x64 \
  --target x86_64-pc-windows-gnu \
  --source-commit 0123456789abcdef \
  --source-clean true \
  --rust-version "rustc test" \
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --static-status passed \
  --local-runtime-status not_run \
  --local-runtime-authority not_run
python3 - "${tmp_dir}/cross-artifact.build-info.json" <<'PY'
import json
import sys

document = json.load(open(sys.argv[1], encoding="utf-8"))
assert document["builder"]["image_id"] is None
assert document["builder"]["base_image"] == {"actual": None, "expected": None}
assert document["runtime"]["image_id"] is None
assert document["inspector"]["image_id"] == "sha256:" + "c" * 64
PY

if python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/mismatch.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform linux-x64 \
  --target x86_64-unknown-linux-gnu \
  --source-commit 0123456789abcdef \
  --source-clean true \
  --rust-version "rustc test" \
  --expected-builder-base sha256:expected \
  --actual-builder-base sha256:wrong \
  --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --static-status passed \
  --local-runtime-status passed \
  --local-runtime-authority authoritative \
  >/dev/null 2>&1; then
  echo "mismatched builder identity unexpectedly produced build evidence" >&2
  exit 1
fi

if python3 scripts/write-public-cli-build-info.py \
  "${build_info_args[@]}" \
  --builder-image-id not-a-digest >/dev/null 2>&1; then
  echo "invalid builder image identity unexpectedly produced build evidence" >&2
  exit 1
fi

if python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/bad-authority.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform linux-x64 \
  --target x86_64-unknown-linux-gnu \
  --source-commit 0123456789abcdef \
  --source-clean true \
  --rust-version "rustc test" \
  --static-status passed \
  --local-runtime-status not_run \
  --local-runtime-authority authoritative >/dev/null 2>&1; then
  echo "inconsistent runtime authority unexpectedly produced build evidence" >&2
  exit 1
fi

test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin arm64 passed)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 not_run)" = not_run
if scripts/public-cli-runtime-authority.sh macos-x64 Darwin arm64 invalid >/dev/null 2>&1; then
  echo "invalid runtime status unexpectedly produced authority" >&2
  exit 1
fi

multiline_cross_output='cross 0.2.5
rustup 1.28.2
cargo 1.88.0'
test "$(printf '%s\n' "${multiline_cross_output}" | sed -n '1p')" = 'cross 0.2.5'
test "$(printf '%s\n' 'cross 0.2.4' 'rustup 1.28.2' | sed -n '1p')" != 'cross 0.2.5'

mkdir -p "${tmp_dir}/dirty-path"
cat > "${tmp_dir}/dirty-path/git" <<'EOF'
#!/bin/sh
case "${1:-}" in
  rev-parse) printf '%s\n' 0123456789abcdef ;;
  status) printf '%s\n' '?? synthetic-dirty-file' ;;
  *) exit 2 ;;
esac
EOF
chmod +x "${tmp_dir}/dirty-path/git"
dirty_out="target/ctx-release-dirty-test.$$"
trap 'rm -rf "${tmp_dir}" "${dirty_out}"' EXIT
mkdir -p "${dirty_out}"
printf 'stale evidence\n' > "${dirty_out}/ctx.exe.build-info.json"
if PATH="${tmp_dir}/dirty-path:${PATH}" \
  CTX_PUBLIC_CLI_ARTIFACT_DIR="${dirty_out}" \
  scripts/build-public-cli-artifact.sh windows-x64 \
  >"${tmp_dir}/dirty.out" 2>"${tmp_dir}/dirty.err"; then
  echo "non-Linux construction accepted a dirty source tree" >&2
  exit 1
fi
grep -Fq 'public release construction requires a clean checkout' "${tmp_dir}/dirty.err"
test ! -e "${dirty_out}/ctx.exe.build-info.json"

grep -F '20260701T000000Z' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'ubuntu:22.04@sha256:' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'RUSTUP_VERSION="1.28.2"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'RUST_TOOLCHAIN_VERSION="1.88.0"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'rustup target add --toolchain "${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'cargo "+${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'public release construction requires a clean checkout' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'source commit changed during public release construction' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'linux-*' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--network none' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'scripts/run-native-candidate-smoke.sh' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'LINUX_X64_QEMU_CPU_PROFILE="qemu64"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'CTX_TEST_ONLY_ALLOW_EMULATED_LINUX_BUILD' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'flock -n' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'local_runtime_authority' scripts/write-public-cli-build-info.py >/dev/null
grep -F -- '--expected-builder-base "${LINUX_RELEASE_UBUNTU_DIGEST}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--actual-builder-base "${actual_base_digest}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--runtime-image-id "${runtime_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--inspector-image-id "${inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--inspector-image-id "${artifact_inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'build-info.json' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--locked --offline' scripts/build-linux-release-offline.sh >/dev/null
grep -F "cross --version | sed -n '1p'" scripts/build-public-cli-artifact.sh >/dev/null
grep -F "cargo-zigbuild --version | sed -n '1p'" scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'run_host_artifact_check' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--target runtime' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--target inspector' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'org.ctx.release.role="runtime"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'runtime tool missing' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F '"${runtime_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F '"${inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'timeout --signal=KILL 120s' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'x86_64-unknown-freebsd:0.2.5@sha256:' Cross.toml >/dev/null

printf 'Linux release construction self-test passed\n'
