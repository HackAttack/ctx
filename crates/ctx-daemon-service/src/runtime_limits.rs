pub const SEMANTIC_WORKER_BATCH_MAX: usize = 1_000_000;
pub(super) const SEMANTIC_MODEL_INIT_MIN_REMAINING_SECS: u64 = 15;
pub(super) const DAEMON_QUERY_ENDPOINT_FILE: &str = "query-endpoint.json";
pub(super) const DAEMON_SEMANTIC_JOB_FILE: &str = "semantic-index.json";
pub const DAEMON_IDLE_EXIT_SECONDS_CAP: u64 = 24 * 60 * 60;
pub(super) const DAEMON_SEMANTIC_RESERVE_GRACE_SECS: u64 = 10;
pub(super) const DAEMON_MIN_REMAINING_FOR_JOB_SECS: u64 = 2;
