pub(in crate::family::route) fn family_scanner_worker_count_policy(
    recommended: usize,
    requested_workers: Option<usize>,
) -> usize {
    if recommended == 0 {
        return 0;
    }
    requested_workers
        .unwrap_or(recommended)
        .clamp(1, recommended)
}

pub(super) fn family_scanner_worker_count(recommended: usize) -> usize {
    #[cfg(test)]
    {
        super::super::FAMILY_SCANNER_WORKERS_OVERRIDE.with(|value| {
            family_scanner_worker_count_policy(recommended, Some(value.get().unwrap_or(1)))
        })
    }
    #[cfg(not(test))]
    {
        family_scanner_worker_count_policy(recommended, None)
    }
}
