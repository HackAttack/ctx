//! Shared human-terminal rendering foundation.
//!
//! Machine-readable output deliberately bypasses this module. Commands will
//! migrate to this internal surface separately; keeping it registered but
//! otherwise unused in this patch lets the foundation land without changing
//! existing output contracts.

mod bootstrap;
mod components;
mod context;
mod document;
mod glyph;
mod style;
mod writer;

pub use bootstrap::{bootstrap_color_choice, scan_color_mode, scan_machine_output_hint};
pub use components::{
    diagnostic, empty_state, evidence_list, fields, hint, is_copyable_atom, outcome, progress,
    refresh_progress, section, table, Action, Diagnostic, DiagnosticLevel, EmptyState, Evidence,
    Field, Hint, Outcome, OutcomeState, Progress, RefreshCurrentSourceProgress,
    RefreshCurrentSourceProgressStage, RefreshLogicalPhase, RefreshLogicalStatus, RefreshProgress,
    RefreshProgressSnapshot, RefreshRequestState, RefreshStatusKind, RefreshStructuredOutcome,
    RefreshWholeRunStage, Table,
};
pub use context::{ColorMode, RenderContext, StreamKind, TestContext};
pub use document::{sanitize_untrusted_history_body_for_terminal, Document, Line, Span};
pub use style::{trim_terminal_line_ends, Token};
pub use writer::{LiveOutput, Ui};

/// Estimates one logical human result in a fixed, unbounded plain context.
///
/// This is useful for deterministic component tests and local size decisions.
/// Dispatch measures the actual wrapped, styled stdout and stderr bytes used
/// for runtime delivery accounting.
pub fn canonical_human_output_bytes(render: impl FnOnce(&RenderContext) -> Document) -> usize {
    render(&RenderContext::canonical_human_measurement())
        .render_plain()
        .len()
}

#[cfg(test)]
mod tests;
