use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_refresh::{verify_generation_query_authority, GenerationQueryAuthorityError};
use serde_json::{json, Value};

use crate::ui::{diagnostic, Action, Diagnostic, DiagnosticLevel, Field, RenderContext, Ui};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const ACTIVE_GENERATION_RACE_ERROR_CODE: &str = "generation_changed";
const ACTIVE_GENERATION_RACE_FAILURE_KIND: &str = "active_generation_race";
const ACTIVE_GENERATION_RACE_DETAIL: &str =
    "the active searchable generation changed while the command was opening it";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ActiveGenerationRaceCommand {
    Search,
    Show,
}

impl ActiveGenerationRaceCommand {
    const fn summary(self) -> &'static str {
        match self {
            Self::Search => "History changed during search",
            Self::Show => "History changed while opening this item",
        }
    }

    const fn retry_detail(self) -> &'static str {
        match self {
            Self::Search => {
                "A refresh published a new searchable generation while ctx was opening the previous one. Retry the same search command."
            }
            Self::Show => {
                "A refresh published a new searchable generation while ctx was opening the previous one. Retry the same show command."
            }
        }
    }
}

#[cfg(test)]
pub(super) use ctx_history_query::{resolve_core_event, resolve_session};
pub(super) use ctx_history_query::{
    resolve_core_event_with_refs, resolve_session_with_refs, validate_ctx_id,
    validate_session_selector, MissingLookupError, MissingLookupKind,
};

pub(super) fn resolve_lookup_for_output<T>(
    result: Result<T>,
    human_output: bool,
    recovery_command: &str,
    ui: &mut Ui,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if human_output => {
            let Some(missing) = error.downcast_ref::<MissingLookupError>() else {
                return Err(error);
            };
            let document = render_missing_lookup(ui.stderr_context(), missing, recovery_command);
            ui.write_stderr(&document)?;
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn render_active_generation_race<T>(
    result: Result<T>,
    json_output: bool,
    command: ActiveGenerationRaceCommand,
    ui: &mut Ui,
) -> Result<T> {
    match result {
        Ok(value) => Ok(value),
        Err(error) if is_active_generation_race(&error) => {
            if json_output {
                let encoded = serde_json::to_string(&active_generation_race_error_json())?;
                writeln!(ui.stderr_writer(), "{encoded}")?;
            } else {
                let document = diagnostic(
                    ui.stderr_context(),
                    Diagnostic {
                        level: DiagnosticLevel::Error,
                        summary: command.summary(),
                        detail: Some(command.retry_detail()),
                        fields: &[],
                        action: None,
                    },
                );
                ui.write_stderr(&document)?;
            }
            Err(crate::dispatch::rendered_cli_error())
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn is_active_generation_race(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<IndexError>(),
            Some(IndexError::ConcurrentGenerationChange)
        )
    })
}

pub(crate) fn active_generation_race_error_json() -> Value {
    json!({
        "error": format!(
            "{ACTIVE_GENERATION_RACE_ERROR_CODE}/{ACTIVE_GENERATION_RACE_FAILURE_KIND}"
        ),
        "error_code": ACTIVE_GENERATION_RACE_ERROR_CODE,
        "failure_kind": ACTIVE_GENERATION_RACE_FAILURE_KIND,
        "detail": ACTIVE_GENERATION_RACE_DETAIL,
        "retryable": true,
    })
}

pub(crate) fn generation_query_authority_error_json(
    error: &GenerationQueryAuthorityError,
) -> Value {
    let detail = error.to_string();
    json!({
        "error": detail.clone(),
        "error_code": error.error_code(),
        "detail": detail,
        "retryable": error.retryable(),
    })
}

pub(super) fn render_missing_lookup(
    context: &RenderContext,
    missing: &MissingLookupError,
    recovery_command: &str,
) -> crate::ui::Document {
    let (summary, detail, label) = match missing.kind() {
        MissingLookupKind::Event => (
            "Event not found",
            "This event is not in the current searchable generation. Search for text from the event, then retry with a returned event ID.",
            "Requested event",
        ),
        MissingLookupKind::Session => (
            "Session not found",
            "This session is not in the current searchable generation. Search for text from the session, then retry with a returned session ID.",
            "Requested session",
        ),
    };
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Error,
            summary,
            detail: Some(detail),
            fields: &[Field::new(label, missing.requested())],
            action: Some(Action {
                command: recovery_command,
            }),
        },
    )
}

pub(super) fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    let root = index_root(data_root);
    let index = match VerifiedIndex::open_pinned(&root) {
        Ok(index) => index,
        Err(ctx_history_index::IndexError::MissingActiveGenerationPointer) => {
            return Err(anyhow!(
                "the Core index does not exist; retry with daemon refresh enabled"
            ));
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("open verified Core index {}", root.display()));
        }
    };
    verify_generation_query_authority(&index).map_err(anyhow::Error::new)?;
    Ok(index)
}

pub(super) fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}
