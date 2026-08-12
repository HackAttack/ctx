# Semantic Core ML Model Bundle

This directory produces and verifies the public
`intfloat/multilingual-e5-small` fp16 Core ML bundle. Packaging is offline: it
accepts already-converted `.mlpackage` directories, a pinned `tokenizer.json`,
and the upstream model license. It does not read credentials, private evals, or
machine-specific paths into the bundle.

The public release step prepares its Core ML inputs from the immutable archive
at
`https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz`.
`semantic-release-assets.py prepare-coreml` requires its pinned 423,600,648-byte
size and `94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e`
SHA-256 before safely extracting the tokenizer, document/query packages, and
model license. The producer below remains an offline packaging step.

## Reproducible production

1. Use the pinned Buildkite macOS Tahoe 26.3.1 hosted image with Xcode 26.2.
   `run-pinned-producer.sh` downloads checksum-pinned CPython 3.11.9 and `uv`,
   then installs the exact top-level distributions in `requirements.lock` into
   that isolated interpreter. The producer verifies every locked version.
2. Resolve the model only at revision
   `614241f622f53c4eeff9890bdc4f31cfecc418b3`; retain its `tokenizer.json`
   and source weights plus the Microsoft MIT model license locally. Convert
   with the repository's pinned-revision converter and explicit fixed dimensions:

```bash
python scripts/convert-e5-coreml.py /artifacts/document.mlpackage \
  --batch 16 --sequence 512 --precision fp16
python scripts/convert-e5-coreml.py /artifacts/query.mlpackage \
  --batch 1 --sequence 512 --precision fp16
```

3. Run the producer with the same fixed tensor dimensions:

```bash
scripts/semantic-model-bundle/run-pinned-producer.sh \
  --tokenizer /public-snapshot/tokenizer.json \
  --document-model /artifacts/document.mlpackage \
  --query-model /artifacts/query.mlpackage \
  --model-license /public-snapshot/LICENSE \
  --bundle-version 1.0.0 \
  --document-batch-size 16 --query-batch-size 1 --sequence-length 512 \
  --output-dir /artifacts/ctx-e5-coreml-1.0.0 \
  --archive /artifacts/ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz
```

The document and query packages have distinct fixed contracts: document inputs
are `[16, 512]` with output `[16, 384]`, while query inputs are `[1, 512]` with
output `[1, 384]`. The producer validates each package against its role. To
produce a bundle without a separate query package, omit both `--query-model`
and `--query-batch-size`; the manifest then omits `query_batch_size`, and the
runtime uses the document model for queries.

The producer rejects tool-version drift, symlinks, unknown source revisions,
and existing output paths. Tar member order, modes, owners, and mtimes are
normalized, so identical inputs produce an identical manifest and archive. It
also emits `<archive>.asset.json`, the canonical signed-catalog input for the
validated Core ML archive.

Verify without importing Core ML or loading model weights:

```bash
python scripts/semantic-model-bundle/verify.py /artifacts/ctx-e5-coreml-1.0.0
```

The manifest intentionally excludes itself from `files`; every other regular
file is listed with its complete lowercase SHA-256 and exact byte size. Empty
directories, symlinks, special files, unlisted payloads, and paths outside the
small allowlist are rejected.

## ONNX model release archives

The CPU and accelerator ONNX archives use the same pinned model revision and
tokenizer/config files. Prepare each exact source from Hugging Face revision
`614241f622f53c4eeff9890bdc4f31cfecc418b3`, then construct its deterministic
archive offline:

```bash
python scripts/semantic-release-assets.py prepare-model \
  --variant cpu-fp32 --output-dir /public/fp32-snapshot
python scripts/semantic-release-assets.py build-model \
  --variant cpu-fp32 --source /public/fp32-snapshot --output-dir /artifacts
python scripts/semantic-release-assets.py prepare-model \
  --variant accelerator-o4-fp16 --output-dir /public/o4-fp16-snapshot
python scripts/semantic-release-assets.py build-model \
  --variant accelerator-o4-fp16 \
  --source /public/o4-fp16-snapshot --output-dir /artifacts
```

The CPU object is upstream `onnx/model.onnx`; the exact accelerator object is
upstream `onnx/model_O4.onnx`. Every downloaded file is admitted only at its
pinned byte size and SHA-256. The model license comes from the immutable
Microsoft/unilm revision `0e31c7c09737df491e7ff74ded19614b884c52b4`.

These commands produce and re-validate
`ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz` and
`ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz`, their checksums, and
canonical asset records. Only the `prepare-*` commands use the network.

After all model and runtime builders have populated one artifact directory,
`scripts/construct-semantic-release-catalog.sh` independently validates every
archive and emits the six `CTX_RELEASE_SEMANTIC_*` fields that are appended to
the release metadata before that complete metadata file is signed.

The public release matrix gathers the ten archives, their checksums, and their
canonical `.asset.json` records with
`scripts/stage-semantic-release-handoff.sh`. That handoff includes
`semantic-release.env`; release assembly passes the unsigned base metadata,
that field file, and a new output path to
`scripts/append-semantic-release-metadata.sh`. A trusted release environment
validates the handoff and signs the resulting complete metadata bytes. These
helpers stop after producing unsigned release inputs.

CUDA archive validation includes a static `DT_NEEDED` closure check across the
bundled ELF libraries. A real NVIDIA GPU execution-provider model session is
intentionally deferred to release qualification; it is not a producer or CI
requirement in this pass.
