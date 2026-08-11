#!/usr/bin/env python3
"""Fail closed on the generic MCP application dependency boundary."""

from __future__ import annotations

import pathlib
import re
import sys
import tomllib


PACKAGE = "ctx-agent-application"
EXPECTED_LOCAL_DEPS = {"ctx-agent-integrations", "ctx-client-observability"}
ALLOWED_REVERSE_DEPENDENTS = {"ctx"}
REVIEW_CLOC_TARGET = 14_000
HARD_CLOC_LIMIT = 19_500


def fail(message: str) -> None:
    raise SystemExit(f"agent application boundary check failed: {message}")


def manifest_dependencies(data: dict) -> set[str]:
    dependencies: set[str] = set()

    def collect(table: object) -> None:
        if not isinstance(table, dict):
            return
        for alias, value in table.items():
            dependencies.add(value.get("package", alias) if isinstance(value, dict) else alias)

    for name in ("dependencies", "build-dependencies"):
        collect(data.get(name))
    for target in data.get("target", {}).values():
        if isinstance(target, dict):
            for name in ("dependencies", "build-dependencies"):
                collect(target.get(name))
    return dependencies


def find_cycle(graph: dict[str, set[str]]) -> list[str] | None:
    active: list[str] = []
    complete: set[str] = set()

    def visit(node: str) -> list[str] | None:
        if node in active:
            return active[active.index(node) :] + [node]
        if node in complete:
            return None
        active.append(node)
        for dependency in sorted(graph[node]):
            cycle = visit(dependency)
            if cycle:
                return cycle
        active.pop()
        complete.add(node)
        return None

    for node in sorted(graph):
        cycle = visit(node)
        if cycle:
            return cycle
    return None


def approximate_physical_cloc(paths: list[pathlib.Path]) -> int:
    count = 0
    in_block_comment = False
    for path in paths:
        for raw_line in path.read_text(encoding="utf-8").splitlines():
            line = raw_line.strip()
            if in_block_comment:
                if "*/" not in line:
                    continue
                line = line.split("*/", 1)[1].strip()
                in_block_comment = False
            while line.startswith("/*"):
                if "*/" not in line[2:]:
                    in_block_comment = True
                    line = ""
                    break
                line = line.split("*/", 1)[1].strip()
            if line and not line.startswith("//"):
                count += 1
    return count


def main() -> None:
    if len(sys.argv) != 4:
        fail("expected ROOT_CARGO_TOML APPLICATION_CARGO_TOML APPLICATION_BUILD")
    root_manifest = pathlib.Path(sys.argv[1]).resolve()
    application_manifest = pathlib.Path(sys.argv[2]).resolve()
    application_build = pathlib.Path(sys.argv[3]).resolve()
    root = root_manifest.parent
    workspace = tomllib.loads(root_manifest.read_text(encoding="utf-8"))["workspace"]
    manifests = [root / member / "Cargo.toml" for member in workspace["members"]]
    packages = {
        tomllib.loads(path.read_text(encoding="utf-8"))["package"]["name"]: path
        for path in manifests
    }
    if packages.get(PACKAGE) != application_manifest:
        fail("manifest is not the canonical workspace member")

    graph = {
        name: manifest_dependencies(tomllib.loads(path.read_text(encoding="utf-8"))).intersection(packages)
        for name, path in packages.items()
    }
    if cycle := find_cycle(graph):
        fail(f"workspace dependency cycle: {' -> '.join(cycle)}")
    if graph[PACKAGE] != EXPECTED_LOCAL_DEPS:
        fail(f"local dependencies are {sorted(graph[PACKAGE])}, expected {sorted(EXPECTED_LOCAL_DEPS)}")
    reverse = {name for name, dependencies in graph.items() if PACKAGE in dependencies}
    if reverse != ALLOWED_REVERSE_DEPENDENTS:
        fail(f"reverse dependents are {sorted(reverse)}, expected {sorted(ALLOWED_REVERSE_DEPENDENTS)}")

    build = application_build.read_text(encoding="utf-8")
    block = re.search(r"CTX_AGENT_APPLICATION_DEPS\s*=\s*\[(.*?)\]", build, re.DOTALL)
    if block is None:
        fail("missing CTX_AGENT_APPLICATION_DEPS inventory")
    bazel_local_deps = set(re.findall(r'"//crates/([^/:]+):lib"', block.group(1)))
    if bazel_local_deps != EXPECTED_LOCAL_DEPS:
        fail(f"Bazel local dependencies are {sorted(bazel_local_deps)}, expected {sorted(EXPECTED_LOCAL_DEPS)}")

    sources = sorted((application_manifest.parent / "src").rglob("*.rs"))
    forbidden_fragments = [
        'env!("CARGO_PKG_VERSION")',
        "AppConfig",
        "clap::",
        "ctx_cli::",
        "ctx_daemon_application",
        "ctx_daemon",
        "ctx_history_capture::",
        "ctx_history_index::",
        "ctx_history_query::",
        "ctx_history_read_application",
        "ctx_pro_",
        "ctx_semantic",
        "LocalToolBackend",
    ]
    for path in sources:
        body = path.read_text(encoding="utf-8")
        for fragment in forbidden_fragments:
            if fragment in body:
                fail(f"forbidden fragment {fragment!r} in {path.relative_to(root)}")

    stale = [
        "crates/ctx-cli/src/mcp/telemetry.rs",
        "crates/ctx-cli/src/mcp/telemetry/tests.rs",
        "crates/ctx-cli/src/mcp/tests.rs",
    ]
    remaining = [path for path in stale if (root / path).exists()]
    if remaining:
        fail(f"stale CLI MCP application authorities remain: {remaining}")

    required_authorities = {
        "crates/ctx-agent-application/src/mcp_tool_call.rs": ["invoke_mcp_tool_call"],
        "crates/ctx-agent-application/src/tool_backend/mod.rs": [
            "HistoryReadPort",
            "SearchReadinessPort",
            "SourceCatalogPort",
            "ExtensionToolPort",
        ],
    }
    for relative, symbols in required_authorities.items():
        path = root / relative
        if not path.is_file():
            fail(f"missing application authority {relative}")
        body = path.read_text(encoding="utf-8")
        missing = [symbol for symbol in symbols if symbol not in body]
        if missing:
            fail(f"application authority {relative} is missing {missing}")

    cli_backend = (root / "crates/ctx-cli/src/tool_backend/application.rs").read_text(
        encoding="utf-8"
    )
    stale_cli_orchestration = [
        symbol
        for symbol in ("fn execute_inner(", "ToolIntegrationReceipt {", "ToolTransportFacts::")
        if symbol in cli_backend
    ]
    if stale_cli_orchestration:
        fail(f"stale CLI tool orchestration remains: {stale_cli_orchestration}")

    cloc = approximate_physical_cloc(sources)
    if cloc >= HARD_CLOC_LIMIT:
        fail(f"physical Rust CLOC {cloc} is not below hard stop {HARD_CLOC_LIMIT}")
    review = "within review target" if cloc < REVIEW_CLOC_TARGET else "above review target"
    print(
        f"agent application boundary is acyclic with local deps {sorted(graph[PACKAGE])}; "
        f"physical Rust CLOC={cloc} ({review} <{REVIEW_CLOC_TARGET}; hard <{HARD_CLOC_LIMIT})"
    )


if __name__ == "__main__":
    main()
