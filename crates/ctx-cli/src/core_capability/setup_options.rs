use anyhow::{anyhow, Result};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SetupProgressMode {
    Legacy(crate::progress::ProgressArg),
    Events,
}

pub(super) fn setup_notice_lines(object: &serde_json::Map<String, Value>) -> Result<Vec<String>> {
    const MAX_NOTICE_LINES: usize = 8;
    const MAX_NOTICE_LINE_BYTES: usize = 512;
    let lines = object
        .get("notice_lines")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("setup option notice_lines must be an array"))?;
    if lines.len() > MAX_NOTICE_LINES {
        return Err(anyhow!("setup notice has too many lines"));
    }
    lines
        .iter()
        .map(|line| {
            line.as_str()
                .filter(|line| {
                    line.len() <= MAX_NOTICE_LINE_BYTES
                        && !line
                            .bytes()
                            .any(|byte| matches!(byte, b'\r' | b'\n' | b'\0'))
                        && !line
                            .chars()
                            .any(|character| character.is_control() && character != '\t')
                })
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("setup notice line is invalid"))
        })
        .collect()
}

pub(super) fn progress_mode_for_notice(
    mode: crate::progress::ProgressArg,
    terminal_width: Option<usize>,
    lines: &[String],
) -> crate::progress::ProgressArg {
    if mode == crate::progress::ProgressArg::Auto
        && terminal_width.is_some_and(|width| {
            lines
                .iter()
                .any(|line| ctx_terminal::ui::display_width(line) > width)
        })
    {
        crate::progress::ProgressArg::Plain
    } else {
        mode
    }
}

pub(super) fn setup_progress_mode(
    object: &serde_json::Map<String, Value>,
) -> Result<SetupProgressMode> {
    match object.get("progress").and_then(Value::as_str) {
        Some("auto") => Ok(SetupProgressMode::Legacy(
            crate::progress::ProgressArg::Auto,
        )),
        Some("plain") => Ok(SetupProgressMode::Legacy(
            crate::progress::ProgressArg::Plain,
        )),
        Some("json") => Ok(SetupProgressMode::Legacy(
            crate::progress::ProgressArg::Json,
        )),
        Some("none") => Ok(SetupProgressMode::Legacy(
            crate::progress::ProgressArg::None,
        )),
        Some("events") => Ok(SetupProgressMode::Events),
        _ => Err(anyhow!("setup progress option is invalid")),
    }
}
