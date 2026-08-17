use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticLifecycleBehavior {
    RetryAppend,
    Overclassify,
    StopBeforeTerminal,
}

#[derive(Debug, Default)]
pub(super) struct SemanticLifecycleObservations {
    pub(super) constructed_modes: Vec<JsonlFamilyProjectionMode>,
    pub(super) preflight_modes: Vec<JsonlFamilyProjectionMode>,
    pub(super) page_modes: Vec<JsonlFamilyProjectionMode>,
    pub(super) finished_modes: Vec<JsonlFamilyProjectionMode>,
}

pub(super) struct SemanticLifecycleTestAdapter {
    pub(super) behavior: SemanticLifecycleBehavior,
    pub(super) observations: Arc<Mutex<SemanticLifecycleObservations>>,
}

pub(super) struct SemanticLifecycleTestExecutor {
    pub(super) behavior: SemanticLifecycleBehavior,
    pub(super) mode: JsonlFamilyProjectionMode,
    pub(super) observations: Arc<Mutex<SemanticLifecycleObservations>>,
    pub(super) consumed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DirectAppendPassObservation {
    pub(super) mode: JsonlFamilyProjectionMode,
    pub(super) direct_append: bool,
    pub(super) preflight_bytes: u64,
    pub(super) projection_bytes: u64,
    pub(super) projected_records: u64,
}

#[derive(Default)]
pub(super) struct DirectAppendTestAdapter {
    pub(super) observations: Arc<Mutex<Vec<DirectAppendPassObservation>>>,
}

pub(super) struct DirectAppendTestExecutor {
    pub(super) mode: JsonlFamilyProjectionMode,
    pub(super) observations: Arc<Mutex<Vec<DirectAppendPassObservation>>>,
    pub(super) prior_records: u64,
    pub(super) preflight_bytes: u64,
    pub(super) projection_bytes: u64,
    pub(super) projected_records: u64,
    pub(super) direct_append: bool,
}

impl JsonlFamilySemanticExecutor for DirectAppendTestExecutor {
    type Runtime = TestJsonlRuntime;

    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<JsonlFamilySemanticPreflight> {
        self.direct_append = input.is_direct_append_resume();
        while let Some(record) = input.next_record()? {
            self.preflight_bytes = self.preflight_bytes.checked_add(record.byte_len()).ok_or(
                CaptureError::SystemInvariant("direct append preflight byte count overflowed"),
            )?;
        }
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        _worker: &mut JsonlFamilyWorkerContext,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        let Some(record) = input.next_record()? else {
            return Ok(None);
        };
        self.projection_bytes = self.projection_bytes.checked_add(record.byte_len()).ok_or(
            CaptureError::SystemInvariant("direct append projection byte count overflowed"),
        )?;
        self.projected_records =
            self.projected_records
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "direct append record count overflowed",
                ))?;
        Ok(Some(JsonlFamilySemanticPage::new(Vec::new())))
    }

    fn finish(self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        let checkpoint = self
            .prior_records
            .checked_add(self.projected_records)
            .ok_or(CaptureError::SystemInvariant(
                "direct append checkpoint count overflowed",
            ))?;
        self.observations
            .lock()
            .unwrap()
            .push(DirectAppendPassObservation {
                mode: self.mode,
                direct_append: self.direct_append,
                preflight_bytes: self.preflight_bytes,
                projection_bytes: self.projection_bytes,
                projected_records: self.projected_records,
            });
        Ok(JsonlFamilySemanticSummary::new(
            0,
            0,
            Some(TypedKey::U64(checkpoint)),
        ))
    }
}

impl JsonlFamilyAdapter for DirectAppendTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "direct-append-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn bind_admitted_eof(&self) -> bool {
        true
    }

    fn append_only_same_object_v1(&self) -> bool {
        true
    }

    fn accepts_direct_append_checkpoint(&self, checkpoint: &TypedKey) -> bool {
        matches!(checkpoint, TypedKey::U64(_))
    }

    fn allows_direct_append_for_leaf(&self, leaf: &JsonlFamilyLeaf) -> bool {
        leaf.observation().supports_exact_revalidation()
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "direct append tests require the semantic executor",
        ))
    }

    fn semantic_executor(
        &self,
        _leaf: &JsonlFamilyLeaf,
        checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<JsonlFamilySemanticExecutorObject>>> {
        let prior_records = match checkpoint {
            Some(TypedKey::U64(records)) => *records,
            Some(_) => {
                return Err(CaptureError::InvalidPayload(
                    "direct append test checkpoint is malformed".to_owned(),
                ))
            }
            None => 0,
        };
        Ok(Some(Box::new(DirectAppendTestExecutor {
            mode,
            observations: Arc::clone(&self.observations),
            prior_records,
            preflight_bytes: 0,
            projection_bytes: 0,
            projected_records: 0,
            direct_append: false,
        })))
    }
}

impl JsonlFamilySemanticExecutor for SemanticLifecycleTestExecutor {
    type Runtime = TestJsonlRuntime;

    fn preflight(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
    ) -> Result<JsonlFamilySemanticPreflight> {
        self.observations
            .lock()
            .unwrap()
            .preflight_modes
            .push(self.mode);
        if self.behavior == SemanticLifecycleBehavior::RetryAppend
            && self.mode == JsonlFamilyProjectionMode::CertifiedAppend
        {
            return Ok(JsonlFamilySemanticPreflight::RetryReplacement);
        }
        while let Some(record) = input.next_record()? {
            let _ = input.record_bytes(record)?;
        }
        Ok(JsonlFamilySemanticPreflight::Ready)
    }

    fn next_page(
        &mut self,
        input: &mut JsonlFamilyExecutionIo,
        _worker: &mut JsonlFamilyWorkerContext,
    ) -> Result<Option<JsonlFamilySemanticPage>> {
        self.observations.lock().unwrap().page_modes.push(self.mode);
        if self.behavior == SemanticLifecycleBehavior::StopBeforeTerminal {
            return Ok(None);
        }
        let Some(record) = input.next_record()? else {
            return Ok(None);
        };
        let _ = input.record_bytes(record)?;
        self.consumed = self.consumed.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("semantic lifecycle test count overflowed".to_owned())
        })?;
        Ok(Some(JsonlFamilySemanticPage::new(Vec::new())))
    }

    fn finish(self: Box<Self>) -> Result<JsonlFamilySemanticSummary> {
        self.observations
            .lock()
            .unwrap()
            .finished_modes
            .push(self.mode);
        let represented = if self.behavior == SemanticLifecycleBehavior::Overclassify {
            self.consumed.saturating_add(1)
        } else {
            0
        };
        Ok(JsonlFamilySemanticSummary::with_logical_counts(
            represented,
            0,
            self.consumed,
            0,
            None,
        ))
    }
}

impl JsonlFamilyAdapter for SemanticLifecycleTestAdapter {
    type Runtime = TestJsonlRuntime;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Pi
    }

    fn source_format(&self) -> &'static str {
        TEST_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        TEST_SCHEMA
    }

    fn parser_revision(&self) -> &'static str {
        "semantic-lifecycle-test-parser-v1"
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn bind_admitted_eof(&self) -> bool {
        true
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        TestAdapter.discover(root)
    }

    fn projector(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<JsonlFamilyProjectorObject>> {
        Err(CaptureError::SystemInvariant(
            "semantic lifecycle tests require the semantic executor",
        ))
    }

    fn semantic_executor(
        &self,
        _leaf: &JsonlFamilyLeaf,
        _checkpoint: Option<&TypedKey>,
        _base_event_lookup: Option<IndexBaseEventLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Option<Box<JsonlFamilySemanticExecutorObject>>> {
        self.observations
            .lock()
            .unwrap()
            .constructed_modes
            .push(mode);
        Ok(Some(Box::new(SemanticLifecycleTestExecutor {
            behavior: self.behavior,
            mode,
            observations: Arc::clone(&self.observations),
            consumed: 0,
        })))
    }
}
