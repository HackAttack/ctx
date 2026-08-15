#[cfg(test)]
mod exact_json_tests;
mod fallback_identity;
mod family;
mod mcp_exchange;
mod model;
mod occurrence;
mod pending_exchange;
mod resumable_sha256;
mod source_identity;
mod terminal_authority;

#[cfg(feature = "test-support")]
mod bench_timings {
    use std::{cell::RefCell, time::Duration};

    #[derive(Debug, Clone, Copy, Default)]
    pub struct JsonlPartialBenchPhaseTimings {
        pub total_us: u64,
        pub open_members_us: u64,
        pub ownership_us: u64,
        pub base_lookup_us: u64,
        pub reset_us: u64,
        pub scan_us: u64,
        pub retain_us: u64,
        pub finish_us: u64,
        pub rejection_code: u64,
    }

    #[derive(Debug, Clone, Copy)]
    pub(crate) enum JsonlPartialBenchPhase {
        Total,
        OpenMembers,
        Ownership,
        BaseLookup,
        Reset,
        Scan,
        Retain,
        Finish,
    }

    thread_local! {
        static TIMINGS: RefCell<JsonlPartialBenchPhaseTimings> =
            const { RefCell::new(JsonlPartialBenchPhaseTimings {
                total_us: 0,
                open_members_us: 0,
                ownership_us: 0,
                base_lookup_us: 0,
                reset_us: 0,
                scan_us: 0,
                retain_us: 0,
                finish_us: 0,
                rejection_code: 0,
            }) };
    }

    pub(crate) fn record(phase: JsonlPartialBenchPhase, elapsed: Duration) {
        let elapsed = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        TIMINGS.with(|timings| {
            let timings = &mut *timings.borrow_mut();
            let value = match phase {
                JsonlPartialBenchPhase::Total => &mut timings.total_us,
                JsonlPartialBenchPhase::OpenMembers => &mut timings.open_members_us,
                JsonlPartialBenchPhase::Ownership => &mut timings.ownership_us,
                JsonlPartialBenchPhase::BaseLookup => &mut timings.base_lookup_us,
                JsonlPartialBenchPhase::Reset => &mut timings.reset_us,
                JsonlPartialBenchPhase::Scan => &mut timings.scan_us,
                JsonlPartialBenchPhase::Retain => &mut timings.retain_us,
                JsonlPartialBenchPhase::Finish => &mut timings.finish_us,
            };
            *value = value.saturating_add(elapsed);
        });
    }

    pub(crate) fn reject(code: u64) {
        TIMINGS.with(|timings| timings.borrow_mut().rejection_code = code);
    }

    pub fn reset() {
        TIMINGS.with(|timings| *timings.borrow_mut() = JsonlPartialBenchPhaseTimings::default());
    }

    pub fn get() -> JsonlPartialBenchPhaseTimings {
        TIMINGS.with(|timings| *timings.borrow())
    }
}

#[cfg(feature = "test-support")]
pub use bench_timings::JsonlPartialBenchPhaseTimings;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn reset_jsonl_partial_bench_phase_timings() {
    bench_timings::reset();
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn jsonl_partial_bench_phase_timings() -> JsonlPartialBenchPhaseTimings {
    bench_timings::get()
}

pub use ctx_history_capture_model::{
    exact_bounded_string_alias, exact_json_value, raw_object_keys_are_unique, ExactJsonStringAlias,
};
pub use fallback_identity::*;
pub use family::*;
pub use mcp_exchange::*;
pub use model::*;
pub use occurrence::*;
pub use pending_exchange::*;
pub use resumable_sha256::*;
pub use source_identity::*;
pub use terminal_authority::*;

#[cfg(test)]
mod test_support_paths {
    pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
        tempfile::Builder::new().prefix("ctx-jsonl-").tempdir()
    }
}
