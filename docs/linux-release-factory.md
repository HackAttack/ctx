# Linux release factory

The public release has one construction path. A Linux x86_64 checkout builds
the five supported CLI binaries, signs and notarizes the two macOS binaries,
and emits all checksums and release evidence into one directory. Native hosts
validate those exact bytes afterwards.

## Inputs

- A clean checkout at the requested source commit.
- Rust 1.97.1 with the five release targets installed.
- A complete macOS SDK directory or archive supplied with `--macos-sdk` or
  `CTX_MACOS_SDK_ROOT`. The SDK is an unredistributed private build input.
- Offline OSV scanner/database inputs in official mode.
- The existing five Apple signing values in official mode.

Zig 0.15.2 and rcodesign 0.29.0 are downloaded from checksum-pinned upstream
archives into `target/release-toolchain`. cargo-zigbuild 0.23.0 must already be
installed; `CTX_RELEASE_ALLOW_TOOL_INSTALL=1` permits the factory to install
that pinned version.

## Commands

An official candidate:

```bash
scripts/release/build-public-candidate-on-linux.sh \
  --source-commit "$(git rev-parse HEAD)" \
  --macos-sdk /private/path/MacOSX.sdk.tar.gz
```

An unsigned, non-promotable local diagnostic:

```bash
scripts/release/build-public-candidate-on-linux.sh \
  --source-commit "$(git rev-parse HEAD)" \
  --macos-sdk /private/path/MacOSX.sdk.tar.gz \
  --diagnostic-unsigned --skip-runtimes
```

The factory refuses a dirty checkout, an unpinned toolchain, a missing SDK, or
missing official credentials. It builds targets with bounded parallelism; use
`--jobs` and `--build-parallelism` to tune a local machine.

## Native validation

Buildkite's five native jobs download artifacts from
`public-cli-linux-factory` and invoke:

```bash
scripts/validate-public-cli-factory-artifact.sh \
  PLATFORM target/public-cli-artifacts target/public-cli-native-smoke/PLATFORM
```

The validator checks the factory checksum, executes the native candidate, and
checks that validation did not mutate it. macOS additionally performs strict
native codesign verification. Semantic runtime smokes use the same CLI bytes.
Only after all five jobs pass does the staging job assemble GitHub assets.
