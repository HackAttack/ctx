use std::{
    io::{self, IsTerminal as _, Write},
    sync::{Arc, Mutex},
};

use crate::output::MeasuredWriter;

use super::{ColorMode, Document, RenderContext, StreamKind};

type BoxedWriter = Box<dyn Write + Send>;

#[derive(Clone)]
struct SharedDestinationWriter(Arc<Mutex<BoxedWriter>>);

impl SharedDestinationWriter {
    fn new(writer: BoxedWriter) -> Self {
        Self(Arc::new(Mutex::new(writer)))
    }
}

impl Write for SharedDestinationWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .flush()
    }
}

pub struct Ui {
    stdout: Destination,
    stderr: Destination,
}

impl Ui {
    /// Probes stdout and stderr independently and owns adaptive writers for
    /// both destinations.
    pub fn stdio(color_mode: ColorMode) -> Self {
        let stdout = io::stdout();
        let stdout_terminal = stdout.is_terminal();
        let stdout_auto_color = auto_color_enabled(&stdout);
        let stdout_context = RenderContext::detected(
            StreamKind::Stdout,
            color_mode,
            stdout_terminal,
            stream_width(StreamKind::Stdout),
            supports_unicode::on(supports_unicode::Stream::Stdout),
            stdout_auto_color,
            term_is_dumb(),
        );
        let stdout_terminal_controls = stdio_terminal_controls(&stdout, stdout_context);
        let stderr = io::stderr();
        let stderr_terminal = stderr.is_terminal();
        let stderr_auto_color = auto_color_enabled(&stderr);
        let stderr_context = RenderContext::detected(
            StreamKind::Stderr,
            color_mode,
            stderr_terminal,
            stream_width(StreamKind::Stderr),
            supports_unicode::on(supports_unicode::Stream::Stderr),
            stderr_auto_color,
            term_is_dumb(),
        );
        let stderr_terminal_controls = stdio_terminal_controls(&stderr, stderr_context);
        Self {
            stdout: Destination::adapted(stdout_context, stdout, stdout_terminal_controls),
            stderr: Destination::adapted(stderr_context, stderr, stderr_terminal_controls),
        }
    }

    /// Constructs a UI with explicit capabilities and owned writers.
    pub fn with_writers<Out, Err>(
        stdout: Out,
        stdout_context: RenderContext,
        stderr: Err,
        stderr_context: RenderContext,
    ) -> Self
    where
        Out: Write + Send + 'static,
        Err: Write + Send + 'static,
    {
        Self {
            stdout: Destination::injected(stdout_context, stdout),
            stderr: Destination::injected(stderr_context, stderr),
        }
    }

    #[cfg(test)]
    fn with_writers_and_terminal_controls<Out, Err>(
        stdout: Out,
        stdout_context: RenderContext,
        stdout_terminal_controls: bool,
        stderr: Err,
        stderr_context: RenderContext,
        stderr_terminal_controls: bool,
    ) -> Self
    where
        Out: Write + Send + 'static,
        Err: Write + Send + 'static,
    {
        Self {
            stdout: Destination::injected_with_terminal_controls(
                stdout_context,
                stdout,
                stdout_terminal_controls,
            ),
            stderr: Destination::injected_with_terminal_controls(
                stderr_context,
                stderr,
                stderr_terminal_controls,
            ),
        }
    }

    pub fn context(&self, stream: StreamKind) -> &RenderContext {
        match stream {
            StreamKind::Stdout => self.stdout.context(),
            StreamKind::Stderr => self.stderr.context(),
        }
    }

    pub fn stdout_context(&self) -> &RenderContext {
        self.stdout.context()
    }

    pub fn stderr_context(&self) -> &RenderContext {
        self.stderr.context()
    }

    pub fn write(&mut self, stream: StreamKind, document: &Document) -> io::Result<()> {
        match stream {
            StreamKind::Stdout => self.stdout.write(document),
            StreamKind::Stderr => self.stderr.write(document),
        }
    }

    pub fn write_stdout(&mut self, document: &Document) -> io::Result<()> {
        self.stdout.write(document)
    }

    pub fn write_stderr(&mut self, document: &Document) -> io::Result<()> {
        self.stderr.write(document)
    }

    /// Writes an already-framed machine or plain-text protocol to the selected stdout stream.
    pub fn write_stdout_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdout.writer().write_all(bytes)
    }

    /// Writes an already-framed machine or plain-text protocol to the selected stderr stream.
    pub fn write_stderr_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.writer().write_all(bytes)
    }

    pub fn stdout_live_output(&mut self) -> LiveOutput<&mut (dyn Write + Send)> {
        let context = *self.stdout.context();
        LiveOutput::new(self.stdout.writer(), context)
    }

    pub fn stderr_live_output(&mut self) -> LiveOutput<&mut (dyn Write + Send)> {
        let context = *self.stderr.context();
        LiveOutput::new(self.stderr.writer(), context)
    }

    pub(crate) fn stderr_shared_live_output(&self) -> LiveOutput<BoxedWriter> {
        let context = *self.stderr.context();
        LiveOutput::new(Box::new(self.stderr.shared_writer()), context)
    }

    pub fn stdout_writer(&mut self) -> &mut (dyn Write + Send) {
        self.stdout.writer()
    }

    pub fn stderr_writer(&mut self) -> &mut (dyn Write + Send) {
        self.stderr.writer()
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.stderr.flush()
    }
}

/// Owns all cursor motion used to replace a rendered terminal frame. Dynamic
/// content is rendered separately and is never part of a control sequence.
pub struct LiveOutput<W: Write> {
    writer: W,
    context: RenderContext,
    rendered_rows: Vec<String>,
    repaint_cursor: Option<RepaintCursor>,
    cursor_hidden: bool,
}

#[derive(Debug, Clone, Copy)]
struct RepaintCursor {
    current_row: usize,
    recovery_row: usize,
}

impl<W: Write> LiveOutput<W> {
    pub fn new(writer: W, context: RenderContext) -> Self {
        Self {
            writer,
            context,
            rendered_rows: Vec::new(),
            repaint_cursor: None,
            cursor_hidden: false,
        }
    }

    pub const fn context(&self) -> &RenderContext {
        &self.context
    }

    #[doc(hidden)]
    pub fn into_inner(mut self) -> W {
        self.restore_cursor_best_effort();
        let output = std::mem::ManuallyDrop::new(self);
        // SAFETY: `output` will not be dropped, so reading its writer transfers
        // ownership exactly once without running `LiveOutput::drop` a second time.
        unsafe { std::ptr::read(&output.writer) }
    }

    #[doc(hidden)]
    pub const fn inner(&self) -> &W {
        &self.writer
    }

    pub fn write_document(&mut self, document: &Document) -> io::Result<()> {
        self.writer
            .write_all(document.render(&self.context).as_bytes())?;
        self.writer.flush()
    }

    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }

    pub fn write_frame(&mut self, document: &Document, final_frame: bool) -> io::Result<()> {
        let frame = document.render(&self.context);
        if !self.context.live_output_capable() {
            self.writer.write_all(frame.as_bytes())?;
            self.writer.write_all(b"\n")?;
            return self.writer.flush();
        }

        let rows = frame_rows(&frame);
        if !self.cursor_hidden {
            let write_result = if final_frame {
                self.writer.write_all(frame.as_bytes())
            } else {
                self.hide_cursor()
                    .and_then(|()| self.writer.write_all(frame.as_bytes()))
            };
            if !final_frame && write_result.is_ok() {
                self.rendered_rows = rows.iter().map(|row| (*row).to_owned()).collect();
            }
            return self.finish_frame(final_frame, write_result);
        }

        let repaint_result = self.repaint_changed_rows(&rows);
        if repaint_result.is_ok() {
            self.rendered_rows = rows.iter().map(|row| (*row).to_owned()).collect();
        }
        self.finish_frame(final_frame, repaint_result)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.cursor_hidden = true;
        self.writer.write_all(b"\x1b[?25l")
    }

    fn repaint_changed_rows(&mut self, rows: &[&str]) -> io::Result<()> {
        if self
            .rendered_rows
            .iter()
            .map(String::as_str)
            .eq(rows.iter().copied())
        {
            return Ok(());
        }

        let height = self.rendered_rows.len().max(rows.len());
        self.repaint_cursor = Some(RepaintCursor {
            current_row: self.rendered_rows.len(),
            recovery_row: height,
        });
        let Some(cursor) = self.repaint_cursor.as_mut() else {
            return Err(io::Error::other("missing repaint cursor"));
        };
        write_cursor_up(
            &mut self.writer,
            self.rendered_rows.len(),
            Some(&mut cursor.current_row),
        )?;
        for row in 0..height {
            if self.rendered_rows.get(row).map(String::as_str) == rows.get(row).copied() {
                continue;
            }
            let Some(cursor) = self.repaint_cursor.as_mut() else {
                return Err(io::Error::other("missing repaint cursor"));
            };
            write_cursor_down(
                &mut self.writer,
                row.saturating_sub(cursor.current_row),
                Some(&mut cursor.current_row),
            )?;
            self.writer.write_all(b"\r")?;
            if let Some(line) = rows.get(row) {
                self.writer.write_all(line.as_bytes())?;
            }
            self.writer.write_all(b"\x1b[K")?;
        }

        let Some(cursor) = self.repaint_cursor.as_mut() else {
            return Err(io::Error::other("missing repaint cursor"));
        };
        if cursor.current_row < rows.len() {
            write_cursor_down(
                &mut self.writer,
                rows.len() - cursor.current_row,
                Some(&mut cursor.current_row),
            )?;
        } else {
            write_cursor_up(
                &mut self.writer,
                cursor.current_row - rows.len(),
                Some(&mut cursor.current_row),
            )?;
        }
        self.writer.write_all(b"\r")?;
        self.repaint_cursor = None;
        Ok(())
    }

    fn finish_frame(&mut self, final_frame: bool, result: io::Result<()>) -> io::Result<()> {
        if let Err(error) = result {
            self.restore_repaint_anchor_best_effort();
            let _ = self.writer.write_all(b"\r");
            let _ = self.restore_cursor();
            self.rendered_rows.clear();
            let _ = self.writer.flush();
            return Err(error);
        }
        if final_frame {
            let restore_result = self.restore_cursor();
            self.rendered_rows.clear();
            return restore_result.and_then(|()| self.writer.flush());
        }
        self.writer.flush()
    }

    fn restore_repaint_anchor_best_effort(&mut self) {
        let Some(cursor) = self.repaint_cursor.take() else {
            return;
        };
        if cursor.current_row < cursor.recovery_row {
            let _ = write_cursor_down(
                &mut self.writer,
                cursor.recovery_row - cursor.current_row,
                None,
            );
        } else {
            let _ = write_cursor_up(
                &mut self.writer,
                cursor.current_row - cursor.recovery_row,
                None,
            );
        }
    }

    fn restore_cursor(&mut self) -> io::Result<()> {
        if self.cursor_hidden {
            self.writer.write_all(b"\x1b[?25h")?;
            self.cursor_hidden = false;
        }
        Ok(())
    }

    fn restore_cursor_best_effort(&mut self) {
        if self.cursor_hidden {
            let _ = self.writer.write_all(b"\x1b[?25h");
            let _ = self.writer.flush();
            self.cursor_hidden = false;
        }
    }
}

impl<W: Write> Drop for LiveOutput<W> {
    fn drop(&mut self) {
        self.restore_cursor_best_effort();
    }
}

fn frame_rows(frame: &str) -> Vec<&str> {
    if frame.is_empty() {
        Vec::new()
    } else {
        frame
            .strip_suffix('\n')
            .unwrap_or(frame)
            .split('\n')
            .collect()
    }
}

fn write_cursor_up(
    writer: &mut impl Write,
    rows: usize,
    mut current_row: Option<&mut usize>,
) -> io::Result<()> {
    for _ in 0..rows {
        writer.write_all(b"\x1b[A")?;
        if let Some(current_row) = current_row.as_deref_mut() {
            *current_row = current_row.saturating_sub(1);
        }
    }
    Ok(())
}

fn write_cursor_down(
    writer: &mut impl Write,
    rows: usize,
    mut current_row: Option<&mut usize>,
) -> io::Result<()> {
    for _ in 0..rows {
        writer.write_all(b"\x1b[B")?;
        if let Some(current_row) = current_row.as_deref_mut() {
            *current_row = current_row.saturating_add(1);
        }
    }
    Ok(())
}

struct Destination {
    context: RenderContext,
    writer: SharedDestinationWriter,
}

impl Destination {
    fn new(context: RenderContext, writer: BoxedWriter) -> Self {
        Self {
            context,
            writer: SharedDestinationWriter::new(writer),
        }
    }

    fn injected<W>(context: RenderContext, writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self::injected_with_terminal_controls(context, writer, context.live_output_capable())
    }

    fn injected_with_terminal_controls<W>(
        context: RenderContext,
        writer: W,
        terminal_controls: bool,
    ) -> Self
    where
        W: Write + Send + 'static,
    {
        let writer: BoxedWriter = Box::new(writer);
        Self::adapted(context, writer, terminal_controls)
    }

    /// Keeps platform terminal adaptation at the final shared writer boundary.
    /// Measurement remains outside the adapter so every caller follows the
    /// same stdout/stderr accounting path.
    fn adapted<W>(context: RenderContext, writer: W, terminal_controls: bool) -> Self
    where
        W: anstream::stream::RawStream + anstream::stream::AsLockedWrite + Send + 'static,
    {
        let context = context.with_terminal_control_support(terminal_controls);
        let adapted = terminal_adapter(writer, context);
        let measured: BoxedWriter = Box::new(MeasuredWriter::current(adapted, context.stream()));
        Self::new(context, measured)
    }

    const fn context(&self) -> &RenderContext {
        &self.context
    }

    fn write(&mut self, document: &Document) -> io::Result<()> {
        self.writer
            .write_all(document.render(&self.context).as_bytes())
    }

    fn writer(&mut self) -> &mut (dyn Write + Send) {
        &mut self.writer
    }

    fn shared_writer(&self) -> SharedDestinationWriter {
        self.writer.clone()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

fn terminal_adapter<W>(writer: W, context: RenderContext) -> anstream::AutoStream<W>
where
    W: anstream::stream::RawStream,
{
    anstream::AutoStream::new(writer, terminal_adapter_choice(context))
}

const fn terminal_adapter_choice(context: RenderContext) -> anstream::ColorChoice {
    if context.live_output_capable() {
        // The actual destination handle has already enabled VT processing.
        // Bypass anstream's combined stdout/stderr capability probe.
        anstream::ColorChoice::AlwaysAnsi
    } else if context.color_enabled() {
        // Keep anstream's Wincon styling fallback when VT is unavailable.
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    }
}

fn resolve_terminal_controls(
    context: RenderContext,
    enable_for_destination: impl FnOnce() -> bool,
) -> bool {
    context.live_output_capable() && enable_for_destination()
}

#[cfg(not(windows))]
fn stdio_terminal_controls<W>(_writer: &W, context: RenderContext) -> bool {
    resolve_terminal_controls(context, || true)
}

#[cfg(windows)]
fn stdio_terminal_controls<W>(writer: &W, context: RenderContext) -> bool
where
    W: std::os::windows::io::AsRawHandle,
{
    resolve_terminal_controls(context, || {
        enable_windows_terminal_controls(writer.as_raw_handle())
    })
}

#[cfg(windows)]
fn enable_windows_terminal_controls(handle: std::os::windows::io::RawHandle) -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    };

    let handle = handle as HANDLE;
    if handle.is_null() {
        return false;
    }

    let mut mode: CONSOLE_MODE = 0;
    unsafe {
        if GetConsoleMode(handle, &mut mode) == 0 {
            // `IsTerminal` also recognizes MSYS/Cygwin pseudo-terminals,
            // whose pipe handles do not expose console modes. Ordinary pipes
            // never reach this probe because the render context gates them.
            return std::env::var_os("TERM").is_some_and(|term| term != "dumb" && term != "cygwin");
        }
        if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
            return true;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

fn auto_color_enabled<S>(stream: &S) -> bool
where
    S: anstream::stream::RawStream,
{
    matches!(
        anstream::AutoStream::choice(stream),
        anstream::ColorChoice::Always | anstream::ColorChoice::AlwaysAnsi
    )
}

fn term_is_dumb() -> bool {
    std::env::var_os("TERM").is_some_and(|term| term == "dumb")
}

fn stream_width(stream: StreamKind) -> Option<usize> {
    #[cfg(any(unix, windows))]
    {
        let size = match stream {
            StreamKind::Stdout => terminal_size::terminal_size_of(io::stdout()),
            StreamKind::Stderr => terminal_size::terminal_size_of(io::stderr()),
        };
        size.map(|(terminal_size::Width(width), _)| usize::from(width))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = stream;
        None
    }
}

#[cfg(test)]
mod tests;
