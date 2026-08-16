use super::*;

#[derive(Clone, Copy)]
pub(super) struct ItemParseContext<'a> {
    pub(super) identity: &'a ClineTaskIdentity,
    pub(super) component: ClineEventComponent,
    pub(super) max_item_units: usize,
}

#[derive(Default)]
pub(super) struct RawEnvelope<'a> {
    pub(super) native_id: Option<String>,
    pub(super) role: Option<String>,
    pub(super) item_type: Option<String>,
    pub(super) kind: Option<String>,
    pub(super) say: Option<String>,
    pub(super) ask: Option<String>,
    pub(super) name: Option<String>,
    pub(super) call_id: Option<String>,
    pub(super) arguments: Option<Value>,
    pub(super) conflicting_argument_alias: bool,
    pub(super) occurred_at_millis: Option<i64>,
    pub(super) content: Option<&'a RawValue>,
    pub(super) text: Option<&'a RawValue>,
    pub(super) message: Option<&'a RawValue>,
    pub(super) output: Option<&'a RawValue>,
    pub(super) result: Option<&'a RawValue>,
    pub(super) response: Option<&'a RawValue>,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) status: Option<String>,
    pub(super) conflicting_discriminator: bool,
    pub(super) oversized_discriminator: bool,
    pub(super) conflicting_name_alias: bool,
    pub(super) conflicting_call_id_alias: bool,
}

impl<'a> RawEnvelope<'a> {
    pub(super) fn unique_result_body(
        &self,
    ) -> Result<Option<&'a RawValue>, (ClineItemRejectionKind, String)> {
        let candidates = [
            self.output,
            self.result,
            self.text,
            self.content,
            self.message,
            self.response,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => Ok(None),
            [selected] => Ok(Some(*selected)),
            _ => Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline result exposes more than one candidate body field".to_owned(),
            )),
        }
    }

    pub(super) fn unique_retained_body(
        &self,
    ) -> Result<Option<&'a RawValue>, (ClineItemRejectionKind, String)> {
        let candidates = [self.text, self.message, self.content]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [] => Ok(None),
            [selected] => Ok(Some(*selected)),
            _ => Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline message exposes more than one candidate body field".to_owned(),
            )),
        }
    }

    pub(super) fn argument_capture(&self) -> ActivityJsonCapture {
        if self.conflicting_argument_alias {
            ActivityJsonCapture::Unavailable
        } else {
            self.arguments
                .clone()
                .map_or(ActivityJsonCapture::Absent, |value| {
                    ActivityJsonCapture::Present { value }
                })
        }
    }

    pub(super) fn normalized_discriminators(&self) -> impl Iterator<Item = String> + '_ {
        [
            self.role.as_deref(),
            self.item_type.as_deref(),
            self.kind.as_deref(),
            self.say.as_deref(),
            self.ask.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(normalize_discriminator)
    }
}

impl<'de> Deserialize<'de> for RawEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawEnvelopeVisitor)
    }
}

struct RawEnvelopeVisitor;

impl<'de> Visitor<'de> for RawEnvelopeVisitor {
    type Value = RawEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline native item object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut envelope = RawEnvelope::default();
        let mut seen_id_fields = BTreeSet::new();
        let mut native_id_observed = false;
        let mut native_id_conflict = false;
        let mut seen_role = false;
        let mut seen_type = false;
        let mut seen_kind = false;
        let mut seen_say = false;
        let mut seen_ask = false;
        let mut seen_name_fields = BTreeSet::new();
        let mut seen_call_id_fields = BTreeSet::new();
        let mut seen_argument_fields = BTreeSet::new();
        let mut seen_timestamp_fields = BTreeSet::new();
        let mut timestamp_observed = false;
        let mut timestamp_conflict = false;
        let mut seen_status_fields = BTreeSet::new();
        let mut status_observed = false;
        let mut status_conflict = false;
        let mut seen_exit_code_fields = BTreeSet::new();
        let mut exit_code_observed = false;
        let mut exit_code_conflict = false;
        let mut seen_duration_fields = BTreeSet::new();
        let mut duration_observed = false;
        let mut duration_conflict = false;
        while let Some(BoundedString(field, _)) =
            map.next_key::<BoundedString<MAX_JSON_KEY_BYTES>>()?
        {
            let Some(field) = field else {
                map.next_value::<IgnoredAny>()?;
                continue;
            };
            match field.as_str() {
                "id" | "uuid" | "messageId" => {
                    let value = map.next_value::<BoundedString<MAX_NATIVE_ID_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    let value = value.0;
                    let duplicate_field = !seen_id_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field || native_id_observed && envelope.native_id != value {
                        native_id_conflict = true;
                        envelope.native_id = None;
                    } else if !native_id_conflict && !native_id_observed {
                        envelope.native_id = value;
                    }
                    native_id_observed = true;
                }
                "role" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_role;
                    seen_role = true;
                    envelope.role = value.0;
                }
                "type" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_type;
                    seen_type = true;
                    envelope.item_type = value.0;
                }
                "kind" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_kind;
                    seen_kind = true;
                    envelope.kind = value.0;
                }
                "say" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_say;
                    seen_say = true;
                    envelope.say = value.0;
                }
                "ask" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    envelope.conflicting_discriminator |= seen_ask;
                    seen_ask = true;
                    envelope.ask = value.0;
                }
                "name" | "tool" | "tool_name" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    let duplicate_field = !seen_name_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field
                        || envelope
                            .name
                            .as_ref()
                            .zip(value.0.as_ref())
                            .is_some_and(|(left, right)| left != right)
                    {
                        envelope.conflicting_name_alias = true;
                        envelope.name = None;
                    } else if !envelope.conflicting_name_alias && envelope.name.is_none() {
                        envelope.name = value.0;
                    }
                }
                "tool_use_id" | "toolUseId" | "call_id" | "callId" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    let duplicate_field = !seen_call_id_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field
                        || envelope
                            .call_id
                            .as_ref()
                            .zip(value.0.as_ref())
                            .is_some_and(|(left, right)| left != right)
                    {
                        envelope.conflicting_call_id_alias = true;
                        envelope.call_id = None;
                    } else if !envelope.conflicting_call_id_alias && envelope.call_id.is_none() {
                        envelope.call_id = value.0;
                    }
                }
                "arguments" | "args" | "input" | "parameters" => {
                    let value = map.next_value::<Value>()?;
                    let duplicate_field = !seen_argument_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field {
                        envelope.conflicting_argument_alias = true;
                        envelope.arguments = None;
                        continue;
                    }
                    if value.is_null() {
                        continue;
                    }
                    if envelope
                        .arguments
                        .as_ref()
                        .is_some_and(|selected| selected != &value)
                    {
                        envelope.conflicting_argument_alias = true;
                        envelope.arguments = None;
                    } else if !envelope.conflicting_argument_alias && envelope.arguments.is_none() {
                        envelope.arguments = Some(value);
                    }
                }
                "ts" | "timestamp" | "createdAt" => {
                    let value = map.next_value::<LooseTimestamp>()?.0;
                    let duplicate_field = !seen_timestamp_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field || timestamp_observed && envelope.occurred_at_millis != value
                    {
                        timestamp_conflict = true;
                        envelope.occurred_at_millis = None;
                    } else if !timestamp_conflict && !timestamp_observed {
                        envelope.occurred_at_millis = value;
                    }
                    timestamp_observed = true;
                }
                "content" => set_raw_once(
                    &mut envelope.content,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "text" => set_raw_once(
                    &mut envelope.text,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "message" => set_raw_once(
                    &mut envelope.message,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "output" => set_raw_once(
                    &mut envelope.output,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "result" => set_raw_once(
                    &mut envelope.result,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "response" => set_raw_once(
                    &mut envelope.response,
                    map.next_value::<&'de RawValue>()?,
                    &mut envelope.conflicting_discriminator,
                ),
                "timed_out" | "timedOut" | "timeout" => {
                    map.next_value::<IgnoredAny>()?;
                }
                "exit_code" | "exitCode" => {
                    let value = map.next_value::<LooseI32>()?.0;
                    let duplicate_field = !seen_exit_code_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field || exit_code_observed && envelope.exit_code != value {
                        exit_code_conflict = true;
                        envelope.exit_code = None;
                    } else if !exit_code_conflict && !exit_code_observed {
                        envelope.exit_code = value;
                    }
                    exit_code_observed = true;
                }
                "duration_ms" | "durationMs" => {
                    let value = map.next_value::<LooseU64>()?.0;
                    let duplicate_field = !seen_duration_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field || duration_observed && envelope.duration_ms != value {
                        duration_conflict = true;
                        envelope.duration_ms = None;
                    } else if !duration_conflict && !duration_observed {
                        envelope.duration_ms = value;
                    }
                    duration_observed = true;
                }
                "success" | "ok" => {
                    map.next_value::<IgnoredAny>()?;
                }
                "isError" | "is_error" | "failed" => {
                    map.next_value::<IgnoredAny>()?;
                }
                "status" | "state" | "outcome" => {
                    let value = map.next_value::<BoundedString<MAX_SMALL_FIELD_BYTES>>()?;
                    envelope.oversized_discriminator |= value.1;
                    let value = value.0;
                    let duplicate_field = !seen_status_fields.insert(field);
                    envelope.conflicting_discriminator |= duplicate_field;
                    if duplicate_field || status_observed && envelope.status != value {
                        status_conflict = true;
                        envelope.status = None;
                    } else if !status_conflict && !status_observed {
                        envelope.status = value;
                    }
                    status_observed = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }
}

pub(super) fn set_raw_once<'a>(
    slot: &mut Option<&'a RawValue>,
    value: &'a RawValue,
    duplicate: &mut bool,
) {
    if slot.replace(value).is_some() {
        *duplicate = true;
    }
}

pub(super) struct OutputCandidate<'a> {
    pub(super) kind: OutputObservationKind,
    pub(super) sub_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) status: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) body: Option<&'a RawValue>,
}

pub(super) struct OutputCandidateContext {
    pub(super) kind: OutputObservationKind,
    pub(super) base_sub_index: u32,
    pub(super) call_id: Option<String>,
    pub(super) status: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
}

pub(super) fn push_explicit_outputs<'a>(
    selected: Option<&'a RawValue>,
    context: OutputCandidateContext,
    outputs: &mut Vec<OutputCandidate<'a>>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let mut leaves = Vec::new();
    if let Some(selected) = selected {
        collect_explicit_output_leaves(selected, &mut leaves, 0)?;
    }
    if leaves.is_empty() {
        outputs.push(OutputCandidate {
            kind: context.kind,
            sub_index: context.base_sub_index,
            call_id: context.call_id,
            status: context.status,
            exit_code: context.exit_code,
            duration_ms: context.duration_ms,
            body: None,
        });
        return Ok(());
    }
    for (inner_index, leaf) in leaves.into_iter().enumerate() {
        if inner_index >= CLINE_NATIVE_PAGE_MAX_UNITS {
            return Err((
                ClineItemRejectionKind::UnsupportedShape,
                "Cline result has more than 64 explicit inner outputs".to_owned(),
            ));
        }
        outputs.push(OutputCandidate {
            kind: context.kind,
            sub_index: context
                .base_sub_index
                .saturating_add(u32::try_from(inner_index).unwrap_or(u32::MAX)),
            call_id: context.call_id.clone(),
            status: context.status.clone(),
            exit_code: context.exit_code,
            duration_ms: context.duration_ms,
            body: Some(leaf),
        });
    }
    Ok(())
}

pub(super) fn push_explicit_result_blocks<'a>(
    content: &'a RawValue,
    kind: OutputObservationKind,
    outer: &RawEnvelope<'a>,
    outputs: &mut Vec<OutputCandidate<'a>>,
) -> Result<(), (ClineItemRejectionKind, String)> {
    let blocks = deserialize_bounded_raw_array(content, "Cline explicit result block array")?;
    for (index, raw_block) in blocks.into_iter().enumerate() {
        if !raw_block.get().trim_start().starts_with('{') {
            continue;
        }
        let block = serde_json::from_str::<RawEnvelope<'_>>(raw_block.get()).map_err(|error| {
            (
                ClineItemRejectionKind::MalformedRecord,
                format!("malformed Cline explicit result block: {error}"),
            )
        })?;
        if block.conflicting_discriminator || block.oversized_discriminator {
            return Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline explicit result block has conflicting or oversized discriminator fields"
                    .to_owned(),
            ));
        }
        if !block
            .normalized_discriminators()
            .any(|value| is_result_discriminator(&value))
        {
            continue;
        }
        push_explicit_outputs(
            block.unique_result_body()?,
            OutputCandidateContext {
                kind,
                base_sub_index: u32::try_from(index)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(1_024),
                call_id: exact_optional_string_alias(&block.call_id, &outer.call_id),
                status: exact_optional_string_alias(&block.status, &outer.status),
                exit_code: exact_optional_copy_alias(block.exit_code, outer.exit_code),
                duration_ms: exact_optional_copy_alias(block.duration_ms, outer.duration_ms),
            },
            outputs,
        )?;
    }
    Ok(())
}

pub(super) fn exact_optional_string_alias(
    left: &Option<String>,
    right: &Option<String>,
) -> Option<String> {
    match (left.as_deref(), right.as_deref()) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => Some(value.to_owned()),
        (None, None) => None,
    }
}

pub(super) fn exact_optional_copy_alias<T: Copy + Eq>(
    left: Option<T>,
    right: Option<T>,
) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) if left != right => None,
        (Some(value), _) | (_, Some(value)) => Some(value),
        (None, None) => None,
    }
}

pub(super) fn collect_explicit_output_leaves<'a>(
    raw: &'a RawValue,
    leaves: &mut Vec<&'a RawValue>,
    depth: usize,
) -> Result<(), (ClineItemRejectionKind, String)> {
    if depth >= MAX_EXPLICIT_RESULT_DEPTH {
        return Err((
            ClineItemRejectionKind::UnsupportedShape,
            "Cline explicit result exceeds the bounded nesting depth".to_owned(),
        ));
    }
    let text = raw.get().trim_start();
    if text == "null" {
        return Ok(());
    }
    if text.starts_with('[') {
        let items = deserialize_bounded_raw_array(raw, "explicit Cline result array")?;
        for item in items {
            if leaves.len() > CLINE_NATIVE_PAGE_MAX_UNITS {
                break;
            }
            let selected = if item.get().trim_start().starts_with('{') {
                serde_json::from_str::<RawExplicitInner<'_>>(item.get())
                    .map_err(|error| {
                        (
                            ClineItemRejectionKind::MalformedRecord,
                            format!("malformed explicit Cline result value: {error}"),
                        )
                    })?
                    .selected()?
                    .unwrap_or(item)
            } else {
                item
            };
            collect_explicit_output_leaves(selected, leaves, depth.saturating_add(1))?;
        }
        return Ok(());
    }
    if text.starts_with('{') {
        let selected = serde_json::from_str::<RawExplicitInner<'_>>(raw.get())
            .map_err(|error| {
                (
                    ClineItemRejectionKind::MalformedRecord,
                    format!("malformed explicit Cline result value: {error}"),
                )
            })?
            .selected()?;
        if let Some(selected) = selected {
            return collect_explicit_output_leaves(selected, leaves, depth.saturating_add(1));
        }
    }
    leaves.push(raw);
    Ok(())
}

pub(super) fn deserialize_bounded_raw_array<'a>(
    raw: &'a RawValue,
    context: &'static str,
) -> Result<Vec<&'a RawValue>, (ClineItemRejectionKind, String)> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let values = deserializer
        .deserialize_seq(BoundedRawArrayVisitor)
        .map_err(|error| {
            let kind = if error.to_string().contains("more than 64") {
                ClineItemRejectionKind::UnsupportedShape
            } else {
                ClineItemRejectionKind::MalformedRecord
            };
            (kind, format!("malformed {context}: {error}"))
        })?;
    deserializer.end().map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("trailing {context} data: {error}"),
        )
    })?;
    Ok(values)
}

struct BoundedRawArrayVisitor;

impl<'de> Visitor<'de> for BoundedRawArrayVisitor {
    type Value = Vec<&'de RawValue>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Cline array with no more than 64 values")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(CLINE_NATIVE_PAGE_MAX_UNITS);
        while values.len() < CLINE_NATIVE_PAGE_MAX_UNITS {
            let Some(value) = sequence.next_element::<&RawValue>()? else {
                return Ok(values);
            };
            values.push(value);
        }
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(
                "Cline array has more than 64 independently publishable values",
            ));
        }
        Ok(values)
    }
}

#[derive(Default)]
struct RawExplicitInner<'a> {
    text: Option<&'a RawValue>,
    content: Option<&'a RawValue>,
    output: Option<&'a RawValue>,
    result: Option<&'a RawValue>,
    ambiguous: bool,
}

impl<'a> RawExplicitInner<'a> {
    fn selected(&self) -> Result<Option<&'a RawValue>, (ClineItemRejectionKind, String)> {
        if self.ambiguous {
            return Err((
                ClineItemRejectionKind::ConflictingDiscriminator,
                "Cline explicit result object exposes more than one candidate body field"
                    .to_owned(),
            ));
        }
        Ok(self.text.or(self.content).or(self.output).or(self.result))
    }
}

impl<'de> Deserialize<'de> for RawExplicitInner<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawExplicitInnerVisitor)
    }
}

struct RawExplicitInnerVisitor;

impl<'de> Visitor<'de> for RawExplicitInnerVisitor {
    type Value = RawExplicitInner<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an explicit Cline result object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut inner = RawExplicitInner::default();
        while let Some(BoundedString(field, _)) =
            map.next_key::<BoundedString<MAX_JSON_KEY_BYTES>>()?
        {
            match field.as_deref() {
                Some("text") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.text.replace(value).is_some()
                        || inner.content.is_some()
                        || inner.output.is_some()
                        || inner.result.is_some();
                }
                Some("content") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.content.replace(value).is_some()
                        || inner.text.is_some()
                        || inner.output.is_some()
                        || inner.result.is_some();
                }
                Some("output") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.output.replace(value).is_some()
                        || inner.text.is_some()
                        || inner.content.is_some()
                        || inner.result.is_some();
                }
                Some("result") => {
                    let value = map.next_value::<&RawValue>()?;
                    inner.ambiguous |= inner.result.replace(value).is_some()
                        || inner.text.is_some()
                        || inner.content.is_some()
                        || inner.output.is_some();
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(inner)
    }
}

pub(super) fn parse_item(
    raw: &RawValue,
    context: ItemParseContext<'_>,
    native_index: u64,
    native_id_occurrences: &mut BTreeMap<String, u64>,
    stats: &mut ClinePublicationStats,
) -> ParsedItem {
    let ItemParseContext {
        identity,
        component,
        max_item_units,
    } = context;
    let observed_bytes = u64::try_from(raw.get().len()).unwrap_or(u64::MAX);
    let envelope = match serde_json::from_str::<RawEnvelope<'_>>(raw.get()) {
        Ok(envelope) => envelope,
        Err(error) => {
            return rejected_item(
                component,
                native_index,
                None,
                observed_bytes,
                ClineItemRejectionKind::MalformedRecord,
                &error.to_string(),
                stats,
            );
        }
    };
    let native_key = native_key(
        envelope.native_id.as_deref(),
        native_index,
        Some(native_id_occurrences),
    );
    if envelope.conflicting_discriminator || envelope.oversized_discriminator {
        let kind = if envelope.conflicting_discriminator {
            ClineItemRejectionKind::ConflictingDiscriminator
        } else {
            ClineItemRejectionKind::OversizedRetainedItem
        };
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            kind,
            "Cline item has conflicting or oversized discriminator fields",
            native_key,
            stats,
        );
    }

    let parsed = match component {
        ClineEventComponent::ApiHistory | ClineEventComponent::FallbackHistory => {
            parse_api_projection(&envelope, component, identity, &native_key, native_index)
        }
        ClineEventComponent::UiMessages => {
            parse_ui_projection(&envelope, identity, &native_key, native_index)
        }
    };
    let mut projection = match parsed {
        Ok(projection) => projection,
        Err((kind, detail)) => {
            return rejected_item_with_key(
                component,
                native_index,
                envelope.native_id,
                observed_bytes,
                kind,
                &detail,
                native_key,
                stats,
            );
        }
    };
    let output_rows = projection.outputs.len();
    let retained_units = projection.rows.len().saturating_add(output_rows);
    if retained_units > max_item_units {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::UnsupportedShape,
            "Cline item exceeds its activation-invariant page unit budget",
            native_key,
            stats,
        );
    }
    let native_content = match serde_json::from_str::<Value>(raw.get()) {
        Ok(value) => value,
        Err(error) => {
            return rejected_item_with_key(
                component,
                native_index,
                envelope.native_id,
                observed_bytes,
                ClineItemRejectionKind::MalformedRecord,
                &error.to_string(),
                native_key,
                stats,
            );
        }
    };
    let mut output_details = Vec::with_capacity(projection.outputs.len());
    for output in projection.outputs {
        stats.outputs_observed = stats.outputs_observed.saturating_add(1);
        let body = match output.body.map(decode_explicit_output_text).transpose() {
            Ok(Some(body)) if !body.trim().is_empty() => body,
            Ok(_) => serde_json::to_string(&native_content).unwrap_or_else(|_| "null".to_owned()),
            Err((kind, detail)) => {
                return rejected_item_with_key(
                    component,
                    native_index,
                    envelope.native_id,
                    observed_bytes,
                    kind,
                    &detail,
                    native_key,
                    stats,
                );
            }
        };
        let output_bytes = body.len();
        let output_structured_content = output
            .body
            .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
            .unwrap_or_else(|| native_content.clone());
        let diagnostic = ClineSparseOutputDiagnostic {
            status: output.status.map(String::into_boxed_str),
            exit_code: output.exit_code,
            duration_ms: output.duration_ms,
            output_bytes,
            call_id: output.call_id.map(String::into_boxed_str),
            structured_content: output_structured_content,
        };
        output_details.push(diagnostic.clone());
        projection.rows.push(ClineEventRow::output(
            ClineEventContext {
                task: identity,
                component,
                item: &native_key,
                item_index: native_index,
                role: ClineEventRole::Unknown,
                occurred_at_millis: projection.occurred_at_millis,
            },
            output.sub_index,
            match output.kind {
                OutputObservationKind::Command => ClineEventKind::CommandOutput,
                OutputObservationKind::Tool => ClineEventKind::ToolOutput,
            },
            body,
            diagnostic,
        ));
    }
    for row in &mut projection.rows {
        row.structured_content = native_content.clone();
    }
    let retained_body_bytes = projection
        .rows
        .iter()
        .map(|row| row.body.as_deref().map_or(0, str::len))
        .sum::<usize>();
    if retained_body_bytes > CLINE_NATIVE_MAX_RETAINED_ITEM_BYTES {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline selected item content exceeds the shared Core content bound",
            native_key,
            stats,
        );
    }
    projection
        .rows
        .sort_by_key(|row| (row.native_order.item_index, row.native_order.sub_index));
    let core_bytes = projection
        .rows
        .iter()
        .map(estimated_event_bytes)
        .sum::<usize>();
    if core_bytes > CLINE_NATIVE_CORE_PAGE_MAX_BYTES {
        return rejected_item_with_key(
            component,
            native_index,
            envelope.native_id,
            observed_bytes,
            ClineItemRejectionKind::OversizedRetainedItem,
            "Cline Core projection exceeds the shared encoded Core record bound",
            native_key,
            stats,
        );
    }
    let checkpoint = ClineItemCheckpoint::new(native_key, &projection.rows, &output_details, None);
    stats.core_rows = stats.core_rows.saturating_add(projection.rows.len());
    ParsedItem {
        checkpoint,
        rows: projection.rows,
        rejection: None,
        core_bytes,
        source_record: None,
    }
}

fn decode_explicit_output_text(raw: &RawValue) -> Result<String, (ClineItemRejectionKind, String)> {
    let value = serde_json::from_str::<Value>(raw.get()).map_err(|error| {
        (
            ClineItemRejectionKind::MalformedRecord,
            format!("invalid selected Cline result content: {error}"),
        )
    })?;
    Ok(provider_normalized_result_value(&value))
}

#[cfg(test)]
mod result_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retains_complete_results_and_rejects_ambiguity_for_cline_and_roo() {
        let identity = ClineTaskIdentity::new("shared-task");
        let parse = |value: serde_json::Value| {
            let raw = RawValue::from_string(value.to_string()).unwrap();
            parse_item(
                &raw,
                ItemParseContext {
                    identity: &identity,
                    component: ClineEventComponent::ApiHistory,
                    max_item_units: 60,
                },
                0,
                &mut BTreeMap::new(),
                &mut ClinePublicationStats::default(),
            )
        };
        let parse_raw = |value: &str| {
            let raw = RawValue::from_string(value.to_owned()).unwrap();
            parse_item(
                &raw,
                ItemParseContext {
                    identity: &identity,
                    component: ClineEventComponent::ApiHistory,
                    max_item_units: 60,
                },
                0,
                &mut BTreeMap::new(),
                &mut ClinePublicationStats::default(),
            )
        };

        for status in [Some("success"), Some("failure"), None] {
            let mut value = json!({
                "role": "tool",
                "tool_use_id": "call-1",
                "content": format!("complete-{}", status.unwrap_or("absent")),
            });
            if let Some(status) = status {
                value["status"] = json!(status);
            }
            let item = parse(value);
            assert!(item.rejection.is_none());
            assert_eq!(item.rows.len(), 1);
            let expected_body = format!("complete-{}", status.unwrap_or("absent"));
            assert_eq!(item.rows[0].body.as_deref(), Some(expected_body.as_str()));
            let output = item.rows[0].sparse_output.as_ref().unwrap();
            assert_eq!(output.status.as_deref(), status);
            assert_eq!(output.call_id.as_deref(), Some("call-1"));
        }

        let large = format!("{}tail", "x".repeat(9 * 1024 * 1024));
        let item = parse(json!({
            "role": "tool",
            "tool_use_id": "large-call",
            "content": large,
            "status": "success",
        }));
        assert!(item.rejection.is_none());
        assert_eq!(
            item.rows[0].body.as_deref().unwrap().len(),
            9 * 1024 * 1024 + 4
        );
        assert!(item.rows[0].body.as_deref().unwrap().ends_with("tail"));

        let ambiguous = parse(json!({
            "role": "tool",
            "content": "first",
            "output": "second",
        }));
        assert_eq!(
            ambiguous.rejection.as_ref().map(|value| value.kind),
            Some(ClineItemRejectionKind::ConflictingDiscriminator)
        );

        let aliases = parse(json!({
            "role": "assistant",
            "content": {
                "type": "tool_use",
                "tool_use_id": "first-call",
                "call_id": "second-call",
                "name": "first_tool",
                "tool": "second_tool",
                "input": {"x": 1},
                "arguments": {"x": 2}
            }
        }));
        assert!(aliases.rejection.is_none());
        assert_eq!(aliases.rows.len(), 1);
        let call = aliases.rows[0].tool_call.as_ref().unwrap();
        assert!(call.call_id.is_none());
        assert!(call.name.is_none());
        assert_eq!(call.arguments, ActivityJsonCapture::Unavailable);
        assert_eq!(
            aliases.rows[0].structured_content["content"]["tool_use_id"],
            "first-call"
        );
        assert_eq!(
            aliases.rows[0].structured_content["content"]["call_id"],
            "second-call"
        );

        let duplicate_call_fields = parse_raw(
            r#"{"role":"assistant","content":{"type":"tool_use","tool_use_id":"call-1","name":"tool","name":"tool","arguments":{"x":1},"arguments":{"x":1}}}"#,
        );
        assert_eq!(
            duplicate_call_fields
                .rejection
                .as_ref()
                .map(|value| value.kind),
            Some(ClineItemRejectionKind::ConflictingDiscriminator)
        );

        let duplicate_status = parse_raw(
            r#"{"role":"tool","tool_use_id":"call-1","status":"success","status":"success","content":"exact body"}"#,
        );
        assert_eq!(
            duplicate_status.rejection.as_ref().map(|value| value.kind),
            Some(ClineItemRejectionKind::ConflictingDiscriminator)
        );

        let ambiguous_message =
            parse_raw(r#"{"role":"assistant","text":"first","message":"second"}"#);
        assert!(ambiguous_message.rejection.is_none());
        assert!(ambiguous_message.rows.is_empty());
    }
}
