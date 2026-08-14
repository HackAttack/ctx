use std::thread;

use ctx_history_index::WriterOptions;

pub use ctx_history_capture_runtime::source_backed_refresh_work_budget;

/// Binds the neutral CPU split to the index writer without moving index policy
/// into the source-backed runtime crate.
pub fn source_backed_refresh_writer_options() -> WriterOptions {
    source_backed_refresh_writer_options_for_parallelism(
        thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
    )
}

fn source_backed_refresh_writer_options_for_parallelism(
    available_parallelism: usize,
) -> WriterOptions {
    WriterOptions {
        indexer_threads:
            ctx_history_capture_runtime::source_backed_refresh_indexer_threads_for_parallelism(
                available_parallelism,
            ),
        ..WriterOptions::default()
    }
}

#[cfg(test)]
pub(crate) fn source_backed_leaf_worker_budget(indexer_threads: usize) -> usize {
    ctx_history_capture_runtime::source_backed_refresh_work_budget(indexer_threads)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_options_preserve_the_runtime_cpu_split() {
        for available in 1..=64 {
            assert_eq!(
                source_backed_refresh_writer_options_for_parallelism(available).indexer_threads,
                ctx_history_capture_runtime::source_backed_refresh_indexer_threads_for_parallelism(
                    available
                )
            );
        }
    }
}
