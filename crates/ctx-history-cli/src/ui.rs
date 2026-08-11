//! Terminal presentation types used through [`crate::TerminalPort`].

pub use ctx_terminal::{
    canonical_human_output_bytes, diagnostic, is_copyable_atom, Action, Diagnostic,
    DiagnosticLevel, Document, Field, Line, RenderContext, Span, Token, Ui,
};

#[cfg(test)]
pub use ctx_terminal::{ColorMode, StreamKind, TestContext};
