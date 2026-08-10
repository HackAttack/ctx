//! Transport-neutral application queries over one pinned Core generation.
//!
//! This crate owns query-domain parsing, selector resolution, search planning,
//! lexical/semantic ranking, and bounded result contracts. Process lifecycle,
//! refresh execution, concrete semantic services, and UI rendering remain in
//! the outer composition layer.

mod filters;
mod lineage;
mod presentation;
mod search;
mod selector;
mod semantic;

pub use filters::*;
pub use lineage::*;
pub use presentation::*;
pub use search::*;
pub use selector::*;
pub use semantic::*;
