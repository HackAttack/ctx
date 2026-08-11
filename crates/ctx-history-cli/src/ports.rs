use ctx_terminal::{Document, RenderContext, StreamKind, Ui};
use std::{io, time::Duration};

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
        match stream {
            OutputStream::Stdout => self.stdout_writer().write_all(bytes),
            OutputStream::Stderr => self.stderr_writer().write_all(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ui::flush(self)
    }
}

/// Bounded local-usage facts computed by a read command. The final adapter
/// decides whether and how to persist or deliver them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchContextObservation {
    delivered_bytes: Option<usize>,
    matched_bytes: Option<usize>,
}

/// Complete facts from one successful search. This crate measures the command
/// execution; the final binary alone maps and delivers its analytics schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchExecutionObservation {
    pub refresh_mode: crate::RefreshMode,
    pub refresh_status: SearchRefreshStatus,
    pub refresh_source_count: u64,
    pub refresh_duration: Duration,
    pub query_duration: Duration,
    pub render_duration: Duration,
    pub backend_requested: ctx_history_read_application::SearchBackend,
    pub backend_effective: ctx_history_read_application::SearchBackend,
    pub result_count: u64,
    pub citation_count: u64,
    pub zero_result: bool,
    pub has_indexed_content_after: bool,
    pub query_length: u64,
    pub query_term_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRefreshStatus {
    ExistingGeneration,
    DaemonBackground,
    DaemonUnavailable,
    Completed,
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
