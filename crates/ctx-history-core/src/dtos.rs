use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::CoreError;

text_enum! {
    pub enum Fidelity {
        Full => "full",
        Partial => "partial",
        Imported => "imported",
        Inferred => "inferred",
        SummaryOnly => "summary_only",
    }
    default Partial
}

text_enum! {
    pub enum SessionStatus {
        Started => "started",
        Active => "active",
        Idle => "idle",
        Completed => "completed",
        Failed => "failed",
        Interrupted => "interrupted",
        Imported => "imported",
    }
    default Started
}

text_enum! {
    pub enum EventType {
        Message => "message",
        ToolCall => "tool_call",
        ToolOutput => "tool_output",
        CommandStarted => "command_started",
        CommandOutput => "command_output",
        CommandFinished => "command_finished",
        Artifact => "artifact",
        Summary => "summary",
        Notice => "notice",
    }
    default Notice
}

text_enum! {
    pub enum EventRole {
        User => "user",
        Assistant => "assistant",
        System => "system",
        Tool => "tool",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum ArtifactKind {
        Transcript => "transcript",
        Stdout => "stdout",
        Stderr => "stderr",
        Screenshot => "screenshot",
        Report => "report",
        Diff => "diff",
        FileSnapshot => "file_snapshot",
        Json => "json",
        Markdown => "markdown",
        Binary => "binary",
    }
    default Binary
}
