use ctx_history_core::StableEntityId;

use crate::Result;

pub fn core_record_leaf(event_id: StableEntityId, encoded_core_record: &[u8]) -> Result<[u8; 32]> {
    Ok(ctx_history_core::core_record_leaf_digest(
        event_id,
        encoded_core_record,
    )?)
}

pub fn core_record_accumulator_leaf(
    event_id: StableEntityId,
    record_leaf: &[u8; 32],
) -> Result<[u8; 32]> {
    Ok(ctx_history_core::core_record_accumulator_leaf_digest(
        event_id,
        record_leaf,
    )?)
}

/// Adds a domain-separated record leaf to the source's commutative 256-bit
/// accumulator modulo 2^256.
pub fn accumulate_core_record(accumulator: &mut [u8; 32], record_leaf_or_delta: &[u8; 32]) {
    let mut carry = 0_u16;
    for (current, addend) in accumulator
        .iter_mut()
        .rev()
        .zip(record_leaf_or_delta.iter().rev())
    {
        let sum = u16::from(*current) + u16::from(*addend) + carry;
        *current = sum as u8;
        carry = sum >> 8;
    }
}
