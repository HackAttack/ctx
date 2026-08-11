use std::io;

/// The output channel selected by a command body. The final adapter decides
/// how terminal styling and stream handles are implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

pub trait TerminalPort {
    fn write(&mut self, stream: OutputStream, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
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

/// Cardinality-free observation values emitted by history command bodies.
/// Delivery, batching, and product telemetry schemas remain final-adapter
/// responsibilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryCliObservation {
    pub operation: HistoryCliOperation,
    pub succeeded: bool,
    pub result_count: Option<u64>,
    pub output_bytes: Option<u64>,
}

pub trait ObservabilityPort {
    fn observe(&mut self, observation: HistoryCliObservation);
}
