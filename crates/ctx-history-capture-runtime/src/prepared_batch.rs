use super::*;

#[derive(Debug)]
pub struct CorePreparedBatchBuilder<P: CorePreparationPort> {
    prepared: Vec<P::Prepared>,
    lease: Option<CoreRouteByteLease>,
    prepared_bytes: u64,
    progress: CoreRecordBatchProgress,
}
impl<P: CorePreparationPort> Default for CorePreparedBatchBuilder<P> {
    fn default() -> Self {
        Self {
            prepared: Vec::new(),
            lease: None,
            prepared_bytes: 0,
            progress: CoreRecordBatchProgress::default(),
        }
    }
}

impl<P: CorePreparationPort> CorePreparedBatchBuilder<P> {
    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }

    pub fn len(&self) -> usize {
        self.prepared.len()
    }

    pub fn has_lease(&self) -> bool {
        self.lease.is_some()
    }

    pub fn lease_bytes(&self) -> u64 {
        self.lease.as_ref().map_or(0, CoreRouteByteLease::bytes)
    }

    pub fn remaining_bytes(&self) -> u64 {
        self.lease_bytes().saturating_sub(self.prepared_bytes)
    }

    pub fn can_admit(&self, prepared_bytes: u64) -> bool {
        self.lease.as_ref().is_some_and(|lease| {
            self.prepared_bytes
                .checked_add(prepared_bytes)
                .is_some_and(|total| total <= lease.bytes())
        })
    }

    pub fn reserve_bytes(
        &mut self,
        reservation_bytes: u64,
        resources: &CoreRouteResources,
    ) -> Result<(), CorePreparationError<P::Failure>> {
        debug_assert!(self.is_empty());
        debug_assert!(self.lease.is_none());
        let reservation_bytes = usize::try_from(reservation_bytes).map_err(|_| {
            CorePreparationError::Resource(CoreRouteResourceError::AccountingOverflow {
                kind: CoreRouteResourceKind::CoreOutput,
                maximum: resources.maximum_bytes(CoreRouteResourceKind::CoreOutput),
            })
        })?;
        self.lease = Some(
            resources
                .reserve(CoreRouteResourceKind::CoreOutput, reservation_bytes)
                .map_err(CorePreparationError::Resource)?,
        );
        Ok(())
    }

    pub fn release_empty_lease(&mut self) {
        debug_assert!(self.is_empty());
        self.lease = None;
        self.prepared_bytes = 0;
    }

    pub fn push(
        &mut self,
        prepared: P::Prepared,
        port: &P,
    ) -> Result<(), CorePreparationError<P::Failure>> {
        let prepared_bytes = u64::try_from(port.encoded_bytes(&prepared)).map_err(|_| {
            CorePreparationError::Internal("prepared Core-record byte count overflowed")
        })?;
        if !self.can_admit(prepared_bytes) {
            return Err(CorePreparationError::Internal(
                "prepared Core record exceeded its batch reservation",
            ));
        }
        self.prepared_bytes = self.prepared_bytes.checked_add(prepared_bytes).ok_or(
            CorePreparationError::Internal("prepared Core-record batch byte count overflowed"),
        )?;
        self.prepared.push(prepared);
        Ok(())
    }

    pub fn push_with_progress(
        &mut self,
        prepared: P::Prepared,
        port: &P,
        progress: CoreRecordProgress,
    ) -> Result<(), CorePreparationError<P::Failure>> {
        self.push(prepared, port)?;
        self.progress.push(progress);
        Ok(())
    }

    pub fn take_batch(
        &mut self,
    ) -> Result<Option<CorePreparedBatch<P>>, CorePreparationError<P::Failure>> {
        if self.is_empty() {
            self.lease = None;
            self.prepared_bytes = 0;
            self.progress = CoreRecordBatchProgress::default();
            return Ok(None);
        }
        if self.prepared.len() > CORE_RECORD_BATCH_MAX_RECORDS {
            return Err(CorePreparationError::Internal(
                "Core-record emission batch exceeds the shared protocol bound",
            ));
        }
        let lease = self.lease.take().ok_or(CorePreparationError::Internal(
            "prepared Core-record batch has no live byte reservation",
        ))?;
        self.prepared_bytes = 0;
        Ok(Some(CorePreparedBatch {
            prepared: std::mem::take(&mut self.prepared),
            lease,
            progress: std::mem::take(&mut self.progress),
        }))
    }
}

/// One worker-prepared Core-record batch. Its lease is retained until a
/// writer has accepted all records or the batch is dropped on an error path.
#[derive(Debug)]
pub struct CorePreparedBatch<P: CorePreparationPort> {
    prepared: Vec<P::Prepared>,
    lease: CoreRouteByteLease,
    progress: CoreRecordBatchProgress,
}

impl<P: CorePreparationPort> CorePreparedBatch<P> {
    pub fn is_empty(&self) -> bool {
        self.prepared.is_empty()
    }

    pub fn len(&self) -> usize {
        self.prepared.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &P::Prepared> {
        self.prepared.iter()
    }

    pub fn progress(&self) -> &CoreRecordBatchProgress {
        &self.progress
    }

    pub fn into_prepared(self) -> (Vec<P::Prepared>, CoreRouteByteLease) {
        (self.prepared, self.lease)
    }
}
