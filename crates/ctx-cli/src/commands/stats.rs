use anyhow::Result;

use crate::{local_usage, output::print_json, ui::Ui};

pub(crate) use ctx_cli_presentation::commands::StatsArgs;

/// The public stats JSON shape changed when paid-only aggregate fields were
/// removed. Keep that replacement contract explicit at the public CLI edge.
pub(crate) const STATS_JSON_SCHEMA_VERSION: i64 = 3;

pub(crate) fn run(
    args: StatsArgs,
    storage: &local_usage::LocalUsageStorageAuthority,
    control: &local_usage::UsageControlSnapshot,
    ui: &mut Ui,
) -> Result<()> {
    if !args.format.is_json() {
        return ctx_cli_presentation::commands::stats::run(args, storage, control, ui);
    }

    let mut report = local_usage::read_report_authorized(storage, control, true);
    report.schema_version = STATS_JSON_SCHEMA_VERSION;
    print_json(serde_json::to_value(report)?)
}

pub(crate) fn malformed_config_failure(json_output: bool, ui: &mut Ui) -> Result<()> {
    if !json_output {
        return ctx_cli_presentation::commands::stats::malformed_config_failure(false, ui);
    }

    let mut report = local_usage::UsageReport::config_error();
    report.schema_version = STATS_JSON_SCHEMA_VERSION;
    let output = format!("{}\n", serde_json::to_string(&report)?);
    ui.write_stderr_bytes(output.as_bytes())?;
    Err(crate::dispatch::rendered_cli_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_json_schema_is_explicit_for_error_reports() {
        let mut report = local_usage::UsageReport::config_error();
        report.schema_version = STATS_JSON_SCHEMA_VERSION;

        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value["schema_version"], 3);
        assert!(value.get("pro_blame").is_none());
        assert!(value.get("citation_count").is_none());
    }
}
