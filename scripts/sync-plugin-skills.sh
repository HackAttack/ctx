#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

source_skill="skills/ctx/SKILL.md"
plugin_skill="plugins/ctx/skills/ctx/SKILL.md"

usage() {
  cat <<'USAGE'
usage: scripts/sync-plugin-skills.sh --check
       scripts/sync-plugin-skills.sh --write

Checks or updates copied plugin skill files that intentionally mirror the
standalone public skill source.
USAGE
}

mode="${1:---check}"
if (( "$#" > 1 )); then
  usage >&2
  exit 2
fi

case "${mode}" in
  --check)
    if ! diff -u "${source_skill}" "${plugin_skill}" >/dev/null; then
      printf 'plugin skill copy differs from public skill source\n' >&2
      printf 'run scripts/sync-plugin-skills.sh --write to update it\n' >&2
      exit 1
    fi
    ;;
  --write)
    cp "${source_skill}" "${plugin_skill}"
    ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    printf 'unknown mode: %s\n' "${mode}" >&2
    usage >&2
    exit 2
    ;;
esac

printf 'plugin skill sync ok\n'
