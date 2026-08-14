/// Conservative charge for one changed-session registry entry.
///
/// This covers the UUID key, fixed-size session identity facts, hash-table
/// spare capacity, and the route-local UUID undo entry while a route is active.
pub(crate) const CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES: usize = 1024;

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub indexer_threads: usize,
    /// Explicit writer memory budget. Tantivy uses it for indexing buffers;
    /// the changed-session registry independently uses it as a hard charged
    /// ceiling so construction-time identity authority cannot grow unbounded.
    pub memory_bytes: usize,
}

impl Default for WriterOptions {
    fn default() -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Self {
            indexer_threads: parallelism.clamp(1, 8),
            memory_bytes: 512 * 1024 * 1024,
        }
    }
}
