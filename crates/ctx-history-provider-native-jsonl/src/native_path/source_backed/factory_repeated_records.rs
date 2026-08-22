use std::collections::BTreeSet;

use super::*;

pub(super) type RepeatedRecordKey = (String, u32);

#[derive(Debug, Clone)]
pub(super) struct RepeatedRecordObservation {
    parent_id: Option<String>,
    provider_call_id: TypedKey,
    in_certified_prefix: bool,
}

#[derive(Debug, Clone)]
pub(super) struct RepeatedRecordPlan {
    parent_ids: BTreeSet<String>,
    base_parent_id: Option<String>,
}

impl<R: NativeJsonlRuntime> DirectJsonlFamilyProjector<R> {
    pub(super) fn observe_repeated_record(
        &mut self,
        event: &DirectJsonlEvent,
        in_certified_prefix: bool,
    ) -> DirectJsonlAdapterResult<()> {
        if self.adapter.provider != CaptureProvider::FactoryAiDroid
            || event.stable_retry_discriminator.is_some()
        {
            return Ok(());
        }
        let Some(provider_call_id) = event
            .activity
            .as_ref()
            .and_then(|activity| activity.provider_call_id.clone())
        else {
            return Ok(());
        };
        let Some(native_record_id) = event.native_record_id.as_deref() else {
            return Ok(());
        };
        self.repeated_record_observations
            .entry((native_record_id.to_owned(), event.sub_ordinal))
            .or_default()
            .push(RepeatedRecordObservation {
                parent_id: event.native_parent_id.clone(),
                provider_call_id,
                in_certified_prefix,
            });
        Ok(())
    }

    /// Assigns parent selectors to every member of a newly observed repeated
    /// group. If a prior generation already used the ordinary base identity,
    /// exactly one parent may retain it; existing selector IDs and the trusted
    /// append prefix identify that parent without relying on current scan order.
    pub(super) fn prepare_repeated_record_plan(
        &mut self,
        has_certified_prefix: bool,
    ) -> DirectJsonlAdapterResult<()> {
        let mut plan = BTreeMap::new();
        for (key, observations) in &self.repeated_record_observations {
            if observations.len() == 1 {
                let Some(parent_id) = observations[0].parent_id.as_deref() else {
                    continue;
                };
                if self.repeated_record_identity_exists(key, parent_id)? {
                    plan.insert(
                        key.clone(),
                        RepeatedRecordPlan {
                            parent_ids: BTreeSet::from([parent_id.to_owned()]),
                            base_parent_id: None,
                        },
                    );
                }
                continue;
            }

            let mut parents = BTreeMap::new();
            for observation in observations {
                let Some(parent_id) = observation.parent_id.as_ref() else {
                    return Err(Self::ambiguous_repeated_record(
                        key,
                        "every copy must have non-empty parentId evidence",
                    ));
                };
                if parents.insert(parent_id.clone(), observation).is_some() {
                    return Err(Self::ambiguous_repeated_record(
                        key,
                        "parentId does not uniquely identify every copy",
                    ));
                }
            }
            if observations
                .iter()
                .skip(1)
                .any(|observation| observation.provider_call_id != observations[0].provider_call_id)
            {
                return Err(Self::ambiguous_repeated_record(
                    key,
                    "copies do not share one provider call id",
                ));
            }

            let parent_ids = parents.keys().cloned().collect::<BTreeSet<_>>();
            let base_parent_id = if self.base_identity_exists(key)? {
                let mut missing_selector = Vec::new();
                for parent_id in &parent_ids {
                    if !self.repeated_record_identity_exists(key, parent_id)? {
                        missing_selector.push(parent_id.clone());
                    }
                }
                if missing_selector.is_empty() {
                    None
                } else if has_certified_prefix {
                    let prefix_missing = missing_selector
                        .iter()
                        .filter(|parent_id| {
                            parents
                                .get(*parent_id)
                                .is_some_and(|observation| observation.in_certified_prefix)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    if prefix_missing.len() != 1 {
                        return Err(Self::ambiguous_repeated_record(
                            key,
                            "the prior base copy cannot be identified from the certified prefix",
                        ));
                    }
                    prefix_missing.into_iter().next()
                } else if missing_selector.len() == 1 {
                    missing_selector.into_iter().next()
                } else {
                    return Err(Self::ambiguous_repeated_record(
                        key,
                        "the prior base copy cannot be reconciled during replacement",
                    ));
                }
            } else {
                None
            };
            plan.insert(
                key.clone(),
                RepeatedRecordPlan {
                    parent_ids,
                    base_parent_id,
                },
            );
        }
        self.repeated_record_plan = plan;
        self.repeated_record_plan_prepared = true;
        Ok(())
    }

    fn ambiguous_repeated_record(
        key: &RepeatedRecordKey,
        reason: &'static str,
    ) -> DirectJsonlAdapterError {
        DirectJsonlAdapterError::AmbiguousFactoryRepeatedRecord {
            native_record_id: key.0.clone(),
            sub_ordinal: key.1,
            reason,
        }
    }

    pub(super) fn apply_repeated_record_discriminator(
        &mut self,
        event: &mut DirectJsonlEvent,
    ) -> DirectJsonlAdapterResult<()> {
        if self.adapter.provider != CaptureProvider::FactoryAiDroid
            || event.stable_retry_discriminator.is_some()
        {
            return Ok(());
        }
        let Some(native_record_id) = event.native_record_id.as_deref() else {
            return Ok(());
        };
        let key = (native_record_id.to_owned(), event.sub_ordinal);
        let Some(plan) = self.repeated_record_plan.get(&key) else {
            return Ok(());
        };
        let Some(parent_id) = event.native_parent_id.clone() else {
            return Err(Self::ambiguous_repeated_record(
                &key,
                "planned copy lost its parentId evidence",
            ));
        };
        if !plan.parent_ids.contains(&parent_id) {
            return Err(Self::ambiguous_repeated_record(
                &key,
                "projected copy was absent from the admitted preflight",
            ));
        }
        if plan.base_parent_id.as_deref() == Some(parent_id.as_str()) {
            return Ok(());
        }
        event.stable_retry_discriminator =
            Some(DirectJsonlRetryDiscriminator::FactoryDroidRepeatedRecord { parent_id });
        Ok(())
    }

    fn base_identity_exists(&self, key: &RepeatedRecordKey) -> DirectJsonlAdapterResult<bool> {
        let subrecord_selector = (key.1 != 0)
            .then(|| {
                SubrecordSelector::certified_position(
                    "direct-jsonl-subrecord",
                    TypedKey::U64(u64::from(key.1)),
                    PositionStability::StableSlot,
                )
            })
            .transpose()?;
        self.event_identity_exists(key, subrecord_selector.as_ref())
    }

    fn repeated_record_identity_exists(
        &self,
        key: &RepeatedRecordKey,
        parent_id: &str,
    ) -> DirectJsonlAdapterResult<bool> {
        let selector = factory_repeated_record_selector(parent_id, key.1)?;
        self.event_identity_exists(key, Some(&selector))
    }

    fn event_identity_exists(
        &self,
        key: &RepeatedRecordKey,
        subrecord_selector: Option<&SubrecordSelector>,
    ) -> DirectJsonlAdapterResult<bool> {
        let Some(base_lookup) = self.base_event_lookup.as_ref() else {
            return Ok(false);
        };
        let native_item_key = NativeItemKey::native_id(
            format!("{}.direct-jsonl-event", self.adapter.provider.as_str()),
            TypedKey::utf8(&key.0)?,
        )?;
        let candidate = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.session_id,
            logical_item_kind: "direct-jsonl-event",
            native_item_key: &native_item_key,
            subrecord_selector,
        })?;
        base_lookup
            .contains(candidate.as_uuid())
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()).into())
    }
}

pub(super) fn factory_repeated_record_selector(
    parent_id: &str,
    sub_ordinal: u32,
) -> DirectJsonlAdapterResult<SubrecordSelector> {
    Ok(SubrecordSelector::native_id(
        "factory-ai-droid.repeated-record",
        TypedKey::composite(vec![
            TypedKey::utf8(parent_id)?,
            TypedKey::U64(u64::from(sub_ordinal)),
        ])?,
    )?)
}
