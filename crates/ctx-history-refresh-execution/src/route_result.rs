use super::*;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshRouteResult {
    pub route_identity: String,
    pub outcome: SourceBackedRefreshRouteOutcome,
    /// Exact number of source-level failures observed inside this route.
    pub source_failure_total: usize,
    /// Exact retryable subset of `source_failure_total`. This remains exact
    /// even when human-readable source diagnostics are bounded.
    pub source_retryable_failure_total: usize,
    /// Bounded details owned by this route result; omitted detail is derived
    /// from `source_failure_total` and this vector's length.
    pub source_failures: Vec<SourceBackedRefreshSourceFailure>,
    /// Exact rejected-record cardinality in the route's committed sources.
    pub rejected_record_total: u64,
    /// Bounded path/line/payload diagnostics owned by this route result.
    pub rejection_diagnostics: Vec<SourceBackedRefreshRecordRejection>,
}

impl SourceBackedRefreshRouteResult {
    pub fn succeeded(route_identity: String, changed: bool) -> Self {
        Self {
            route_identity,
            outcome: SourceBackedRefreshRouteOutcome::Succeeded { changed },
            source_failure_total: 0,
            source_retryable_failure_total: 0,
            source_failures: Vec::new(),
            rejected_record_total: 0,
            rejection_diagnostics: Vec::new(),
        }
    }

    pub fn failed(route_identity: String, class: String, carried_forward: bool) -> Self {
        let source_retryable_failure_total =
            usize::from(matches!(class.as_str(), "unavailable" | "source_changed"));
        Self {
            route_identity,
            outcome: SourceBackedRefreshRouteOutcome::Failed {
                class,
                carried_forward,
            },
            source_failure_total: 1,
            source_retryable_failure_total,
            source_failures: Vec::new(),
            rejected_record_total: 0,
            rejection_diagnostics: Vec::new(),
        }
    }

    pub fn has_source_failures(&self) -> bool {
        self.source_failure_total != 0
    }

    #[doc(hidden)]
    pub fn validate_source_failures(&self) -> Result<()> {
        if self.source_failures.len() > self.source_failure_total {
            bail!("terminal route result has more diagnostics than source failures");
        }
        if self.source_retryable_failure_total > self.source_failure_total {
            bail!("terminal route result has more retryable failures than source failures");
        }
        let retained_retryable = self
            .source_failures
            .iter()
            .filter(|failure| matches!(failure.class.as_str(), "unavailable" | "source_changed"))
            .count();
        let retained_non_retryable = self.source_failures.len() - retained_retryable;
        if retained_retryable > self.source_retryable_failure_total
            || retained_non_retryable
                > self
                    .source_failure_total
                    .saturating_sub(self.source_retryable_failure_total)
        {
            bail!("terminal route result has inconsistent retryable failure totals");
        }
        if self.outcome.is_failure() && self.source_failure_total == 0 {
            bail!("failed terminal route result has no source failure count");
        }
        if let Some(class) = self.outcome.failure_class() {
            let expected_retryable = if matches!(class, "unavailable" | "source_changed") {
                self.source_failure_total
            } else {
                0
            };
            if self.source_retryable_failure_total != expected_retryable {
                bail!("failed terminal route result has inconsistent retryability");
            }
        }
        let mut diagnostics = BTreeSet::new();
        for failure in &self.source_failures {
            if failure.route_identity != self.route_identity
                || !is_sha256_identity(&failure.source_identity)
                || !source_failure_class_is_typed(&failure.class)
                || failure.provider.is_empty()
                || failure.source_selector.is_empty()
                || failure.detail.is_empty()
                || !diagnostics.insert((
                    failure.source_identity.as_str(),
                    failure.provider.as_str(),
                    failure.class.as_str(),
                    failure.carried_forward,
                    failure.source_selector.as_str(),
                    failure.detail.as_str(),
                ))
            {
                bail!("terminal route result contains an inconsistent source diagnostic");
            }
        }
        if self.outcome.is_failure()
            && (self.rejected_record_total != 0 || !self.rejection_diagnostics.is_empty())
        {
            bail!("failed terminal route result contains successful-route rejections");
        }
        if self.rejection_diagnostics.len() as u64 > self.rejected_record_total {
            bail!("terminal route result has more rejection diagnostics than rejected records");
        }
        let mut rejections = BTreeSet::new();
        for rejection in &self.rejection_diagnostics {
            if rejection.route_identity != self.route_identity
                || !is_sha256_identity(&rejection.source_identity)
                || rejection.provider.is_empty()
                || rejection.source_selector.is_empty()
                || rejection.line == 0
                || rejection.payload_type.is_empty()
                || !record_rejection_class_is_typed(&rejection.class)
                || rejection.detail.is_empty()
                || !rejections.insert((
                    rejection.source_identity.as_str(),
                    rejection.provider.as_str(),
                    rejection.source_selector.as_str(),
                    rejection.line,
                    rejection.payload_type.as_str(),
                    rejection.class.as_str(),
                    rejection.detail.as_str(),
                ))
            {
                bail!("terminal route result contains an inconsistent rejection diagnostic");
            }
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn compact_json(&self) -> Value {
        let details = self
            .source_failures
            .iter()
            .map(SourceBackedRefreshSourceFailure::compact_json)
            .collect::<Vec<_>>();
        match &self.outcome {
            SourceBackedRefreshRouteOutcome::Succeeded { changed }
                if self.source_failure_total == 0 && self.rejected_record_total == 0 =>
            {
                json!(["s", changed])
            }
            SourceBackedRefreshRouteOutcome::Succeeded { changed } => {
                json!([
                    "s",
                    changed,
                    self.source_failure_total,
                    self.source_retryable_failure_total,
                    details,
                    self.rejected_record_total,
                    self.rejection_diagnostics
                        .iter()
                        .map(SourceBackedRefreshRecordRejection::compact_json)
                        .collect::<Vec<_>>(),
                ])
            }
            SourceBackedRefreshRouteOutcome::Failed {
                class,
                carried_forward,
            } => json!([
                "f",
                source_failure_class_code(class),
                carried_forward,
                self.source_failure_total,
                details,
            ]),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SourceBackedRefreshRouteOutcome {
    Succeeded {
        changed: bool,
    },
    Failed {
        class: String,
        carried_forward: bool,
    },
}

impl SourceBackedRefreshRouteOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    pub fn changed(&self) -> Option<bool> {
        match self {
            Self::Succeeded { changed } => Some(*changed),
            Self::Failed { .. } => None,
        }
    }

    pub fn failure_class(&self) -> Option<&str> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed { class, .. } => Some(class),
        }
    }
}

/// Returns `None` when the route is clean, `Some(true)` when it should retry,
/// and `Some(false)` when the admitted observation should remain blocked.
/// The route outcome and exact source-failure counts are authoritative, while
/// bounded diagnostic prose is presentation-only.
pub fn source_backed_route_retry_disposition(
    result: &SourceBackedRefreshRouteResult,
) -> Option<bool> {
    if let Some(class) = result.outcome.failure_class() {
        return Some(matches!(class, "unavailable" | "source_changed"));
    }
    if result.source_failure_total == 0 {
        return None;
    }
    Some(result.source_retryable_failure_total != 0)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshCatalogRouteOutcome {
    pub catalog_lineage: String,
    pub route_identity: String,
    pub outcome: String,
    pub failure_class: Option<String>,
    pub changed: Option<bool>,
    pub source_failure_total: usize,
    pub rejected_record_total: u64,
}

impl SourceBackedRefreshCatalogRouteOutcome {
    #[doc(hidden)]
    pub fn from_result(catalog_lineage: String, result: &SourceBackedRefreshRouteResult) -> Self {
        let (outcome, failure_class, changed) = match &result.outcome {
            SourceBackedRefreshRouteOutcome::Succeeded { changed } => {
                let outcome = match (
                    result.has_source_failures(),
                    result.rejected_record_total != 0,
                ) {
                    (false, false) => "succeeded",
                    (false, true) => "completed_with_rejections",
                    (true, false) => "succeeded_with_source_failures",
                    (true, true) => "completed_with_rejections_and_source_failures",
                };
                (outcome.to_owned(), None, Some(*changed))
            }
            SourceBackedRefreshRouteOutcome::Failed { class, .. } => {
                ("failed".to_owned(), Some(class.clone()), None)
            }
        };
        Self {
            catalog_lineage,
            route_identity: result.route_identity.clone(),
            outcome,
            failure_class,
            changed,
            source_failure_total: result.source_failure_total,
            rejected_record_total: result.rejected_record_total,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshRecordRejection {
    pub route_identity: String,
    pub source_identity: String,
    pub provider: String,
    pub source_selector: String,
    pub line: u64,
    pub payload_type: String,
    pub class: String,
    pub detail: String,
}

impl SourceBackedRefreshRecordRejection {
    #[doc(hidden)]
    pub fn compact_json(&self) -> Value {
        json!([
            self.source_identity,
            self.provider,
            self.source_selector,
            self.line,
            self.payload_type,
            record_rejection_class_code(&self.class),
            self.detail,
        ])
    }
}

fn record_rejection_class_is_typed(class: &str) -> bool {
    matches!(class, "malformed_record" | "unsupported_record")
}

fn record_rejection_class_code(class: &str) -> &'static str {
    match class {
        "malformed_record" => "m",
        "unsupported_record" => "u",
        _ => "?",
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedRefreshSourceFailure {
    pub route_identity: String,
    pub source_identity: String,
    pub provider: String,
    pub class: String,
    pub carried_forward: bool,
    pub source_selector: String,
    pub detail: String,
}

impl SourceBackedRefreshSourceFailure {
    #[doc(hidden)]
    pub fn compact_json(&self) -> Value {
        json!([
            self.source_identity,
            self.provider,
            source_failure_class_code(&self.class),
            self.carried_forward,
            self.source_selector,
            self.detail,
        ])
    }
}

#[doc(hidden)]
pub fn source_failure_class_is_typed(class: &str) -> bool {
    matches!(
        class,
        "unavailable" | "source_changed" | "unreadable" | "incompatible"
    )
}

fn source_failure_class_code(class: &str) -> &'static str {
    match class {
        "unavailable" => "u",
        "source_changed" => "c",
        "unreadable" => "r",
        "incompatible" => "i",
        _ => "?",
    }
}

fn is_sha256_identity(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
