use std::{io, path::Path, time::Duration};

use anyhow::Result;
use ctx_daemon_cli::{
    PinnedSourceBackedGeneration, SourceBackedRefreshMode, SourceBackedRefreshObservation,
};
use ctx_terminal::{Document, RenderContext, StreamKind, Ui};

use crate::HistoryCliConfig;

/// The output channel selected by a command body. The final adapter decides
/// how terminal styling and stream handles are implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

pub trait TerminalPort {
    fn context(&self, stream: OutputStream) -> &RenderContext;

    fn write_document(&mut self, stream: OutputStream, document: &Document) -> io::Result<()>;

    fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

impl TerminalPort for Ui {
    fn context(&self, stream: OutputStream) -> &RenderContext {
        self.context(match stream {
            OutputStream::Stdout => StreamKind::Stdout,
            OutputStream::Stderr => StreamKind::Stderr,
        })
    }

    fn write_document(&mut self, stream: OutputStream, document: &Document) -> io::Result<()> {
        self.write(
            match stream {
                OutputStream::Stdout => StreamKind::Stdout,
                OutputStream::Stderr => StreamKind::Stderr,
            },
            document,
        )
    }

    fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write as _;

        match stream {
            OutputStream::Stdout => self.stdout_writer().write_all(bytes),
            OutputStream::Stderr => self.stderr_writer().write_all(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ui::flush(self)
    }
}

/// The daemon-facing capability required by the read/source-index command
/// family. The final binary supplies its already-read configuration snapshot;
/// this application never reaches back into final configuration or analytics.
pub trait HistoryCliRuntimePort {
    fn config(&self) -> HistoryCliConfig;

    fn coordinate_refresh(
        &self,
        data_root: &Path,
        mode: SourceBackedRefreshMode,
    ) -> Result<SourceBackedRefreshObservation>;

    fn wait_for_query_service(&self, data_root: &Path, timeout: Duration) -> bool;

    fn pin_active_generation(&self, data_root: &Path) -> Result<PinnedSourceBackedGeneration>;
}

/// Bounded local-usage facts computed by a read command. The final adapter
/// decides whether and how to persist or deliver them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchContextObservation {
    delivered_bytes: Option<usize>,
    matched_bytes: Option<usize>,
}

impl SearchContextObservation {
    pub const fn unavailable() -> Self {
        Self {
            delivered_bytes: None,
            matched_bytes: None,
        }
    }

    pub fn complete(delivered_bytes: usize, matched_bytes: usize) -> Option<Self> {
        Some(Self {
            delivered_bytes: Some(delivered_bytes),
            matched_bytes: Some(matched_bytes),
        })
    }

    pub const fn complete_byte_totals(self) -> Option<(usize, usize)> {
        match (self.delivered_bytes, self.matched_bytes) {
            (Some(delivered), Some(matched)) => Some((delivered, matched)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryCliOperation {
    Search,
    Show,
    Locate,
    List,
    Import,
    Setup,
    Sources,
}

/// Values captured at the command boundary. The final binary maps these to
/// its release telemetry schemas and remains the sole delivery/finalization
/// authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryCliObservationValue {
    Text(String),
    Count(u64),
    Flag(bool),
    DurationMillis(u64),
}

/// Cardinality-free observation values emitted by history command bodies.
/// Delivery, batching, and product telemetry schemas remain final-adapter
/// responsibilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCliObservation {
    pub operation: HistoryCliOperation,
    pub succeeded: bool,
    pub result_count: Option<u64>,
    pub output_bytes: Option<u64>,
    pub fields: Vec<(&'static str, HistoryCliObservationValue)>,
    pub search: Option<SearchExecutionObservation>,
}

/// Complete, bounded facts produced by one search execution. These retain the
/// command's measured outcomes without exposing the final product event
/// schema or delivery client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchExecutionObservation {
    pub refresh_mode: RefreshObservationMode,
    pub refresh_status: RefreshObservationStatus,
    pub refresh_source_count: u64,
    pub refresh_duration: Duration,
    pub query_duration: Duration,
    pub render_duration: Duration,
    pub backend_requested: crate::SearchBackend,
    pub backend_effective: crate::SearchBackend,
    pub result_count: u64,
    pub citation_count: u64,
    pub zero_result: bool,
    pub has_indexed_content_after: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshObservationMode {
    Background,
    Off,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshObservationStatus {
    ExistingGeneration,
    DaemonBackground,
    DaemonUnavailable,
    Completed,
}

impl HistoryCliObservation {
    pub fn search(facts: SearchExecutionObservation, output_bytes: u64) -> Self {
        Self {
            operation: HistoryCliOperation::Search,
            succeeded: true,
            result_count: Some(facts.result_count),
            output_bytes: Some(output_bytes),
            fields: Vec::new(),
            search: Some(facts),
        }
    }
}

pub trait ObservabilityPort {
    fn observe(&mut self, observation: HistoryCliObservation);
}

#[cfg(test)]
mod tests {
    use super::{OutputStream, TerminalPort};
    use ctx_terminal::{Document, RenderContext, StreamKind, TestContext};
    use std::io;

    struct RecordingPort {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: RenderContext,
    }

    impl TerminalPort for RecordingPort {
        fn context(&self, _stream: OutputStream) -> &RenderContext {
            &self.context
        }

        fn write_document(
            &mut self,
            _stream: OutputStream,
            _document: &Document,
        ) -> io::Result<()> {
            Ok(())
        }

        fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()> {
            match stream {
                OutputStream::Stdout => self.stdout.extend_from_slice(bytes),
                OutputStream::Stderr => self.stderr.extend_from_slice(bytes),
            }
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn terminal_port_preserves_selected_stream_bytes() {
        let mut port = RecordingPort {
            stdout: Vec::new(),
            stderr: Vec::new(),
            context: RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        };
        {
            let terminal: &mut dyn TerminalPort = &mut port;
            terminal.write(OutputStream::Stdout, b"out").unwrap();
            terminal.write(OutputStream::Stderr, b"err").unwrap();
        }
        assert_eq!(port.stdout, b"out");
        assert_eq!(port.stderr, b"err");
    }
}
