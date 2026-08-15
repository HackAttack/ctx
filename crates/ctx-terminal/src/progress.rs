use std::{
    fmt,
    io::{self, Write},
    sync::{mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration as StdDuration, Instant},
};

use serde_json::json;

use crate::ui::{
    refresh_progress, Document, Line, LiveOutput, RefreshProgressSnapshot, Span, Token, Ui,
};

const MAX_PROGRESS_MESSAGE_BYTES: usize = 512;
const MAX_PROGRESS_SOURCE_BYTES: usize = 256;
const MAX_PROGRESS_PHASE_BYTES: usize = 64;
const LIVE_RENDER_INTERVAL: StdDuration = StdDuration::from_millis(100);
const LIVE_BACKEND_SILENCE_TIMEOUT: StdDuration = StdDuration::from_secs(5);

#[derive(Debug, Default)]
struct ActiveElapsedClock {
    displayed_millis: u64,
    observed_at: Option<StdDuration>,
    backend_snapshot_observed_at: Option<StdDuration>,
    backend_elapsed_millis_high_water: Option<u64>,
}

impl ActiveElapsedClock {
    fn advance(
        &mut self,
        reported_millis: Option<u64>,
        now: StdDuration,
        backend_snapshot_received: bool,
    ) -> u64 {
        let local_advance = self
            .observed_at
            .map(|observed_at| duration_millis(now.saturating_sub(observed_at)))
            .unwrap_or_default();
        self.displayed_millis = self
            .displayed_millis
            .saturating_add(local_advance)
            .max(reported_millis.unwrap_or_else(|| duration_millis(now)));
        self.observed_at = Some(now);
        if backend_snapshot_received {
            let first_snapshot = self.backend_snapshot_observed_at.is_none();
            let backend_clock_advanced = reported_millis
                .zip(self.backend_elapsed_millis_high_water)
                .is_some_and(|(reported, high_water)| reported > high_water);
            if first_snapshot || backend_clock_advanced {
                self.backend_snapshot_observed_at = Some(now);
            }
            if let Some(reported_millis) = reported_millis {
                self.backend_elapsed_millis_high_water = Some(
                    self.backend_elapsed_millis_high_water
                        .map_or(reported_millis, |high_water| {
                            high_water.max(reported_millis)
                        }),
                );
            }
        }
        self.displayed_millis
    }

    fn backend_snapshot_silent(&self, now: StdDuration) -> bool {
        self.backend_snapshot_observed_at
            .is_some_and(|observed_at| {
                now.saturating_sub(observed_at) >= LIVE_BACKEND_SILENCE_TIMEOUT
            })
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

fn duration_millis(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Plain,
    Json,
    None,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgressRenderMode {
    None,
    Live,
    Plain,
    Json,
}

#[derive(Debug)]
pub struct ProgressWriterError(io::Error);

impl From<io::Error> for ProgressWriterError {
    fn from(error: io::Error) -> Self {
        Self(error)
    }
}

impl fmt::Display for ProgressWriterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "write progress output: {}", self.0)
    }
}

impl std::error::Error for ProgressWriterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub struct ProgressReporter<'a> {
    mode: ProgressRenderMode,
    operation: &'static str,
    total_bytes: u64,
    started: Instant,
    presentation_agent_histories: Option<Vec<String>>,
    output: ProgressOutput<'a>,
}

enum ProgressOutput<'a> {
    Direct(LiveOutput<&'a mut (dyn Write + Send)>),
    Live(LocalLiveRenderer),
}

impl<'a> ProgressOutput<'a> {
    fn direct_mut<'output>(
        &'output mut self,
    ) -> io::Result<&'output mut LiveOutput<&'a mut (dyn Write + Send)>> {
        match self {
            Self::Direct(output) => Ok(output),
            Self::Live(_) => Err(io::Error::other("live renderer has no direct writer")),
        }
    }

    fn write_live_document(&mut self, document: Document, final_frame: bool) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_document(document, final_frame),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }

    fn write_live_refresh(&mut self, snapshot: RefreshProgressSnapshot) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_refresh(snapshot),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }

    fn write_live_notice(&mut self, document: Document) -> io::Result<()> {
        match self {
            Self::Live(output) => output.write_notice(document),
            Self::Direct(_) => Err(io::Error::other("direct renderer has no live worker")),
        }
    }
}

enum LiveRenderCommand {
    Document {
        document: Document,
        final_frame: bool,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Refresh {
        snapshot: Box<RefreshProgressSnapshot>,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Notice {
        document: Document,
        complete: mpsc::Sender<io::Result<()>>,
    },
    Shutdown,
}

struct LocalLiveRenderer {
    commands: mpsc::Sender<LiveRenderCommand>,
    background_error: Arc<Mutex<Option<io::Error>>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveRefreshPresentation {
    Shared,
    Setup,
}

impl LocalLiveRenderer {
    fn new(
        output: LiveOutput<Box<dyn Write + Send>>,
        started: Instant,
        presentation: LiveRefreshPresentation,
    ) -> Self {
        let (commands, receiver) = mpsc::channel();
        let background_error = Arc::new(Mutex::new(None));
        let worker_error = Arc::clone(&background_error);
        let worker = thread::spawn(move || {
            run_live_renderer(output, receiver, started, presentation, &worker_error);
        });
        Self {
            commands,
            background_error,
            worker: Some(worker),
        }
    }

    fn write_document(&mut self, document: Document, final_frame: bool) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Document {
                document,
                final_frame,
                complete,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn write_refresh(&mut self, snapshot: RefreshProgressSnapshot) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Refresh {
                snapshot: Box::new(snapshot),
                complete,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn write_notice(&mut self, document: Document) -> io::Result<()> {
        self.check_background_error()?;
        let (complete, completed) = mpsc::channel();
        self.commands
            .send(LiveRenderCommand::Notice { document, complete })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?;
        completed
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "live renderer stopped"))?
    }

    fn check_background_error(&self) -> io::Result<()> {
        let mut error = self
            .background_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        error.take().map_or(Ok(()), Err)
    }
}

impl Drop for LocalLiveRenderer {
    fn drop(&mut self) {
        let _ = self.commands.send(LiveRenderCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_live_renderer(
    mut output: LiveOutput<Box<dyn Write + Send>>,
    commands: mpsc::Receiver<LiveRenderCommand>,
    started: Instant,
    presentation: LiveRefreshPresentation,
    background_error: &Mutex<Option<io::Error>>,
) {
    let mut active = None;
    let mut persistent_notice = None;
    let mut clock = ActiveElapsedClock::default();
    loop {
        match commands.recv_timeout(LIVE_RENDER_INTERVAL) {
            Ok(LiveRenderCommand::Document {
                document,
                final_frame,
                complete,
            }) => {
                active = None;
                clock.reset();
                let result = output.write_frame(&document, final_frame);
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
            }
            Ok(LiveRenderCommand::Refresh { snapshot, complete }) => {
                let terminal = snapshot.is_terminal();
                let rendered =
                    prepare_live_snapshot((*snapshot).clone(), &mut clock, started.elapsed(), true);
                let context = *output.context();
                let document = render_live_refresh(
                    presentation,
                    &context,
                    rendered,
                    persistent_notice.as_ref(),
                );
                let result = output.write_frame(&document, terminal);
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
                active = (!terminal).then_some(*snapshot);
                if terminal {
                    clock.reset();
                }
            }
            Ok(LiveRenderCommand::Notice { document, complete }) => {
                persistent_notice = Some(document);
                let context = *output.context();
                let document = active.as_ref().map_or_else(
                    || persistent_notice.as_ref().cloned().unwrap_or_default(),
                    |snapshot| {
                        let rendered = prepare_live_snapshot(
                            snapshot.clone(),
                            &mut clock,
                            started.elapsed(),
                            false,
                        );
                        render_live_refresh(
                            presentation,
                            &context,
                            rendered,
                            persistent_notice.as_ref(),
                        )
                    },
                );
                let result = output.write_frame(&document, false);
                let failed = result.is_err();
                let _ = complete.send(result);
                if failed {
                    break;
                }
            }
            Ok(LiveRenderCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let Some(snapshot) = active.as_ref() else {
                    continue;
                };
                let rendered =
                    prepare_live_snapshot(snapshot.clone(), &mut clock, started.elapsed(), false);
                let context = *output.context();
                let document = render_live_refresh(
                    presentation,
                    &context,
                    rendered,
                    persistent_notice.as_ref(),
                );
                if let Err(error) = output.write_frame(&document, false) {
                    *background_error
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
                    break;
                }
            }
        }
    }
}

fn render_live_refresh(
    presentation: LiveRefreshPresentation,
    context: &crate::ui::RenderContext,
    mut snapshot: RefreshProgressSnapshot,
    persistent_notice: Option<&Document>,
) -> Document {
    let terminal = snapshot.is_terminal();
    if presentation == LiveRefreshPresentation::Setup {
        snapshot.use_setup_live_presentation();
    }
    let mut document = refresh_progress(context, &snapshot);
    if let Some(notice) = persistent_notice {
        document.append(notice.clone());
        if terminal {
            // Terminal setup frames intentionally omit the no-longer-relevant
            // ETA row. Keep the live block's height stable through that final
            // differential repaint without adding space before the notice.
            document.push_blank();
        }
    }
    document
}

fn prepare_live_snapshot(
    mut snapshot: RefreshProgressSnapshot,
    clock: &mut ActiveElapsedClock,
    now: StdDuration,
    backend_snapshot_received: bool,
) -> RefreshProgressSnapshot {
    if !snapshot.is_terminal() {
        let elapsed = clock.advance(
            snapshot.progress().elapsed_millis,
            now,
            backend_snapshot_received,
        );
        if clock.backend_snapshot_silent(now) {
            snapshot.suppress_stale_presentation_eta();
        }
        snapshot.advance_presentation_clock(elapsed);
    }
    snapshot
}

impl<'a> ProgressReporter<'a> {
    pub fn new(
        ui: &'a mut Ui,
        arg: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
    ) -> Self {
        Self::new_with_live_json_stderr(ui, arg, json_output, operation, total_bytes, false)
    }

    pub fn new_with_live_json_stderr(
        ui: &'a mut Ui,
        arg: ProgressMode,
        json_output: bool,
        operation: &'static str,
        total_bytes: u64,
        allow_live_json_stderr: bool,
    ) -> Self {
        let live_output_capable = ui.stderr_context().live_output_capable();
        let mode = match arg {
            ProgressMode::None => ProgressRenderMode::None,
            ProgressMode::Json => ProgressRenderMode::Json,
            ProgressMode::Plain => ProgressRenderMode::Plain,
            ProgressMode::Auto
                if !live_output_capable || (json_output && !allow_live_json_stderr) =>
            {
                ProgressRenderMode::None
            }
            ProgressMode::Auto => ProgressRenderMode::Live,
        };
        let started = Instant::now();
        let output = if mode == ProgressRenderMode::Live {
            let presentation = if operation == "setup" {
                LiveRefreshPresentation::Setup
            } else {
                LiveRefreshPresentation::Shared
            };
            ProgressOutput::Live(LocalLiveRenderer::new(
                ui.stderr_shared_live_output(),
                started,
                presentation,
            ))
        } else {
            ProgressOutput::Direct(ui.stderr_live_output())
        };
        Self {
            mode,
            operation,
            total_bytes,
            started,
            presentation_agent_histories: None,
            output,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.mode != ProgressRenderMode::None
    }

    pub fn message(
        &mut self,
        phase: &'static str,
        message: impl Into<String>,
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.presentation_agent_histories = None;
        let message = bounded_progress_text(&message.into(), MAX_PROGRESS_MESSAGE_BYTES);
        self.emit_status(ProgressLine {
            phase: bounded_progress_text(phase, MAX_PROGRESS_PHASE_BYTES),
            message,
            completed_bytes: 0,
            total_bytes: self.total_bytes,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: false,
            refresh: None,
        })
    }

    /// Emits a trusted, source-authored multi-line notice through the active
    /// progress transport. Unlike dynamic progress messages, line boundaries
    /// are preserved so a live or hosted renderer can present the notice while
    /// another operation continues.
    pub fn notice(
        &mut self,
        phase: &'static str,
        lines: &[&str],
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() || lines.is_empty() {
            return Ok(());
        }
        self.presentation_agent_histories = None;
        let lines = lines
            .iter()
            .map(|line| bounded_progress_text(line, MAX_PROGRESS_MESSAGE_BYTES))
            .collect::<Vec<_>>();
        let message = lines.join("\n");
        let elapsed = self.started.elapsed();
        match self.mode {
            ProgressRenderMode::None => Ok(()),
            ProgressRenderMode::Live => {
                let mut document = Document::new();
                document.push_blank();
                for line in lines {
                    document.push_line(Line::new().with(Span::new(line, Token::Text)));
                }
                self.output
                    .write_live_notice(document)
                    .map_err(ProgressWriterError)
            }
            ProgressRenderMode::Plain => self
                .output
                .direct_mut()
                .and_then(|output| output.write_line(&message))
                .map_err(ProgressWriterError),
            ProgressRenderMode::Json => write_progress(
                &mut self.output,
                self.mode,
                self.operation,
                &ProgressLine {
                    phase: bounded_progress_text(phase, MAX_PROGRESS_PHASE_BYTES),
                    message,
                    completed_bytes: 0,
                    total_bytes: self.total_bytes,
                    completed_files: None,
                    total_files: None,
                    imported_events: None,
                    done: false,
                    refresh: None,
                },
                elapsed,
            )
            .map_err(ProgressWriterError),
        }
    }

    pub fn source_refresh(
        &mut self,
        snapshot: RefreshProgressSnapshot,
    ) -> Result<(), ProgressWriterError> {
        let now = self.started.elapsed();
        self.source_refresh_at(snapshot, now)
    }

    fn source_refresh_at(
        &mut self,
        mut snapshot: RefreshProgressSnapshot,
        now: StdDuration,
    ) -> Result<(), ProgressWriterError> {
        if !self.is_enabled() {
            return Ok(());
        }
        if self.mode == ProgressRenderMode::Live {
            if self.presentation_agent_histories.is_none()
                && snapshot.discovery_complete()
                && !snapshot.progress().agent_histories.is_empty()
            {
                self.presentation_agent_histories =
                    Some(snapshot.progress().agent_histories.clone());
            }
            snapshot.set_presentation_agent_histories(self.presentation_agent_histories.clone());
        }
        let line = source_refresh_line(snapshot, self.total_bytes);
        write_progress(&mut self.output, self.mode, self.operation, &line, now)
            .map_err(ProgressWriterError)
    }

    fn emit_status(&mut self, line: ProgressLine) -> Result<(), ProgressWriterError> {
        let elapsed = self.started.elapsed();
        write_progress(&mut self.output, self.mode, self.operation, &line, elapsed)
            .map_err(ProgressWriterError)
    }
}

struct ProgressLine {
    phase: String,
    message: String,
    completed_bytes: u64,
    total_bytes: u64,
    completed_files: Option<usize>,
    total_files: Option<usize>,
    imported_events: Option<usize>,
    done: bool,
    refresh: Option<RefreshProgressSnapshot>,
}

fn write_progress(
    output: &mut ProgressOutput<'_>,
    mode: ProgressRenderMode,
    operation: &'static str,
    line: &ProgressLine,
    elapsed: StdDuration,
) -> io::Result<()> {
    match mode {
        ProgressRenderMode::None => Ok(()),
        ProgressRenderMode::Live => {
            if let Some(snapshot) = line.refresh.as_ref() {
                output.write_live_refresh(snapshot.clone())
            } else {
                let document =
                    Document::from_line(Line::new().with(Span::new(&line.message, Token::Text)));
                output.write_live_document(document, line.done)
            }
        }
        ProgressRenderMode::Plain => {
            let output = output.direct_mut()?;
            if let Some(snapshot) = line.refresh.as_ref() {
                let document = refresh_progress(output.context(), snapshot);
                output.write_line(document.render_plain().trim_end_matches('\n'))
            } else {
                output.write_line(&line.message)
            }
        }
        ProgressRenderMode::Json => output
            .direct_mut()?
            .write_line(&progress_json(operation, line, elapsed)),
    }
}

fn progress_json(operation: &'static str, line: &ProgressLine, elapsed: StdDuration) -> String {
    let (completed_bytes, total_bytes) = progress_line_bytes(line);
    let mut value = json!({
        "type": "ctx_progress",
        "operation": operation,
        "phase": line.phase,
        "message": line.message,
        "completed_bytes": completed_bytes,
        "total_bytes": total_bytes,
        "percent": progress_line_percent(line),
        "elapsed_seconds": elapsed.as_secs_f64(),
        // Compatibility: this documented legacy field remains byte-rate based.
        // Source-backed consumers use estimated_remaining_millis below for the
        // explicit whole-run time until the refreshed generation is usable.
        "eta_seconds": progress_line_eta_seconds(line, elapsed),
        "completed_files": line.completed_files,
        "total_files": line.total_files,
        "imported_events": line.imported_events,
        "done": line.done,
    });
    if let Some(snapshot) = line.refresh.as_ref() {
        let progress = snapshot.progress();
        value["completed_sources"] = json!(progress.completed_sources);
        value["total_sources"] = json!(progress.total_sources);
        value["total_sources_known"] = json!(snapshot.total_sources_known());
        value["source_completed_records"] = json!(progress.completed_records);
        value["source_completed_bytes"] = json!(progress.completed_bytes);
        value["agent_histories"] = json!(progress.agent_histories);
        value["processed_sessions"] = json!(progress.processed_sessions);
        value["processed_messages"] = json!(progress.processed_messages);
        value["processed_tool_calls"] = json!(progress.processed_tool_calls);
        value["processed_bytes"] = json!(progress.processed_bytes);
        value["whole_run_stage"] = json!(progress.whole_run_stage.as_str());
        value["estimated_remaining_millis"] = json!(progress.estimated_remaining_millis);
        value["refresh_elapsed_millis"] = json!(progress.elapsed_millis);
        value["current_source"] = json!(progress
            .current_source
            .as_deref()
            .map(|source| bounded_progress_text(source, MAX_PROGRESS_SOURCE_BYTES)));
        value["current_source_progress"] = progress
            .current_source_progress
            .as_ref()
            .map(crate::ui::RefreshCurrentSourceProgress::to_json)
            .unwrap_or(serde_json::Value::Null);
        snapshot.append_json_fields(&mut value);
    }
    value.to_string()
}

fn source_refresh_line(
    snapshot: RefreshProgressSnapshot,
    legacy_terminal_total_bytes: u64,
) -> ProgressLine {
    let (completed_bytes, engine_total_bytes) = snapshot.byte_progress();
    let phase = snapshot.phase();
    let message = snapshot.message();
    let done = snapshot.is_terminal();
    let total_bytes = if done && (completed_bytes, engine_total_bytes) == (0, 0) {
        legacy_terminal_total_bytes
    } else {
        engine_total_bytes
    };
    let imported_events = snapshot
        .progress()
        .completed_records
        .and_then(|value| usize::try_from(value).ok());
    ProgressLine {
        phase: bounded_progress_text(&phase, MAX_PROGRESS_PHASE_BYTES),
        message: bounded_progress_text(&message, MAX_PROGRESS_MESSAGE_BYTES),
        completed_bytes,
        total_bytes,
        completed_files: None,
        total_files: None,
        imported_events,
        done,
        refresh: Some(snapshot),
    }
}

fn progress_percent(completed: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    ((completed as f64 / total as f64) * 100.0).clamp(0.0, 100.0)
}

fn progress_line_bytes(line: &ProgressLine) -> (u64, u64) {
    let total_bytes = line.total_bytes.max(line.completed_bytes);
    let completed_bytes = if line.done {
        total_bytes
    } else {
        line.completed_bytes.min(total_bytes)
    };
    (completed_bytes, total_bytes)
}

fn progress_line_percent(line: &ProgressLine) -> f64 {
    if line.done && line.total_bytes.max(line.completed_bytes) != 0 {
        100.0
    } else {
        let (completed_bytes, total_bytes) = progress_line_bytes(line);
        progress_percent(completed_bytes, total_bytes)
    }
}

fn progress_line_eta_seconds(line: &ProgressLine, elapsed: StdDuration) -> Option<f64> {
    if line.done {
        None
    } else {
        let (completed_bytes, total_bytes) = progress_line_bytes(line);
        eta_seconds(completed_bytes, total_bytes, elapsed)
    }
}

fn eta_seconds(completed: u64, total: u64, elapsed: StdDuration) -> Option<f64> {
    if completed == 0 || total <= completed {
        return None;
    }
    let rate = completed as f64 / elapsed.as_secs_f64().max(0.001);
    if rate <= 0.0 {
        return None;
    }
    Some((total - completed) as f64 / rate)
}

fn bounded_progress_text(value: &str, max_bytes: usize) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= max_bytes {
        return sanitized;
    }
    const SUFFIX: &str = "...";
    let mut end = max_bytes.saturating_sub(SUFFIX.len()).min(sanitized.len());
    while end > 0 && !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = sanitized[..end].to_owned();
    bounded.push_str(SUFFIX);
    bounded
}

pub fn format_bytes(bytes: u64) -> String {
    let (value, unit) = scaled_bytes(bytes);
    if unit == "B" {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn scaled_bytes(bytes: u64) -> (f64, &'static str) {
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < BYTE_UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    (value, BYTE_UNITS[unit])
}

const BYTE_UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

pub fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first_group_len = digits.len() % 3;
    for (index, ch) in digits.chars().enumerate() {
        if index > 0
            && (index == first_group_len
                || (index > first_group_len && (index - first_group_len).is_multiple_of(3)))
        {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn active_status() -> RefreshProgressSnapshot {
        RefreshProgressSnapshot::new(
            Some("logical-request".to_owned()),
            crate::ui::RefreshStatusKind::Logical(crate::ui::RefreshLogicalStatus {
                request_state: crate::ui::RefreshRequestState::Running,
                logical_phase: crate::ui::RefreshLogicalPhase::Direct,
                physical_attempt_id: "physical-attempt".to_owned(),
                physical_attempt_state: crate::ui::RefreshRequestState::Running,
                progress_owner_request_id: "physical-attempt".to_owned(),
                progress_owner_attempt_state: crate::ui::RefreshRequestState::Running,
                structured_outcome: None,
            }),
            crate::ui::RefreshProgress {
                phase: "refreshing".to_owned(),
                completed_sources: 1,
                total_sources: 2,
                current_source: Some("/tmp/history\ncontrol.sqlite".to_owned()),
                completed_records: Some(4_096),
                completed_bytes: Some(2_048),
                agent_histories: vec!["Codex".to_owned(), "Claude".to_owned()],
                processed_sessions: 123,
                processed_messages: 4_000,
                processed_tool_calls: 96,
                processed_bytes: 2_048,
                elapsed_millis: Some(65_000),
                whole_run_stage: crate::ui::RefreshWholeRunStage::Reading,
                estimated_remaining_millis: None,
                current_source_progress: Some(crate::ui::RefreshCurrentSourceProgress {
                    stage: crate::ui::RefreshCurrentSourceProgressStage::LogicalScan,
                    snapshot_pages_completed: None,
                    snapshot_pages_total: None,
                    snapshot_bytes_completed: None,
                    snapshot_bytes_total: None,
                    logical_rows_scanned: Some(4_096),
                    logical_certified_bytes: Some(2_048),
                }),
            },
            true,
        )
    }

    fn active_transfer_status() -> RefreshProgressSnapshot {
        RefreshProgressSnapshot::new(
            Some("explicit-import-request".to_owned()),
            crate::ui::RefreshStatusKind::Logical(crate::ui::RefreshLogicalStatus {
                request_state: crate::ui::RefreshRequestState::Running,
                logical_phase: crate::ui::RefreshLogicalPhase::Attached,
                physical_attempt_id: "shared-physical-attempt".to_owned(),
                physical_attempt_state: crate::ui::RefreshRequestState::Running,
                progress_owner_request_id: "shared-physical-attempt".to_owned(),
                progress_owner_attempt_state: crate::ui::RefreshRequestState::Running,
                structured_outcome: None,
            }),
            crate::ui::RefreshProgress {
                phase: "copying".to_owned(),
                completed_sources: 1,
                total_sources: 3,
                current_source: Some("/explicit.sqlite".to_owned()),
                completed_records: Some(100),
                completed_bytes: Some(777),
                agent_histories: vec!["Codex".to_owned()],
                processed_sessions: 8,
                processed_messages: 80,
                processed_tool_calls: 20,
                processed_bytes: 777,
                elapsed_millis: Some(2_000),
                whole_run_stage: crate::ui::RefreshWholeRunStage::Reading,
                estimated_remaining_millis: None,
                current_source_progress: Some(crate::ui::RefreshCurrentSourceProgress {
                    stage: crate::ui::RefreshCurrentSourceProgressStage::OnlineBackup,
                    snapshot_pages_completed: None,
                    snapshot_pages_total: None,
                    snapshot_bytes_completed: Some(256),
                    snapshot_bytes_total: Some(512),
                    logical_rows_scanned: None,
                    logical_certified_bytes: None,
                }),
            },
            true,
        )
    }

    fn terminal_status() -> RefreshProgressSnapshot {
        terminal_status_with(
            crate::ui::RefreshRequestState::Published,
            "completed",
            "completed",
            false,
        )
    }

    fn terminal_status_with(
        state: crate::ui::RefreshRequestState,
        code: &str,
        class: &str,
        failure: bool,
    ) -> RefreshProgressSnapshot {
        RefreshProgressSnapshot::new(
            Some("logical-request".to_owned()),
            crate::ui::RefreshStatusKind::Logical(crate::ui::RefreshLogicalStatus {
                request_state: state,
                logical_phase: crate::ui::RefreshLogicalPhase::Terminal,
                physical_attempt_id: "physical-attempt".to_owned(),
                physical_attempt_state: state,
                progress_owner_request_id: "physical-attempt".to_owned(),
                progress_owner_attempt_state: state,
                structured_outcome: Some(Box::new(crate::ui::RefreshStructuredOutcome {
                    code: code.to_owned(),
                    class: class.to_owned(),
                    retryable: false,
                    affected_routes: Vec::new(),
                    retryable_routes: Vec::new(),
                    blocked_routes: Vec::new(),
                    physical_attempt_id: "physical-attempt".to_owned(),
                    retained_generation: None,
                    published_generation: None,
                    retry_advice: None,
                    detail: None,
                    failure,
                })),
            }),
            crate::ui::RefreshProgress {
                phase: if state == crate::ui::RefreshRequestState::Failed {
                    "failed".to_owned()
                } else {
                    "committed".to_owned()
                },
                completed_sources: 2,
                total_sources: 2,
                current_source: None,
                completed_records: None,
                completed_bytes: None,
                whole_run_stage: if state == crate::ui::RefreshRequestState::Failed {
                    crate::ui::RefreshWholeRunStage::Failed
                } else {
                    crate::ui::RefreshWholeRunStage::Complete
                },
                ..Default::default()
            },
            true,
        )
    }

    fn ui_with_stderr(
        stderr: SharedWriter,
        stderr_context: crate::ui::RenderContext,
    ) -> (Ui, SharedWriter) {
        let stdout = SharedWriter::default();
        let stdout_capture = stdout.clone();
        let stdout_context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stdout,
        ));
        (
            Ui::with_writers(stdout, stdout_context, stderr, stderr_context),
            stdout_capture,
        )
    }

    mod eta_tests;
    mod notice_tests;

    #[test]
    fn progress_mode_matrix_uses_injected_stderr_and_keeps_stdout_clean() {
        let cases = [
            (ProgressMode::Auto, true, false, false, true),
            (ProgressMode::Auto, false, false, false, false),
            (ProgressMode::Auto, true, false, true, false),
            (ProgressMode::Auto, true, true, false, false),
            (ProgressMode::Plain, false, false, false, true),
            (ProgressMode::Plain, true, false, false, true),
            (ProgressMode::Json, false, false, false, true),
            (ProgressMode::Json, true, false, false, true),
            (ProgressMode::None, true, false, false, false),
        ];
        for (arg, stderr_tty, term_dumb, final_json, expected_output) in cases {
            let stderr = SharedWriter::default();
            let stderr_capture = stderr.clone();
            let test_context = if stderr_tty {
                crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, 80).term_dumb(term_dumb)
            } else {
                crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr)
            };
            let (mut ui, stdout_capture) =
                ui_with_stderr(stderr, crate::ui::RenderContext::for_test(test_context));
            {
                let mut reporter = ProgressReporter::new(&mut ui, arg, final_json, "import", 0);
                reporter.source_refresh(active_status()).unwrap();
            }
            assert_eq!(
                !stderr_capture.text().is_empty(),
                expected_output,
                "mode={arg:?}, tty={stderr_tty}, term_dumb={term_dumb}, final_json={final_json}"
            );
            assert!(stdout_capture.text().is_empty());
            if arg == ProgressMode::Plain {
                assert!(!stderr_capture.text().contains('\u{1b}'));
            }
            if arg == ProgressMode::Json {
                let value: serde_json::Value =
                    serde_json::from_str(stderr_capture.text().trim()).unwrap();
                assert_eq!(value["type"], "ctx_progress");
                assert_eq!(value["logical_phase"], "direct");
            }
        }
    }

    #[test]
    fn plain_refresh_progress_is_the_stable_live_document_without_internal_routes() {
        let stderr = SharedWriter::default();
        let stderr_capture = stderr.clone();
        let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stderr,
        ));
        let shared_document =
            crate::ui::refresh_progress(&context, &active_status()).render_plain();
        let (mut ui, stdout_capture) = ui_with_stderr(stderr, context);

        let mut reporter = ProgressReporter::new(&mut ui, ProgressMode::Plain, false, "setup", 0);
        reporter.source_refresh(active_status()).unwrap();

        assert_eq!(stderr_capture.text(), shared_document);
        assert_eq!(
            shared_document,
            concat!(
                "Indexing your agent history\n",
                "──────────────━━━━━━━━──────────────────────────\n",
                "\n",
                "Agent histories  Codex\n",
                "                 Claude\n",
                "Sessions         123\n",
                "Messages         4,000\n",
                "Tool calls       96\n",
                "Data scanned     2.0 KiB\n",
                "Elapsed          1m 05s\n",
                "Remaining        estimating\n",
            )
        );
        assert!(stdout_capture.text().is_empty());
        assert!(!stderr_capture.text().contains("/tmp/history"));
        assert!(!stderr_capture.text().contains("1 / 2"));
        assert!(!stderr_capture.text().contains('\u{1b}'));
    }

    #[test]
    fn provider_rows_freeze_after_discovery_for_the_live_lifecycle() {
        let stderr = SharedWriter::default();
        let stderr_capture = stderr.clone();
        let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
            crate::ui::StreamKind::Stderr,
            80,
        ));
        let (mut ui, _) = ui_with_stderr(stderr, context);
        let mut reporter = ProgressReporter::new(&mut ui, ProgressMode::Auto, false, "setup", 0);

        let mut discovery = active_status();
        discovery.progress_mut_for_test().phase = "discovering".to_owned();
        reporter
            .source_refresh_at(discovery, StdDuration::ZERO)
            .unwrap();
        assert!(!stderr_capture.text().contains("Agent histories"));

        let scan = active_status();
        reporter
            .source_refresh_at(scan.clone(), StdDuration::from_millis(100))
            .unwrap();
        let mut later = scan;
        later
            .progress_mut_for_test()
            .agent_histories
            .push("Late provider".to_owned());
        later.progress_mut_for_test().phase = "committing".to_owned();
        reporter
            .source_refresh_at(later, StdDuration::from_millis(200))
            .unwrap();
        drop(reporter);

        let output = stderr_capture.text();
        assert!(!output.contains("Late provider"), "{output:?}");
        assert_eq!(output.matches("Agent histories").count(), 1, "{output:?}");
    }

    #[test]
    fn active_and_terminal_refresh_jsonl_contract_is_exact() {
        let active = progress_json(
            "import",
            &source_refresh_line(active_transfer_status(), 4_096),
            StdDuration::from_secs(2),
        );
        let terminal = progress_json(
            "import",
            &source_refresh_line(terminal_status(), 4_096),
            StdDuration::from_secs(2),
        );

        assert_eq!(
            active,
            r#"{"agent_histories":["Codex"],"completed_bytes":256,"completed_files":null,"completed_sources":1,"current_source":"/explicit.sqlite","current_source_progress":{"snapshot_bytes_completed":256,"snapshot_bytes_total":512,"stage":"online_backup"},"done":false,"elapsed_seconds":2.0,"estimated_remaining_millis":null,"eta_seconds":2.0,"imported_events":100,"logical_phase":"attached","logical_request_id":"explicit-import-request","message":"Refreshing history with shared work: /explicit.sqlite (1 / 3).","operation":"import","percent":50.0,"phase":"online_backup","physical_attempt_id":"shared-physical-attempt","physical_attempt_state":"running","processed_bytes":777,"processed_messages":80,"processed_sessions":8,"processed_tool_calls":20,"progress_owner_attempt_state":"running","progress_owner_request_id":"shared-physical-attempt","refresh_elapsed_millis":2000,"request_id":"explicit-import-request","request_state":"running","source_completed_bytes":777,"source_completed_records":100,"total_bytes":512,"total_files":null,"total_sources":3,"total_sources_known":true,"type":"ctx_progress","whole_run_stage":"reading"}"#
        );
        assert_eq!(
            terminal,
            r#"{"agent_histories":[],"completed_bytes":4096,"completed_files":null,"completed_sources":2,"current_source":null,"current_source_progress":null,"done":true,"elapsed_seconds":2.0,"estimated_remaining_millis":null,"eta_seconds":null,"imported_events":null,"logical_phase":"terminal","logical_request_id":"logical-request","message":"History refresh complete (2 / 2).","operation":"import","percent":100.0,"phase":"published","physical_attempt_id":"physical-attempt","physical_attempt_state":"published","processed_bytes":0,"processed_messages":0,"processed_sessions":0,"processed_tool_calls":0,"progress_owner_attempt_state":"published","progress_owner_request_id":"physical-attempt","refresh_elapsed_millis":null,"request_id":"logical-request","request_state":"published","source_completed_bytes":null,"source_completed_records":null,"structured_outcome":{"affected_routes":[],"blocked_routes":[],"class":"completed","code":"completed","physical_attempt_id":"physical-attempt","retryable":false,"retryable_routes":[]},"total_bytes":4096,"total_files":null,"total_sources":2,"total_sources_known":true,"type":"ctx_progress","whole_run_stage":"complete"}"#
        );

        let events = [&active, &terminal]
            .into_iter()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            events.iter().filter(|event| event["done"] == true).count(),
            1
        );
        assert_eq!(
            (
                events[0]["completed_bytes"].as_u64(),
                events[0]["total_bytes"].as_u64()
            ),
            (Some(256), Some(512))
        );
        assert_eq!(events[0]["percent"], 50.0);
        assert_eq!(events[0]["eta_seconds"], 2.0);
        assert_eq!(
            events[0]["estimated_remaining_millis"],
            serde_json::Value::Null
        );
        assert_ne!(
            events[0]["logical_request_id"],
            events[0]["progress_owner_request_id"]
        );
    }

    #[test]
    fn setup_jsonl_holds_legacy_source_eta_but_never_promotes_it_to_whole_run_eta() {
        let value: serde_json::Value = serde_json::from_str(&progress_json(
            "setup",
            &source_refresh_line(active_transfer_status(), 0),
            StdDuration::from_secs(2),
        ))
        .unwrap();

        // eta_seconds is the documented legacy byte-rate field. Preserve it
        // for compatibility; the explicit whole-run field is authoritative for
        // time until setup is usable.
        assert_eq!(value["eta_seconds"], 2.0);
        assert_eq!(value["whole_run_stage"], "reading");
        assert_eq!(value["estimated_remaining_millis"], serde_json::Value::Null);
    }

    #[test]
    fn failed_setup_snapshot_is_failed_in_json_and_live_presentation() {
        let mut snapshot = terminal_status_with(
            crate::ui::RefreshRequestState::Failed,
            "source_refresh_failed",
            "internal",
            true,
        );
        snapshot.use_setup_live_presentation();
        let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::tty(
            crate::ui::StreamKind::Stderr,
            80,
        ));
        let rendered = refresh_progress(&context, &snapshot).render_plain();
        assert!(
            rendered.starts_with("History refresh failed\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("Preparing"), "{rendered}");

        let json = progress_json(
            "setup",
            &source_refresh_line(snapshot, 4_096),
            StdDuration::from_secs(2),
        );
        assert_eq!(
            json,
            r#"{"agent_histories":[],"completed_bytes":4096,"completed_files":null,"completed_sources":2,"current_source":null,"current_source_progress":null,"done":true,"elapsed_seconds":2.0,"estimated_remaining_millis":null,"eta_seconds":null,"imported_events":null,"logical_phase":"terminal","logical_request_id":"logical-request","message":"History refresh failed (2 / 2).","operation":"setup","percent":100.0,"phase":"failed","physical_attempt_id":"physical-attempt","physical_attempt_state":"failed","processed_bytes":0,"processed_messages":0,"processed_sessions":0,"processed_tool_calls":0,"progress_owner_attempt_state":"failed","progress_owner_request_id":"physical-attempt","refresh_elapsed_millis":null,"request_id":"logical-request","request_state":"failed","source_completed_bytes":null,"source_completed_records":null,"structured_outcome":{"affected_routes":[],"blocked_routes":[],"class":"internal","code":"source_refresh_failed","physical_attempt_id":"physical-attempt","retryable":false,"retryable_routes":[]},"total_bytes":4096,"total_files":null,"total_sources":2,"total_sources_known":true,"type":"ctx_progress","whole_run_stage":"failed"}"#
        );
    }

    #[test]
    fn refresh_jsonl_preserves_base_commit_and_verify_messages() {
        for (phase, expected) in [
            ("committing", "Publishing search index (1 / 2)."),
            ("verifying", "Verifying refreshed history (1 / 2)."),
        ] {
            let mut snapshot = active_status();
            snapshot.progress_mut_for_test().phase = phase.to_owned();
            snapshot.progress_mut_for_test().current_source = None;
            snapshot.progress_mut_for_test().current_source_progress = None;
            let line = source_refresh_line(snapshot, 4_096);
            let value: serde_json::Value =
                serde_json::from_str(&progress_json("setup", &line, StdDuration::from_secs(2)))
                    .unwrap();

            assert_eq!(value["message"], expected);
        }
    }

    #[test]
    fn done_progress_json_forces_complete_bytes_with_incomplete_bytes() {
        let line = ProgressLine {
            phase: "finalizing".to_owned(),
            message: "done".to_owned(),
            completed_bytes: 0,
            total_bytes: 4 * 1024,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: true,
            refresh: None,
        };

        let value: serde_json::Value =
            serde_json::from_str(&progress_json("setup", &line, StdDuration::from_secs(120)))
                .expect("progress json should parse");

        assert_eq!(value["completed_bytes"], 4 * 1024);
        assert_eq!(value["total_bytes"], 4 * 1024);
        assert_eq!(value["percent"], 100.0);
        assert_eq!(value["eta_seconds"], serde_json::Value::Null);
        assert_eq!(value["done"], true);
    }

    #[test]
    fn progress_json_remains_exact_and_ansi_free() {
        let line = ProgressLine {
            phase: "cataloging".to_owned(),
            message: "cataloging".to_owned(),
            completed_bytes: 1024,
            total_bytes: 4096,
            completed_files: Some(1),
            total_files: Some(2),
            imported_events: Some(7),
            done: false,
            refresh: None,
        };

        let rendered = progress_json("import", &line, StdDuration::from_secs(2));

        assert_eq!(
            rendered,
            concat!(
                r#"{"completed_bytes":1024,"completed_files":1,"done":false,"#,
                r#""elapsed_seconds":2.0,"eta_seconds":6.0,"imported_events":7,"#,
                r#""message":"cataloging","operation":"import","percent":25.0,"#,
                r#""phase":"cataloging","total_bytes":4096,"total_files":2,"#,
                r#""type":"ctx_progress"}"#,
            )
        );
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn plain_and_json_progress_keep_explicit_stream_contracts() {
        let line = ProgressLine {
            phase: "indexing".to_owned(),
            message: "Indexed 2 sources".to_owned(),
            completed_bytes: 2,
            total_bytes: 4,
            completed_files: Some(2),
            total_files: Some(4),
            imported_events: None,
            done: false,
            refresh: None,
        };

        let plain = match ProgressRenderMode::Plain {
            ProgressRenderMode::Plain => line.message.as_str(),
            _ => unreachable!(),
        };
        let json = match ProgressRenderMode::Json {
            ProgressRenderMode::Json => progress_json("import", &line, StdDuration::from_secs(1)),
            _ => unreachable!(),
        };

        assert_eq!(plain, "Indexed 2 sources");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["type"],
            "ctx_progress"
        );
        assert!(!plain.contains('\u{1b}'));
        assert!(!json.contains('\u{1b}'));
    }

    #[derive(Clone, Copy)]
    enum WriterFailure {
        Write,
        Flush,
    }

    struct FailingWriter(WriterFailure);

    impl Write for FailingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            match self.0 {
                WriterFailure::Write => Err(io::Error::other("injected progress write failure")),
                WriterFailure::Flush => Ok(buffer.len()),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            match self.0 {
                WriterFailure::Write => Ok(()),
                WriterFailure::Flush => Err(io::Error::other("injected progress flush failure")),
            }
        }
    }

    #[test]
    fn progress_write_and_flush_failures_remain_errors() {
        let line = ProgressLine {
            phase: "logical_scan".to_owned(),
            message: "Scanning SQLite history".to_owned(),
            completed_bytes: 0,
            total_bytes: 0,
            completed_files: None,
            total_files: None,
            imported_events: None,
            done: false,
            refresh: None,
        };
        for (failure, expected) in [
            (WriterFailure::Write, "injected progress write failure"),
            (WriterFailure::Flush, "injected progress flush failure"),
        ] {
            let mut writer = FailingWriter(failure);
            let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
                crate::ui::StreamKind::Stderr,
            ));
            let mut output = ProgressOutput::Direct(LiveOutput::new(&mut writer, context));
            let result = write_progress(
                &mut output,
                ProgressRenderMode::Json,
                "import",
                &line,
                StdDuration::ZERO,
            );
            assert!(result
                .expect_err("progress output failure must propagate")
                .to_string()
                .contains(expected));
        }
    }

    #[test]
    fn sqlite_logical_progress_is_typed_and_never_invents_a_total() {
        let snapshot = active_status();
        let line = source_refresh_line(snapshot, 8_192);
        assert_eq!(line.phase, "logical_scan");
        assert!(line.message.contains("history control.sqlite"));
        assert!(!line.message.contains('\n'));
        assert_eq!((line.completed_bytes, line.total_bytes), (0, 0));

        let value: serde_json::Value =
            serde_json::from_str(&progress_json("import", &line, StdDuration::from_secs(2)))
                .unwrap();
        assert_eq!(value["percent"], 0.0);
        assert_eq!(value["eta_seconds"], serde_json::Value::Null);
        assert_eq!(value["current_source_progress"]["stage"], "logical_scan");
        assert_eq!(
            value["current_source_progress"]["logical_rows_scanned"],
            4_096
        );
        assert!(!value["current_source"].as_str().unwrap().contains('\n'));
        assert_eq!(value["logical_phase"], "direct");
        assert_eq!(value["physical_attempt_id"], "physical-attempt");
    }

    #[test]
    fn progress_text_is_control_safe_utf8_and_bounded() {
        let text = format!("{}\n{}", "é".repeat(400), "x".repeat(400));
        let bounded = bounded_progress_text(&text, MAX_PROGRESS_MESSAGE_BYTES);
        assert!(bounded.len() <= MAX_PROGRESS_MESSAGE_BYTES);
        assert!(!bounded.contains('\n'));
        assert!(bounded.ends_with("..."));
    }
}
