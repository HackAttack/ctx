#!/usr/bin/env bash
set -euo pipefail

ZIG_VERSION="0.14.1"
ZIG_LINUX_X64_URL="https://ziglang.org/download/${ZIG_VERSION}/zig-x86_64-linux-${ZIG_VERSION}.tar.xz"
ZIG_LINUX_X64_SHA256="24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c"
ZIG_LINUX_AARCH64_URL="https://ziglang.org/download/${ZIG_VERSION}/zig-aarch64-linux-${ZIG_VERSION}.tar.xz"
ZIG_LINUX_AARCH64_SHA256="f7a654acc967864f7a050ddacfaa778c7504a0eca8d2b678839c21eea47c992b"
CARGO_ZIGBUILD_VERSION="0.23.0"
LINUX_GLIBC_BASELINE="2.39"
LINUX_RELEASE_IMAGE_UBUNTU="24.04"
MACOS_DEPLOYMENT_TARGET="13.0"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/build-public-cli-artifact.sh PLATFORM

Builds one public ctx CLI binary and stages it under target/public-cli-artifacts.
Platforms: linux-x64, linux-aarch64, macos-arm64, macos-x64, windows-x64, freebsd-x64.
USAGE
}

platform="${1:-}"
if [[ -z "${platform}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi

case "${platform}" in
  linux-x64)
    target="x86_64-unknown-linux-gnu"
    build_target="${target}"
    binary_name="ctx"
    ;;
  linux-aarch64)
    target="aarch64-unknown-linux-gnu"
    build_target="${target}"
    binary_name="ctx-linux-aarch64"
    ;;
  macos-arm64)
    target="aarch64-apple-darwin"
    build_target="${target}"
    binary_name="ctx-macos-arm64"
    ;;
  macos-x64)
    target="x86_64-apple-darwin"
    build_target="${target}"
    binary_name="ctx-macos-x64"
    ;;
  windows-x64)
    target="x86_64-pc-windows-gnu"
    build_target="${target}"
    binary_name="ctx.exe"
    ;;
  freebsd-x64)
    target="x86_64-unknown-freebsd"
    build_target="${target}"
    binary_name="ctx-freebsd-x64"
    ;;
  *)
    usage
    exit 2
    ;;
esac

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${root_dir}"

zig_host_archive() {
  case "$(uname -m)" in
    x86_64|amd64)
      printf '%s\t%s\t%s\n' \
        "zig-x86_64-linux-${ZIG_VERSION}" \
        "${ZIG_LINUX_X64_URL}" \
        "${ZIG_LINUX_X64_SHA256}"
      ;;
    aarch64|arm64)
      printf '%s\t%s\t%s\n' \
        "zig-aarch64-linux-${ZIG_VERSION}" \
        "${ZIG_LINUX_AARCH64_URL}" \
        "${ZIG_LINUX_AARCH64_SHA256}"
      ;;
    *)
      echo "error: automatic Zig bootstrap does not support Linux $(uname -m)" >&2
      exit 127
      ;;
  esac
}

ensure_zig_for_linux_host() {
  if command -v zig >/dev/null 2>&1; then
    return
  fi

  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: zig is required to cross-build ${platform} from $(uname -s)" >&2
    exit 127
  fi

  for required_tool in curl tar; do
    if ! command -v "${required_tool}" >/dev/null 2>&1; then
      echo "error: ${required_tool} is required to bootstrap Zig ${ZIG_VERSION}" >&2
      exit 127
    fi
  done

  IFS=$'\t' read -r zig_archive_dir zig_url zig_sha256 < <(zig_host_archive)
  toolchain_dir="${CTX_PUBLIC_CLI_TOOLCHAIN_DIR:-target/public-cli-toolchain}"
  install_dir="${toolchain_dir}/${zig_archive_dir}"
  if [[ ! -x "${install_dir}/zig" ]]; then
    mkdir -p "${toolchain_dir}"
    archive="${toolchain_dir}/${zig_archive_dir}.tar.xz"
    tmp_archive="${archive}.tmp"
    curl -fsSL "${zig_url}" -o "${tmp_archive}"
    if command -v sha256sum >/dev/null 2>&1; then
      actual_sha="$(sha256sum "${tmp_archive}" | awk '{ print $1 }')"
    elif command -v shasum >/dev/null 2>&1; then
      actual_sha="$(shasum -a 256 "${tmp_archive}" | awk '{ print $1 }')"
    else
      echo "error: sha256sum or shasum is required to verify Zig ${ZIG_VERSION}" >&2
      exit 127
    fi
    if [[ "${actual_sha}" != "${zig_sha256}" ]]; then
      echo "error: Zig ${ZIG_VERSION} checksum mismatch: expected ${zig_sha256}, got ${actual_sha}" >&2
      exit 1
    fi
    mv "${tmp_archive}" "${archive}"
    rm -rf "${install_dir}"
    tar -C "${toolchain_dir}" -xf "${archive}"
  fi
  export PATH="${install_dir}:${PATH}"
}

ensure_darwin_cross_tools() {
  if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    cargo install cargo-zigbuild --version "${CARGO_ZIGBUILD_VERSION}" --locked
  fi
  ensure_zig_for_linux_host
  command -v zig >/dev/null 2>&1 || {
    echo "error: zig is required to cross-build ${platform} from $(uname -s)" >&2
    exit 127
  }
}

run_linux_container_build() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: ${platform} artifacts must be built from Linux" >&2
    exit 1
  fi
  case "${platform}:$(uname -m)" in
    linux-x64:x86_64|linux-x64:amd64|linux-aarch64:aarch64|linux-aarch64:arm64)
      ;;
    *)
      echo "error: ${platform} artifacts must be built on matching Linux, got $(uname -m)" >&2
      exit 1
      ;;
  esac
  if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker is required to build Linux release artifacts" >&2
    exit 127
  fi

  local rust_toolchain="${CTX_RUST_TOOLCHAIN:-1.88.0}"
  local image="ctx-public-cli-linux:rust-${rust_toolchain}-ubuntu-${LINUX_RELEASE_IMAGE_UBUNTU}"
  local out_dir="${CTX_PUBLIC_CLI_ARTIFACT_DIR:-target/public-cli-artifacts}"
  local cargo_target_dir="${CARGO_TARGET_DIR:-target/public-cli-linux/${platform}}"

  case "${out_dir}" in
    /*)
      echo "error: absolute CTX_PUBLIC_CLI_ARTIFACT_DIR is not supported for container Linux builds" >&2
      exit 1
      ;;
  esac
  case "${cargo_target_dir}" in
    /*)
      echo "error: absolute CARGO_TARGET_DIR is not supported for container Linux builds" >&2
      exit 1
      ;;
  esac

  mkdir -p "${out_dir}" "${cargo_target_dir}"
  docker build \
    --build-arg "RUST_TOOLCHAIN=${rust_toolchain}" \
    -t "${image}" \
    -f scripts/docker/linux-release.Dockerfile \
    scripts/docker
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e "CTX_PUBLIC_CLI_IN_CONTAINER=1" \
    -e "CTX_PUBLIC_CLI_ARTIFACT_DIR=${out_dir}" \
    -e "CARGO_TARGET_DIR=${cargo_target_dir}" \
    -e "HOME=/tmp" \
    -v "${root_dir}:/work" \
    -w /work \
    "${image}" \
    bash scripts/build-public-cli-artifact.sh "${platform}"
}

if [[ "${platform}" == "linux-x64" && "${CTX_PUBLIC_CLI_IN_CONTAINER:-}" != "1" ]]; then
  run_linux_container_build
  exit 0
fi

version="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; data=json.load(sys.stdin); print(next(pkg["version"] for pkg in data["packages"] if pkg["name"] == "ctx"))')"
if [[ -z "${version}" ]]; then
  echo "error: could not determine ctx package version from Cargo metadata" >&2
  exit 1
fi
echo "building ctx ${version} for ${platform}"

if [[ "${platform}" == linux-* ]]; then
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "error: ${platform} artifacts must be built on native Linux" >&2
    exit 1
  fi
  case "${platform}:$(uname -m)" in
    linux-x64:x86_64|linux-x64:amd64|linux-aarch64:aarch64|linux-aarch64:arm64)
      ;;
    *)
      echo "error: ${platform} artifacts must be built on matching native Linux, got $(uname -m)" >&2
      exit 1
      ;;
  esac
fi

rustup target add "${target}" >/dev/null
out_dir="${CTX_PUBLIC_CLI_ARTIFACT_DIR:-target/public-cli-artifacts}"
mkdir -p "${out_dir}"
build_target_dir="${CARGO_TARGET_DIR:-target}"

if [[ "${platform}" == macos-* ]]; then
  export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-${MACOS_DEPLOYMENT_TARGET}}"
fi

if [[ "${platform}" == linux-* ]]; then
  cargo build -p ctx --release --target "${build_target}" --locked
elif [[ "${platform}" == macos-* && "$(uname -s)" != "Darwin" ]]; then
  ensure_darwin_cross_tools
  cargo zigbuild -p ctx --release --target "${build_target}" --locked
elif [[ "${platform}" == "freebsd-x64" ]]; then
  if ! command -v cross >/dev/null 2>&1; then
    cargo install cross --locked
  fi
  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    export CARGO_TARGET_DIR="target/public-cli-cross/${platform}"
    build_target_dir="${CARGO_TARGET_DIR}"
  fi
  cross build -p ctx --release --target "${target}" --locked
else
  cargo build -p ctx --release --target "${build_target}" --locked
fi

target_binary="${build_target_dir}/${target}/release/ctx"
if [[ ! -f "${target_binary}" && "${build_target}" != "${target}" ]]; then
  target_binary="${build_target_dir}/${build_target}/release/ctx"
fi
if [[ "${platform}" == "windows-x64" ]]; then
  target_binary="${target_binary}.exe"
fi
staged="${out_dir}/${binary_name}"
cp "${target_binary}" "${staged}"
chmod 755 "${staged}"

if command -v file >/dev/null 2>&1; then
  file "${staged}"
fi

sha_file="${staged}.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${staged}" | awk '{ print $1 }' > "${sha_file}"
else
  shasum -a 256 "${staged}" | awk '{ print $1 }' > "${sha_file}"
fi

case "${platform}" in
  linux-x64|linux-aarch64)
    "${staged}" --version | tee "${staged}.version"
    grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    ;;
  macos-arm64)
    if [[ "$(uname -s)" == "Darwin" && "$(uname -m)" == "arm64" ]]; then
      "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  macos-x64)
    if [[ "$(uname -s)" == "Darwin" ]] && /usr/bin/arch -x86_64 /usr/bin/true >/dev/null 2>&1; then
      /usr/bin/arch -x86_64 "${staged}" --version | tee "${staged}.version"
      grep -Fx "ctx ${version}" "${staged}.version" >/dev/null
    else
      printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    fi
    ;;
  *)
    printf 'not run on this host: %s\n' "${platform}" > "${staged}.version"
    ;;
esac

scripts/check-public-cli-artifact.sh "${platform}" "${out_dir}"

printf 'built %s for %s sha256=%s\n' "${staged}" "${platform}" "$(cat "${sha_file}")"
