//! Provider-neutral local history source discovery.
//!
//! Provider-specific discovery remains outside this crate.  Capture supplies
//! the two fixed probe fragments required by the closed catalog below.

pub mod provider_sources;

pub use provider_sources::*;

#[cfg(test)]
mod test_support_paths;
