"""Immutable identities and layouts for public Semantic release assets."""

from __future__ import annotations

SCHEMA_VERSION = 1
DOWNLOAD_USER_AGENT = "ctx-semantic-release-assets/1 (+https://ctx.rs)"
MODEL_VERSION = "1.0.0"
MODEL_ID = "intfloat/multilingual-e5-small"
MODEL_REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"
MODEL_REVISION_URL = f"https://huggingface.co/{MODEL_ID}/resolve/{MODEL_REVISION}"
MODEL_LICENSE_REVISION = "0e31c7c09737df491e7ff74ded19614b884c52b4"
MODEL_LICENSE_URL = (
    "https://raw.githubusercontent.com/microsoft/unilm/"
    f"{MODEL_LICENSE_REVISION}/LICENSE"
)
MODEL_LICENSE_SIZE = 1_104
MODEL_LICENSE_SHA256 = (
    "904dc4d8749877f1dba1cda48200d2462dccbeb7c134d5e4ef6fa75e0198c8fe"
)
MODEL_MAX_EXPANDED_BYTES = 768 * 1024 * 1024
MODEL_PATHS = (
    "LICENSE",
    "config.json",
    "manifest.json",
    "onnx/model.onnx",
    "special_tokens_map.json",
    "tokenizer.json",
    "tokenizer_config.json",
)
COMMON_MODEL_FILES = {
    "LICENSE": (MODEL_LICENSE_SIZE, MODEL_LICENSE_SHA256),
    "config.json": (
        655,
        "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    ),
    "special_tokens_map.json": (
        167,
        "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    ),
    "tokenizer.json": (
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
    "tokenizer_config.json": (
        443,
        "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    ),
}
MODEL_VARIANTS = {
    "cpu-fp32": {
        "asset_id": "onnx_model",
        "artifact": "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz",
        "upstream_onnx": "onnx/model.onnx",
        "onnx_size": 470_268_510,
        "onnx_sha256": "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    },
    "accelerator-o4-fp16": {
        "asset_id": "onnx_model_o4_fp16",
        "artifact": "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz",
        "upstream_onnx": "onnx/model_O4.onnx",
        "onnx_size": 235_052_531,
        "onnx_sha256": "4654c156f3e4171abc9c716cdb771bf9116455d15ac1aab364aeeede0e3205b0",
    },
}
EXPECTED_ASSET_IDS = {
    "apple_coreml",
    "linux_aarch64_cpu",
    "linux_x64_cpu",
    "linux_cuda12",
    "macos_arm64_cpu",
    "macos_x64_cpu",
    "onnx_model",
    "onnx_model_o4_fp16",
    "windows_ml",
}
CPU_RUNTIME_FILES = (
    "GIT_COMMIT_ID",
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
)
CUDA_FILES = (
    *CPU_RUNTIME_FILES,
    "NVIDIA-CUDA-LICENSE.txt",
    "NVIDIA-CUDNN-LICENSE.txt",
    "lib/libcublas.so.12",
    "lib/libcublasLt.so.12",
    "lib/libcudart.so.12",
    "lib/libcudnn.so.9",
    "lib/libcudnn_graph.so.9",
    "lib/libcudnn_ops.so.9",
    "lib/libcufft.so.11",
    "lib/libcurand.so.10",
    "lib/libnvrtc.so.12",
    "lib/libonnxruntime.so",
    "lib/libonnxruntime_providers_cuda.so",
    "lib/libonnxruntime_providers_shared.so",
)
WINDOWS_ML_FILES = (
    "LICENSE",
    "ThirdPartyNotices.txt",
    "lib/DirectML.dll",
    "lib/Microsoft.Windows.AI.MachineLearning.dll",
    "lib/onnxruntime.dll",
)
# The immutable July bundle is the offline producer input. Publication pins
# bind the regenerated bundle after the locked host identity is embedded in
# PROVENANCE.json; the two identities intentionally differ.
COREML_SOURCE_ARCHIVE_SHA256 = (
    "94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e"
)
COREML_SOURCE_ARCHIVE_SIZE = 423_600_648
COREML_SOURCE_MANIFEST_SHA256 = (
    "576c68756563333fdf442e6859f2392ca0065b09a2cb5d73983e30de75df1ad6"
)
COREML_PUBLICATION_ARCHIVE_SHA256 = (
    "25fbf333d1e72f5c075973ef968dfa1446459f61f3ac63ef3690d9865435af17"
)
COREML_PUBLICATION_ARCHIVE_SIZE = 423_625_016
COREML_PUBLICATION_MANIFEST_SHA256 = (
    "20a94162aca7c2f9f65be27839cd6867ec1c54e142fdf0c652de20139dffbc19"
)
COREML_ARCHIVE_NAME = "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"
COREML_SOURCE_ARCHIVE_URL = (
    "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/"
    + COREML_ARCHIVE_NAME
)
COREML_ARCHIVE_ROOT = COREML_ARCHIVE_NAME.removesuffix(".tar.xz")
COREML_SOURCE_PATHS = {
    "tokenizer": "tokenizer.json",
    "document_model": "document.mlpackage",
    "query_model": "query.mlpackage",
    "model_license": "LICENSES/MODEL_LICENSE.txt",
}
COREML_MAX_DIRECTORIES = 1_024
EXPECTED_ASSETS = {
    "onnx_model": {
        "role": "model",
        "backend": "onnx",
        "version": MODEL_VERSION,
        "platform": "any",
        "artifact": MODEL_VARIANTS["cpu-fp32"]["artifact"],
        "archive_format": "tar.xz",
        "archive_path_prefix": MODEL_VARIANTS["cpu-fp32"]["artifact"].removesuffix(
            ".tar.xz"
        ),
        "max_expanded_bytes": 603_979_776,
        "max_files": 16,
        "files": MODEL_PATHS,
    },
    "onnx_model_o4_fp16": {
        "role": "model",
        "backend": "onnx",
        "version": MODEL_VERSION,
        "platform": "any",
        "artifact": MODEL_VARIANTS["accelerator-o4-fp16"]["artifact"],
        "archive_format": "tar.xz",
        "archive_path_prefix": MODEL_VARIANTS["accelerator-o4-fp16"][
            "artifact"
        ].removesuffix(".tar.xz"),
        "max_expanded_bytes": 335_544_320,
        "max_files": 16,
        "files": MODEL_PATHS,
    },
    "apple_coreml": {
        "role": "accelerator",
        "backend": "coreml",
        "version": MODEL_VERSION,
        "platform": "macos-arm64",
        "artifact": COREML_ARCHIVE_NAME,
        "archive_format": "tar.xz",
        "archive_path_prefix": COREML_ARCHIVE_ROOT,
        "max_expanded_bytes": 2_147_483_648,
        "max_files": 4096,
        "files": None,
    },
    "linux_x64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "linux-x64",
        "artifact": "ctx-onnxruntime-linux-x64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 67_108_864,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.so"),
    },
    "linux_aarch64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "linux-aarch64",
        "artifact": "ctx-onnxruntime-linux-aarch64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 67_108_864,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.so"),
    },
    "macos_arm64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "macos-arm64",
        "artifact": "ctx-onnxruntime-macos-arm64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 100_663_296,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.dylib"),
    },
    "macos_x64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "macos-x64",
        "artifact": "ctx-onnxruntime-macos-x64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 100_663_296,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.dylib"),
    },
    "windows_ml": {
        "role": "cpu-runtime",
        "backend": "windows-ml",
        "version": "2.1.74",
        "platform": "windows-x64",
        "artifact": "ctx-windowsml-windows-x64.zip",
        "archive_format": "zip",
        "archive_path_prefix": "",
        "max_expanded_bytes": 50_331_648,
        "max_files": 5,
        "files": WINDOWS_ML_FILES,
    },
    "linux_cuda12": {
        "role": "accelerator",
        "backend": "ort-cuda",
        "version": "1.27.0",
        "platform": "linux-x64-cuda12",
        "artifact": "ctx-onnxruntime-linux-x64-cuda12.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 2_147_483_648,
        "max_files": 18,
        "files": tuple(sorted(CUDA_FILES)),
    },
}
