use std::cell::Cell;

pub(crate) use ctx_client_observability::analytics::*;

thread_local! {
    static DELIVERY_FAILURE_OUTPUT_QUIET: Cell<bool> = const { Cell::new(false) };
}

struct DeliveryFailureOutputGuard {
    previous: bool,
}

pub(crate) fn quiet_delivery_failure_output(quiet: bool) -> impl Drop {
    let previous = DELIVERY_FAILURE_OUTPUT_QUIET.replace(quiet);
    DeliveryFailureOutputGuard { previous }
}

impl Drop for DeliveryFailureOutputGuard {
    fn drop(&mut self) {
        DELIVERY_FAILURE_OUTPUT_QUIET.set(self.previous);
    }
}

pub(crate) fn send_batch(
    data_root: &std::path::Path,
    config: &crate::config::AppConfig,
    events: &[PublicEventV1],
) {
    if let Err(error) =
        crate::observability_composition::deliver_analytics_batch(data_root, config, events)
    {
        let quiet = DELIVERY_FAILURE_OUTPUT_QUIET.get();
        if !quiet && std::env::var_os("CTX_ANALYTICS_DEBUG").is_some() {
            eprintln!("ctx analytics delivery failed: {error:#}");
        }
    }
}

pub(crate) fn send_pro_operation(
    data_root: &std::path::Path,
    operation: ProHostOperationV1,
    outcome: Outcome,
    duration: std::time::Duration,
) {
    send_batch(
        data_root,
        &match crate::config::AppConfig::load(data_root) {
            Ok(config) => config,
            Err(_) => return,
        },
        &[pro_operation_event(operation, outcome, duration)],
    );
}
