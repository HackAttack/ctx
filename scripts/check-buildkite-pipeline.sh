#!/usr/bin/env bash
set -euo pipefail

pipeline=".buildkite/pipeline.yml"
test -f "${pipeline}"

if command -v ruby >/dev/null 2>&1; then
  ruby -e '
    require "yaml"
    data = YAML.load_file(ARGV.fetch(0))
    abort "pipeline must have steps" unless data.is_a?(Hash) && data["steps"].is_a?(Array)
    steps = data["steps"]
    abort "pipeline should include public smoke and gated artifact matrix" unless steps.length == 7
    smoke = steps.fetch(0)
    abort "pipeline step must be a mapping" unless smoke.is_a?(Hash)
    abort "pipeline public smoke step must be keyed" unless smoke.key?("key")
    abort "missing public-smoke step" unless smoke["key"] == "public-smoke"
    abort "public-smoke must use the release-linux-managed queue" unless smoke.dig("agents", "queue") == "release-linux-managed"
    abort "public-smoke must use the release-linux-control runner" unless smoke.dig("agents", "ctx-runner-class") == "release-linux-control"
    command = smoke["command"].to_s
    abort "public-smoke must run scripts/check.sh --mode=ci" unless command.include?("./scripts/check.sh --mode=ci")
    abort "public-smoke must install missing Ubuntu runner packages before Bazel tests" unless command.include?("apt-get install -y")
    abort "public-smoke must verify runner tools before Bazel tests" unless command.include?("command -v \"$${tool_binary}\"")
    required_keys = %w[
      public-cli-linux-x64
      public-cli-windows-x64
      public-cli-freebsd-x64
      public-cli-macos-arm64
      public-cli-macos-x64
    ]
    actual_keys = steps.filter_map { |step| step["key"] if step.is_a?(Hash) }
    required_keys.each { |key| abort "missing gated artifact step #{key}" unless actual_keys.include?(key) }
    steps.drop(2).each do |step|
      next unless step.is_a?(Hash)
      abort "artifact step #{step["key"]} must be gated" unless step["if"].to_s.include?("CTX_PUBLIC_CLI_ARTIFACT_MATRIX")
    end
    %w[public-cli-macos-arm64 public-cli-macos-x64].each do |key|
      step = steps.find { |candidate| candidate.is_a?(Hash) && candidate["key"] == key }
      abort "missing macOS artifact step #{key}" unless step
      abort "#{key} must cross-build on release-linux-managed" unless step.dig("agents", "queue") == "release-linux-managed"
      abort "#{key} must run on linux" unless step.dig("agents", "os") == "linux"
      abort "#{key} must run on x86_64" unless step.dig("agents", "arch") == "x86_64"
      abort "#{key} must not serialize on the Mac GUI queue" if step.key?("concurrency_group")
    end
  ' "${pipeline}"
else
  top_level_steps="$(
    awk '
      /^steps:[[:space:]]*$/ { in_steps = 1; next }
      /^[^[:space:]]/ { in_steps = 0 }
      in_steps && /^  -[[:space:]]/ { count++ }
      END { print count + 0 }
    ' "${pipeline}"
  )"
  if [[ "${top_level_steps}" != "7" ]]; then
    printf 'pipeline should include public smoke and gated artifact matrix\n' >&2
    exit 1
  fi
fi

for required in \
  'key: "public-smoke"' \
  'queue: "release-linux-managed"' \
  'ctx-runner-class: "release-linux-control"' \
  'ensure_runner_tool zip zip' \
  'ensure_runner_tool rg ripgrep' \
  './scripts/check.sh --mode=ci' \
  'CTX_PUBLIC_CLI_ARTIFACT_MATRIX' \
  'scripts/build-public-cli-artifact.sh linux-x64' \
  'scripts/build-public-cli-artifact.sh windows-x64' \
  'scripts/build-public-cli-artifact.sh freebsd-x64' \
  'scripts/build-public-cli-artifact.sh macos-arm64' \
  'scripts/build-public-cli-artifact.sh macos-x64' \
  'cargo zigbuild -p ctx --release --target "${target}" --locked' \
  'CARGO_ZIGBUILD_VERSION' \
  'ZIG_LINUX_X64_SHA256'; do
  if ! grep -F -q "${required}" "${pipeline}"; then
    if ! grep -F -q "${required}" scripts/build-public-cli-artifact.sh; then
      printf 'pipeline or artifact script missing required snippet: %s\n' "${required}" >&2
      exit 1
    fi
    continue
  fi
done

if grep -F -q 'ctx-mac-gui-shared-arm64' "${pipeline}"; then
  printf 'public CLI artifact matrix must not use the scarce Mac GUI queue\n' >&2
  exit 1
fi

for mac_step in public-cli-macos-arm64 public-cli-macos-x64; do
  for required in \
    'queue: "release-linux-managed"' \
    'ctx-runner-class: "release-linux-control"' \
    'os: "linux"' \
    'arch: "x86_64"'; do
    if ! awk '
        index($0, "key: \"" step "\"") { in_step = 1 }
        in_step && /^  - label:/ && index($0, step) == 0 { in_step = 0 }
        in_step && index($0, needle) { found = 1 }
        END { exit found ? 0 : 1 }
      ' step="${mac_step}" needle="${required}" "${pipeline}"; then
      printf '%s artifact step missing required Linux runner snippet: %s\n' "${mac_step}" "${required}" >&2
      exit 1
    fi
  done
done

if grep -E -q 'release-artifact|r2-|provider-live|OpenRouter|completion-certificate|freebsd-native-release-proof|CTX_PUBLIC_CLI_PERF_GATES|--mode=perf|public-perf' "${pipeline}"; then
  printf 'pipeline contains non-smoke release or provider-live wiring\n' >&2
  exit 1
fi

printf 'Buildkite pipeline check ok\n'
