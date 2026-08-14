#[path = "codebuddy/native_path.rs"]
pub mod native_path;
#[path = "codebuddy/normalization.rs"]
mod normalization;
#[path = "codebuddy/source.rs"]
mod source;

const CODEBUDDY_CAPTURE_REVISION: u32 = 5;
const CODEBUDDY_CLI_POLICY_REVISION: u32 = 7;
const CODEBUDDY_MAX_METADATA_TEXT_BYTES: usize = 8 * 1024;
const CODEBUDDY_MAX_FAILURE_BYTES: usize = 2 * 1024;
const CODEBUDDY_MAX_SCAN_REJECTIONS: usize = 64;
