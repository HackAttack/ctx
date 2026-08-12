//! Provider-neutral source-backed capture runtime.

mod diagnostics;
mod driver;
mod parallel;
mod resources;

pub use diagnostics::*;
pub use driver::*;
pub use parallel::*;
pub use resources::*;

pub use ctx_history_capture_model::{
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedRecordProgressDelta,
};
use ctx_history_core::{CertifiedSourceDeletion, CertifiedSourceInventory};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedCertifiedRemoval {
    pub deletion: CertifiedSourceDeletion,
    pub inventory: CertifiedSourceInventory,
}
