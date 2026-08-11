use std::{borrow::Cow, fmt, mem::size_of};

use super::*;
use crate::provider::source_backed::family::jsonl::{
    jsonl_terminal_call_id_digest, JsonlCheckpointedTerminalAuthority,
    JsonlTerminalObservationRegion,
};

const MAX_MCP_RAW_CALL_IDS_PER_RECORD: usize = 8;
const MCP_TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/mcp-terminal-call-id/v1\0";
const RESULT_TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/result-terminal-call-id/v1\0";
const MCP_TERMINAL_AUTHORITY_ENTRY_OVERHEAD_BYTES: usize = 3 * size_of::<usize>();

pub(in super::super) fn mcp_terminal_candidate_evidence(
    record: &[u8],
) -> Option<McpRawRecordEvidence> {
    let evidence = serde_json::from_slice::<McpRawRecordEvidence>(record).ok()?;
    // Terminal uniqueness is source authority, not projection validity. Count
    // every bounded structural terminal occurrence before result or duration
    // validation so malformed same-call-ID evidence forces abstention too.
    evidence.is_terminal().then_some(evidence)
}

#[derive(Debug, Clone)]
pub(in super::super) struct CodexMcpTerminalAuthority {
    mcp_call_ids: JsonlCheckpointedTerminalAuthority,
    result_call_ids: JsonlCheckpointedTerminalAuthority,
}

impl Default for CodexMcpTerminalAuthority {
    fn default() -> Self {
        Self {
            mcp_call_ids: JsonlCheckpointedTerminalAuthority::available(),
            result_call_ids: JsonlCheckpointedTerminalAuthority::available(),
        }
    }
}

impl CodexMcpTerminalAuthority {
    pub(in super::super) fn from_checkpoint(checkpoint: &CodexTerminalAuthorityCheckpoint) -> Self {
        Self {
            mcp_call_ids: JsonlCheckpointedTerminalAuthority::from_digest_counts(
                checkpoint
                    .mcp_call_ids
                    .iter()
                    .map(|entry| (entry.call_id_sha256, entry.candidates)),
                checkpoint.mcp_exhausted,
            ),
            result_call_ids: JsonlCheckpointedTerminalAuthority::from_digest_counts(
                checkpoint
                    .result_call_ids
                    .iter()
                    .map(|entry| (entry.call_id_sha256, entry.candidates)),
                checkpoint.result_exhausted,
            ),
        }
    }

    pub(in super::super) fn checkpoint(&self) -> CodexTerminalAuthorityCheckpoint {
        CodexTerminalAuthorityCheckpoint {
            mcp_call_ids: self
                .mcp_call_ids
                .digest_counts()
                .map(|(call_id_sha256, candidates)| CodexTerminalAuthorityEntry {
                    call_id_sha256,
                    candidates,
                })
                .collect(),
            result_call_ids: self
                .result_call_ids
                .digest_counts()
                .map(|(call_id_sha256, candidates)| CodexTerminalAuthorityEntry {
                    call_id_sha256,
                    candidates,
                })
                .collect(),
            mcp_exhausted: self.mcp_call_ids.exhausted(),
            result_exhausted: self.result_call_ids.exhausted(),
        }
    }

    pub(in super::super) fn appended_suffix_invalidates(
        &self,
        combined: &CodexMcpTerminalAuthority,
    ) -> bool {
        self.mcp_call_ids
            .positive_claim_invalidated_by(&combined.mcp_call_ids)
            || self
                .result_call_ids
                .positive_claim_invalidated_by(&combined.result_call_ids)
    }

    pub(in super::super) fn observe(&mut self, evidence: &McpRawRecordEvidence) {
        if !evidence.is_terminal() {
            return;
        }
        if evidence.call_id_capacity_exceeded {
            self.mcp_call_ids.observe_ambiguous_terminal();
            return;
        }
        for digest in &evidence.call_id_sha256 {
            self.mcp_call_ids.observe_digest(
                *digest,
                JsonlTerminalObservationRegion::WholeSource,
                MAX_CODEX_MCP_TERMINAL_AUTHORITIES,
            );
        }
    }

    pub(in super::super) fn observe_result_call_id(&mut self, call_id: &str) {
        self.result_call_ids.observe(
            RESULT_TERMINAL_CALL_ID_DOMAIN,
            call_id,
            JsonlTerminalObservationRegion::WholeSource,
            MAX_CODEX_MCP_TERMINAL_AUTHORITIES,
        );
    }

    pub(in super::super) fn observe_ambiguous_result_terminal(&mut self) {
        self.result_call_ids.observe_ambiguous_terminal();
    }

    pub(in super::super) fn observe_ambiguous_terminal(&mut self) {
        self.mcp_call_ids.observe_ambiguous_terminal();
        self.observe_ambiguous_result_terminal();
    }

    // Authority keys are the complete domain-separated SHA-256 identity used
    // by the parser. The compact wire encodes every one of those 32 bytes and
    // its exact multiplicity; no prefix, bucket, or probabilistic match can
    // authorize attribution. Capacity exhaustion clears the domain and makes
    // every query abstain, so absence can never be promoted to uniqueness.
    pub(super) fn is_unique(&self, call_id: &str) -> bool {
        self.mcp_call_ids
            .is_unique(MCP_TERMINAL_CALL_ID_DOMAIN, call_id)
    }

    pub(super) fn is_unique_result(&self, call_id: &str) -> bool {
        self.result_call_ids
            .is_unique(RESULT_TERMINAL_CALL_ID_DOMAIN, call_id)
    }

    pub(in super::super) fn entry_count(&self) -> usize {
        self.mcp_call_ids
            .entry_count()
            .saturating_add(self.result_call_ids.entry_count())
    }

    pub(in super::super) fn estimated_owned_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(
            self.mcp_call_ids
                .entry_count()
                .saturating_add(self.result_call_ids.entry_count())
                .saturating_mul(
                    size_of::<([u8; 32], u8)>()
                        .saturating_add(MCP_TERMINAL_AUTHORITY_ENTRY_OVERHEAD_BYTES),
                ),
        )
    }
}

#[cfg(test)]
mod authority_tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn compact_authority_uses_full_digests_and_restart_preserves_abstention() {
        let mut prefixes = BTreeMap::<[u8; 2], String>::new();
        let (first, second) = (0..100_000)
            .find_map(|index| {
                let call_id = format!("adversarial-authority-{index}");
                let digest =
                    jsonl_terminal_call_id_digest(RESULT_TERMINAL_CALL_ID_DOMAIN, &call_id);
                let prefix = [digest[0], digest[1]];
                prefixes
                    .insert(prefix, call_id.clone())
                    .filter(|prior| {
                        jsonl_terminal_call_id_digest(RESULT_TERMINAL_CALL_ID_DOMAIN, prior)
                            != digest
                    })
                    .map(|prior| (prior, call_id))
            })
            .expect("the deterministic fixture must find a truncated-digest collision");

        let mut authority = CodexMcpTerminalAuthority::default();
        authority.observe_result_call_id(&first);
        authority.observe_result_call_id(&second);
        assert_ne!(
            jsonl_terminal_call_id_digest(RESULT_TERMINAL_CALL_ID_DOMAIN, &first),
            jsonl_terminal_call_id_digest(RESULT_TERMINAL_CALL_ID_DOMAIN, &second)
        );
        assert!(authority.is_unique_result(&first));
        assert!(authority.is_unique_result(&second));

        authority.observe_result_call_id(&first);
        assert!(!authority.is_unique_result(&first));
        assert!(authority.is_unique_result(&second));

        let wire = serde_json::to_vec(&authority.checkpoint()).unwrap();
        let checkpoint = serde_json::from_slice(&wire).unwrap();
        let restarted = CodexMcpTerminalAuthority::from_checkpoint(&checkpoint);
        assert!(!restarted.is_unique_result(&first));
        assert!(restarted.is_unique_result(&second));
    }

    #[test]
    fn compact_authority_capacity_exhaustion_is_durable_unknown() {
        let mut authority = CodexMcpTerminalAuthority::default();
        for index in 0..MAX_CODEX_MCP_TERMINAL_AUTHORITIES {
            authority.observe_result_call_id(&format!("bounded-authority-{index}"));
        }
        assert!(authority.is_unique_result("bounded-authority-0"));

        authority.observe_result_call_id("bounded-authority-overflow");
        assert!(!authority.is_unique_result("bounded-authority-0"));
        assert!(!authority.is_unique_result("bounded-authority-overflow"));
        assert!(!authority.is_unique_result("never-observed"));

        let checkpoint = authority.checkpoint();
        assert!(serde_json::to_vec(&checkpoint).unwrap().len() < 16 * 1024);
        let restarted = CodexMcpTerminalAuthority::from_checkpoint(&checkpoint);
        assert!(!restarted.is_unique_result("bounded-authority-0"));
        assert!(!restarted.is_unique_result("never-observed"));
    }
}

#[derive(Default)]
pub(in super::super) struct McpRawRecordEvidence {
    record_type: Option<String>,
    payload: Option<McpAttributionPayload>,
    call_id_sha256: Vec<[u8; 32]>,
    call_id_capacity_exceeded: bool,
}

impl McpRawRecordEvidence {
    pub(super) fn is_terminal(&self) -> bool {
        self.record_type.as_deref() == Some("event_msg")
            && self
                .payload
                .as_ref()
                .and_then(|payload| payload.item_type.as_deref())
                == Some("mcp_tool_call_end")
    }

    fn merge_call_ids(&mut self, payload: &McpAttributionPayload) {
        self.call_id_capacity_exceeded |= payload.call_id_capacity_exceeded;
        if self.call_id_capacity_exceeded {
            return;
        }
        for digest in &payload.call_id_sha256 {
            if self.call_id_sha256.contains(digest) {
                continue;
            }
            if self.call_id_sha256.len() >= MAX_MCP_RAW_CALL_IDS_PER_RECORD {
                self.call_id_capacity_exceeded = true;
                return;
            }
            self.call_id_sha256.push(*digest);
        }
    }
}

impl<'de> serde::Deserialize<'de> for McpRawRecordEvidence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(McpRawRecordEvidenceVisitor)
    }
}

struct McpRawRecordEvidenceVisitor;

impl<'de> serde::de::Visitor<'de> for McpRawRecordEvidenceVisitor {
    type Value = McpRawRecordEvidence;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal envelope")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let mut evidence = McpRawRecordEvidence::default();
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    evidence.record_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "payload" => {
                    let payload = map.next_value::<McpAttributionPayload>()?;
                    evidence.merge_call_ids(&payload);
                    evidence.payload = Some(payload);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(evidence)
    }
}

#[derive(Default)]
struct McpAttributionPayload {
    item_type: Option<String>,
    call_id_sha256: Vec<[u8; 32]>,
    call_id_capacity_exceeded: bool,
}

impl McpAttributionPayload {
    fn observe_call_id(&mut self, call_id: Option<String>) {
        if let Some(call_id) = call_id
            .as_deref()
            .filter(|call_id| !call_id.is_empty() && call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES)
        {
            let digest = jsonl_terminal_call_id_digest(MCP_TERMINAL_CALL_ID_DOMAIN, call_id);
            if !self.call_id_sha256.contains(&digest) {
                if self.call_id_sha256.len() >= MAX_MCP_RAW_CALL_IDS_PER_RECORD {
                    self.call_id_capacity_exceeded = true;
                } else {
                    self.call_id_sha256.push(digest);
                }
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for McpAttributionPayload {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(McpAttributionPayloadVisitor)
    }
}

struct McpAttributionPayloadVisitor;

impl<'de> serde::de::Visitor<'de> for McpAttributionPayloadVisitor {
    type Value = McpAttributionPayload;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal payload")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let mut payload = McpAttributionPayload::default();
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    payload.item_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "call_id" => {
                    let call_id = map
                        .next_value::<BoundedStringProbe<MAX_CODEX_TOOL_CALL_ID_BYTES>>()?
                        .value;
                    payload.observe_call_id(call_id);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(payload)
    }
}

#[derive(Default)]
pub(super) struct BoundedStringProbe<const MAX_BYTES: usize> {
    pub(super) value: Option<String>,
}

impl<'de, const MAX_BYTES: usize> serde::Deserialize<'de> for BoundedStringProbe<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedStringProbeVisitor::<MAX_BYTES>)
    }
}

struct BoundedStringProbeVisitor<const MAX_BYTES: usize>;

impl<'de, const MAX_BYTES: usize> serde::de::Visitor<'de> for BoundedStringProbeVisitor<MAX_BYTES> {
    type Value = BoundedStringProbe<MAX_BYTES>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe {
            value: (value.len() <= MAX_BYTES).then(|| value.to_owned()),
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe {
            value: (value.len() <= MAX_BYTES).then(|| value.to_owned()),
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe {
            value: (value.len() <= MAX_BYTES).then_some(value),
        })
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(BoundedStringProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(BoundedStringProbe::default())
    }
}
