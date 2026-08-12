#!/usr/bin/env bash
set -euo pipefail

runfiles_root="${TEST_SRCDIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)}"
repo_root="${runfiles_root}/${TEST_WORKSPACE:-}"
[[ -d "${repo_root}/scripts/semantic-model-bundle" ]] ||
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
launcher="${repo_root}/scripts/semantic-model-bundle/run-pinned-producer.sh"
lock="${repo_root}/scripts/semantic-model-bundle/toolchain.lock.json"
requirements="${repo_root}/scripts/semantic-model-bundle/requirements.lock"
pipeline="${repo_root}/.buildkite/pipeline.yml"

output="$(CTX_SEMANTIC_COREML_TOOLCHAIN_DRY_RUN=1 "${launcher}")"
grep -Fxq 'python_sha256=cbdac9462bab9671c8e84650e425d3f43b775752a930a2ef954a0d457d5c00c3' <<<"${output}"
grep -Fxq 'uv_sha256=c233bee389c15fdef09a6028db61cc54a12e6171f27d6d9c018eedca5bbbd011' <<<"${output}"
grep -Fq -- '--exact' "${launcher}"
grep -Fq -- 'pip check' "${launcher}"
grep -Fq -- '--no-deps' "${launcher}"
grep -Fq -- '--require-hashes' "${launcher}"
for package in coremltools huggingface-hub numpy safetensors sentencepiece tokenizers torch transformers; do
  grep -Eq "^${package}==" "${requirements}"
done

python3 - "${lock}" <<'PY'
import json
import sys

lock = json.load(open(sys.argv[1], encoding="utf-8"))
assert lock["macos"] == "26.3.1"
assert lock["xcode"] == "26.2"
assert lock["python"] == "3.11.9"
PY

python3 - "${pipeline}" <<'PY'
import sys

source = open(sys.argv[1], encoding="utf-8").read()
start = source.index('key: "semantic-coreml-archive"')
end = source.find('\n  - label:', start)
block = source[start:] if end < 0 else source[start:end]
assert "scripts/semantic-model-bundle/run-pinned-producer.sh" in block
assert "python3 scripts/semantic-model-bundle/produce.py" not in block
assert 'queue: "ctx-release-macos-arm64"' in block
assert 'os: "darwin"' in block
assert 'arch: "arm64"' in block
PY

printf 'Semantic Core ML pinned toolchain contract ok\n'
