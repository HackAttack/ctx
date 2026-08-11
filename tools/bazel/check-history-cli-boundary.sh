#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo 'usage: check-history-cli-boundary.sh ROOT_BUILD' >&2
  exit 64
fi

root_build="$(readlink -f "$1")"
repo_root="$(dirname "${root_build}")"
history_root="${repo_root}/crates/ctx-history-cli"
final_provider_args="${repo_root}/crates/ctx-cli/src/provider_args.rs"
final_provider_sources="${repo_root}/crates/ctx-cli/src/provider_sources.rs"

python3 - "${history_root}/Cargo.toml" <<'PY'
import sys
import tomllib
from pathlib import Path

manifest = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
dependencies = set(manifest.get("dependencies", {}))
forbidden = {"clap", "ctx-cli", "ctx-agent-application", "ctx-agent-integrations"}
present = sorted(forbidden & dependencies)
if present:
    raise SystemExit(f"ctx-history-cli forbidden direct dependency: {present}")
if any(name.startswith("ctx-pro-") for name in dependencies):
    raise SystemExit("ctx-history-cli must not depend directly on ctx-pro packages")
PY

if grep -REn --include='*.rs' \
  '(^|[^[:alnum:]_])(clap::|ctx_cli::|ctx_agent[^[:alnum:]_]*::|ctx_pro[^[:alnum:]_]*::|identity::home_dir|CaptureProvider::Unknown)' \
  "${history_root}/src"; then
  echo 'final transport, identity, agent/pro, Clap, or unknown-provider authority leaked into ctx-history-cli' >&2
  exit 1
fi

if grep -En 'CaptureProvider::[A-Z]' "${final_provider_args}"; then
  echo 'provider vocabulary must not be duplicated in the final Clap/value-parser shell' >&2
  exit 1
fi

if grep -En 'ctx_history_capture::(discover_provider_sources|discover_provider_sources_for_provider_report|discover_provider_sources_report)' \
  "${final_provider_sources}"; then
  echo 'native discovery must be owned by ctx-history-cli, not its final compatibility wrapper' >&2
  exit 1
fi

printf 'ctx-history-cli vocabulary, discovery, and typed-provider boundary ok\n'
