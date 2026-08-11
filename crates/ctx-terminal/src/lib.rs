//! Terminal-safe documents, measured output, and neutral progress presentation.
//!
//! This crate deliberately has no knowledge of CLI parsing or application
//! domains. Composition layers map their arguments and refresh state here.

pub mod output;
pub mod presentation_limit;
pub mod progress;
pub mod ui;

pub use output::*;
pub use presentation_limit::*;
pub use progress::*;
pub use ui::*;
