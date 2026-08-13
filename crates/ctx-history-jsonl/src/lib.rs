#[cfg(test)]
mod exact_json_tests;
mod framing;
mod io_error;
mod mcp_exchange;
mod model;
mod occurrence;
mod pending_exchange;
mod physical;
mod source_identity;
mod terminal_authority;

pub use ctx_history_capture_model::{
    exact_bounded_string_alias, exact_json_value, raw_object_keys_are_unique, ExactJsonStringAlias,
};
pub use framing::*;
pub use io_error::*;
pub use mcp_exchange::*;
pub use model::*;
pub use occurrence::*;
pub use pending_exchange::*;
pub use physical::*;
pub use source_identity::*;
pub use terminal_authority::*;

#[cfg(test)]
mod test_support_paths {
    pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new().prefix("ctx-jsonl-").tempdir()
    }
}
