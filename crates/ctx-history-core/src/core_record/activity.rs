use serde::{Deserialize, Serialize};

use crate::TypedKey;

use super::{
    validation::{validate_count, validate_optional_text, validate_text},
    CoreRecordError, CoreRecordResult, MAX_CORE_CONTENT_BYTES, MAX_TEXT_METADATA_BYTES,
};

/// Revision of the repository-neutral provider activity boundary.
pub const CORE_ACTIVITY_REVISION: u32 = 1;

/// Maximum literal provider-declared facts retained on one event.
pub const MAX_PROVIDER_DECLARED_FACTS: usize = 4_096;

/// One event-local provider activity envelope.
///
/// Providers retain native event granularity. Separate invocation and result
/// records use the same exact `provider_call_id`; a provider that emits a
/// combined terminal record may carry both members. `facts` remain in provider
/// order and are neither sorted nor deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreActivity {
    pub revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_call_id: Option<TypedKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<ActivityInvocation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ActivityResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<ProviderDeclaredFact>,
}

/// Exact provider-declared invocation content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityInvocation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    pub tool: String,
    pub arguments: ActivityJsonCapture,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_unix_ms: Option<i64>,
}

/// Exact provider-declared terminal result content.
///
/// `status` is an uninterpreted provider string. Text and structured channels
/// independently preserve complete content or an explicit capture disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ns: Option<u64>,
    pub text: ActivityTextCapture,
    pub structured_content: ActivityJsonCapture,
}

/// Exhaustive categories of literal source facts admitted to public Core.
///
/// These categories identify where a literal came from; they do not certify a
/// location, object, association, effect, or causal relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiteralFactKind {
    SessionCwd,
    ToolWorkdir,
    File,
    Url,
    Forge,
    Project,
    Vcs,
    Commit,
    PullRequest,
    Command,
    Branch,
    Workspace,
    ProviderDisposition,
}

impl LiteralFactKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionCwd => "session_cwd",
            Self::ToolWorkdir => "tool_workdir",
            Self::File => "file",
            Self::Url => "url",
            Self::Forge => "forge",
            Self::Project => "project",
            Self::Vcs => "vcs",
            Self::Commit => "commit",
            Self::PullRequest => "pull_request",
            Self::Command => "command",
            Self::Branch => "branch",
            Self::Workspace => "workspace",
            Self::ProviderDisposition => "provider_disposition",
        }
    }
}

/// One literal categorized value declared by the provider.
///
/// The value is not trimmed, normalized, path-resolved, effect-classified, or
/// interpreted. Keeping a vector preserves source order and repeated claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDeclaredFact {
    pub kind: LiteralFactKind,
    pub value: String,
}

/// Complete JSON capture or an explicit reason no complete value exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capture_status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivityJsonCapture {
    Present {
        value: serde_json::Value,
    },
    Absent,
    Unavailable,
    Omitted {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_encoded_bytes: Option<u64>,
    },
}

/// Complete text capture, a reference to the record body, or an explicit
/// reason no complete value exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capture_status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActivityTextCapture {
    Present {
        value: String,
    },
    NormalizedBody,
    Absent,
    Unavailable,
    Omitted {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_bytes: Option<u64>,
    },
}

impl CoreActivity {
    pub(super) fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        if self.revision != CORE_ACTIVITY_REVISION
            || self.invocation.is_none() && self.result.is_none() && self.facts.is_empty()
        {
            return Err(CoreRecordError::InvalidActivity);
        }
        if let Some(call_id) = &self.provider_call_id {
            call_id
                .validate_contract()
                .map_err(|_| CoreRecordError::InvalidActivity)?;
        } else if self.invocation.is_some() || self.result.is_some() {
            return Err(CoreRecordError::InvalidActivity);
        }
        if let Some(invocation) = &self.invocation {
            invocation.validate_contract()?;
        }
        if let Some(result) = &self.result {
            result.validate_contract(normalized_body)?;
        }
        validate_count(
            "provider_declared_facts",
            self.facts.len(),
            MAX_PROVIDER_DECLARED_FACTS,
        )?;
        for fact in &self.facts {
            fact.validate_contract()?;
        }
        Ok(())
    }
}

impl ActivityInvocation {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_optional_text(
            "activity.invocation.protocol",
            self.protocol.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_optional_text(
            "activity.invocation.server",
            self.server.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        validate_text(
            "activity.invocation.tool",
            &self.tool,
            MAX_TEXT_METADATA_BYTES,
        )?;
        self.arguments.validate_contract()
    }
}

impl ActivityResult {
    fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        validate_optional_text(
            "activity.result.status",
            self.status.as_deref(),
            MAX_TEXT_METADATA_BYTES,
        )?;
        self.text.validate_contract(normalized_body)?;
        self.structured_content.validate_contract()
    }
}

impl ProviderDeclaredFact {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "provider_declared_fact.value",
            &self.value,
            MAX_CORE_CONTENT_BYTES,
        )
    }
}

impl ActivityJsonCapture {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        if let Self::Omitted { reason, .. } = self {
            validate_text(
                "activity.json_capture.omission_reason",
                reason,
                MAX_TEXT_METADATA_BYTES,
            )?;
        }
        Ok(())
    }
}

impl ActivityTextCapture {
    fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        match self {
            Self::Present { value } => {
                validate_text("activity.result.text", value, MAX_CORE_CONTENT_BYTES)
            }
            Self::NormalizedBody if normalized_body.is_none_or(str::is_empty) => {
                Err(CoreRecordError::InvalidActivity)
            }
            Self::Omitted { reason, .. } => validate_text(
                "activity.text_capture.omission_reason",
                reason,
                MAX_TEXT_METADATA_BYTES,
            ),
            Self::NormalizedBody | Self::Absent | Self::Unavailable => Ok(()),
        }
    }
}
