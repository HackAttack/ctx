use super::*;
use crate::ui::{Line, Span, TestContext, Token};
use std::{
    cell::Cell,
    sync::{Arc, Mutex},
};

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

#[derive(Clone)]
struct FailOnceWriter(Arc<Mutex<FailOnceState>>);

struct FailOnceState {
    bytes: Vec<u8>,
    failed: bool,
}

impl FailOnceWriter {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(FailOnceState {
            bytes: Vec::new(),
            failed: false,
        })))
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().bytes.clone()).unwrap()
    }
}

impl Write for FailOnceWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if !state.failed && buffer == b"\x1b[K" {
            state.failed = true;
            return Err(io::Error::other("injected repaint failure"));
        }
        state.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn document(lines: &[&str]) -> Document {
    let mut document = Document::new();
    for line in lines {
        document.push_line(Line::new().with(Span::new(*line, Token::Heading)));
    }
    document
}

fn comparison_documents() -> Vec<Document> {
    (0..=40)
        .map(|position| {
            let bar = format!(
                "{}{}{}",
                "─".repeat(position),
                "━".repeat(8),
                "─".repeat(40 - position)
            );
            let mut document = Document::new();
            for line in [
                "Reading your agent history",
                &bar,
                "",
                "Agent histories  Codex",
                "                 Claude",
                "",
                "Sessions         1,123",
                "Messages         72,456",
                "Tool calls       31,009",
                "Data scanned     8.20 GiB",
                "Elapsed          2m 05s",
                "Remaining        estimating",
            ] {
                document.push_line(Line::text(line));
            }
            document
        })
        .collect()
}

fn differential_comparison_bytes(frames: usize) -> usize {
    let context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
    );
    let documents = comparison_documents();
    let mut output = LiveOutput::new(Vec::new(), context);
    for frame in 0..frames {
        output
            .write_frame(&documents[frame % documents.len()], false)
            .unwrap();
    }
    output.into_inner().len()
}

fn full_repaint_comparison_bytes(frames: usize) -> usize {
    let context = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
    );
    let documents = comparison_documents();
    let mut output = Vec::new();
    let mut rendered_lines = 0;
    for frame in 0..frames {
        let rendered = documents[frame % documents.len()].render(&context);
        let lines = frame_rows(&rendered);
        if rendered_lines == 0 {
            output.extend_from_slice(rendered.as_bytes());
            rendered_lines = lines.len();
            continue;
        }

        output.extend_from_slice(format!("\x1b[{rendered_lines}A").as_bytes());
        let height = rendered_lines.max(lines.len());
        for row in 0..height {
            output.extend_from_slice(b"\r\x1b[2K");
            if let Some(line) = lines.get(row) {
                output.extend_from_slice(line.as_bytes());
            }
            output.push(b'\n');
        }
        if rendered_lines > lines.len() {
            output.extend_from_slice(
                format!("\x1b[{}A", rendered_lines - lines.len()).as_bytes(),
            );
        }
        rendered_lines = lines.len();
    }
    output.len()
}

#[test]
fn differential_renderer_has_deterministic_write_reduction() {
    let differential = differential_comparison_bytes(1_000);
    let full_repaint = full_repaint_comparison_bytes(1_000);

    assert_eq!((differential, full_repaint), (221_161, 434_935));
    assert!(differential * 5 < full_repaint * 3);
}

#[test]
#[ignore = "bounded renderer CPU comparison; run explicitly through ctx-build-governor"]
fn benchmark_differential_renderer() {
    std::hint::black_box(differential_comparison_bytes(50_000));
}

#[test]
#[ignore = "bounded renderer CPU comparison; run explicitly through ctx-build-governor"]
fn benchmark_full_repaint_renderer() {
    std::hint::black_box(full_repaint_comparison_bytes(50_000));
}

#[test]
fn live_controller_bytes_cover_first_grow_shrink_and_final_frames() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_frame(&document(&["one"]), false).unwrap();
    output
        .write_frame(&document(&["one", "two"]), false)
        .unwrap();
    output.write_frame(&document(&["short"]), false).unwrap();
    output.write_frame(&document(&["done"]), true).unwrap();
    output.write_frame(&document(&["after"]), false).unwrap();

    let rendered = String::from_utf8(output.into_inner()).unwrap();
    assert_eq!(
        rendered,
        concat!(
            "\x1b[?25lone\n",
            "\x1b[A\x1b[B\rtwo\x1b[K\x1b[B\r",
            "\x1b[A\x1b[A\rshort\x1b[K\x1b[B\r\x1b[K\r",
            "\x1b[A\rdone\x1b[K\x1b[B\r\x1b[?25h",
            "\x1b[?25lafter\n\x1b[?25h",
        )
    );
}

#[test]
fn unchanged_live_frames_emit_no_bytes() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output
        .write_frame(&document(&["first", "second"]), false)
        .unwrap();
    let first_frame = output.inner().clone();

    output
        .write_frame(&document(&["first", "second"]), false)
        .unwrap();

    assert_eq!(output.inner(), &first_frame);
}

#[test]
fn sparse_live_row_changes_repaint_only_the_changed_row() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output
        .write_frame(&document(&["first", "second", "third"]), false)
        .unwrap();
    output
        .write_frame(&document(&["first", "SECOND", "third"]), false)
        .unwrap();

    assert_eq!(
        output.inner(),
        concat!(
            "\x1b[?25lfirst\nsecond\nthird\n",
            "\x1b[A\x1b[A\x1b[A\x1b[B\rSECOND\x1b[K\x1b[B\x1b[B\r",
        )
        .as_bytes()
    );
}

#[test]
fn shorter_live_row_clears_only_its_stale_suffix() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_frame(&document(&["long row"]), false).unwrap();
    output.write_frame(&document(&["short"]), false).unwrap();

    let rendered = String::from_utf8(output.into_inner()).unwrap();
    assert_eq!(
        rendered,
        concat!(
            "\x1b[?25llong row\n",
            "\x1b[A\rshort\x1b[K\x1b[B\r\x1b[?25h",
        )
    );
    assert!(!rendered.contains("\x1b[2K"));
}

#[test]
fn live_height_changes_clear_removed_rows_and_reanchor_cursor() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_frame(&document(&["one"]), false).unwrap();
    output
        .write_frame(&document(&["one", "two", "three"]), false)
        .unwrap();
    output.write_frame(&document(&["one"]), false).unwrap();

    assert_eq!(
        output.inner(),
        concat!(
            "\x1b[?25lone\n",
            "\x1b[A\x1b[B\rtwo\x1b[K\x1b[B\rthree\x1b[K\x1b[B\r",
            "\x1b[A\x1b[A\x1b[A\x1b[B\r\x1b[K\x1b[B\r\x1b[K\x1b[A\r",
        )
        .as_bytes()
    );
}

#[test]
fn final_live_frame_restores_cursor_and_resets_the_lifecycle() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_frame(&document(&["working"]), false).unwrap();
    output.write_frame(&document(&["done"]), true).unwrap();
    output.write_frame(&document(&["next"]), false).unwrap();

    let rendered = String::from_utf8(output.into_inner()).unwrap();
    assert_eq!(
        rendered,
        concat!(
            "\x1b[?25lworking\n",
            "\x1b[A\rdone\x1b[K\x1b[B\r\x1b[?25h",
            "\x1b[?25lnext\n\x1b[?25h",
        )
    );
}

#[test]
fn completed_live_output_leaves_following_output_at_column_zero() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_frame(&document(&["working"]), false).unwrap();
    output.write_frame(&document(&["done"]), true).unwrap();
    output.write_document(&document(&["SENTINEL"])).unwrap();

    let rendered = String::from_utf8(output.into_inner()).unwrap();
    assert!(
        rendered.ends_with("\x1b[B\r\x1b[?25hSENTINEL\n"),
        "{rendered:?}"
    );
}

#[test]
fn dropping_an_active_live_output_restores_the_cursor() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let writer = SharedWriter::default();
    let capture = writer.clone();
    {
        let mut output = LiveOutput::new(writer, context);
        output.write_frame(&document(&["working"]), false).unwrap();
    }

    assert_eq!(capture.text(), "\x1b[?25lworking\n\x1b[?25h");
}

#[test]
fn dropping_repainted_output_leaves_following_output_at_column_zero() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let writer = SharedWriter::default();
    let capture = writer.clone();
    {
        let mut output = LiveOutput::new(writer, context);
        output.write_frame(&document(&["working"]), false).unwrap();
        output.write_frame(&document(&["done"]), false).unwrap();
    }
    let mut sentinel = capture.clone();
    sentinel.write_all(b"SENTINEL").unwrap();

    let rendered = capture.text();
    assert!(
        rendered.ends_with("\x1b[B\r\x1b[?25hSENTINEL"),
        "{rendered:?}"
    );
}

#[test]
fn repaint_error_restores_cursor_below_live_block_before_following_output() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let writer = FailOnceWriter::new();
    let capture = writer.clone();
    {
        let mut output = LiveOutput::new(writer, context);
        output.write_frame(&document(&["working"]), false).unwrap();
        let error = output
            .write_frame(&document(&["broken"]), false)
            .expect_err("repaint failure must propagate");
        assert_eq!(error.to_string(), "injected repaint failure");
    }
    let mut sentinel = capture.clone();
    sentinel.write_all(b"SENTINEL").unwrap();

    let rendered = capture.text();
    assert!(
        rendered.ends_with("\x1b[A\rbroken\x1b[B\r\x1b[?25hSENTINEL"),
        "{rendered:?}"
    );
    assert!(!rendered.ends_with("\x1b[A\rbroken\r\x1b[?25hSENTINEL"));
}

#[test]
fn ui_adapter_preserves_live_controls_when_tty_styling_is_disabled() {
    let contexts = [
        TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
        TestContext::tty(StreamKind::Stdout, 80)
            .color(ColorMode::Auto)
            .no_color(true),
        TestContext::tty(StreamKind::Stdout, 80)
            .color(ColorMode::Auto)
            .auto_color(false),
    ];
    for test_context in contexts {
        let stdout = SharedWriter::default();
        let capture = stdout.clone();
        let mut ui = Ui::with_writers(
            stdout,
            RenderContext::for_test(test_context),
            Vec::new(),
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
        );
        let mut output = ui.stdout_live_output();
        output
            .write_frame(&document(&["long first row", "stale second row"]), false)
            .unwrap();
        output
            .write_frame(&document(&["short replacement"]), false)
            .unwrap();
        output.write_frame(&document(&["done"]), true).unwrap();

        let rendered = capture.text();
        assert_eq!(
            rendered,
            concat!(
                "\x1b[?25llong first row\nstale second row\n",
                "\x1b[A\x1b[A\rshort replacement\x1b[K\x1b[B\r\x1b[K\r",
                "\x1b[A\rdone\x1b[K\x1b[B\r\x1b[?25h",
            )
        );
        assert!(
            !rendered.contains("\x1b[1m"),
            "styling must remain disabled"
        );
    }
}

#[test]
fn terminal_adapter_capability_is_independent_of_styling() {
    let live_without_style = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
    );
    assert!(!live_without_style.color_enabled());
    assert!(live_without_style.live_output_capable());
    assert_eq!(
        terminal_adapter_choice(live_without_style),
        anstream::ColorChoice::AlwaysAnsi
    );
    let adapted = terminal_adapter(Vec::new(), live_without_style);
    assert_eq!(adapted.current_choice(), anstream::ColorChoice::AlwaysAnsi);
    let destination = Destination::adapted(live_without_style, Vec::new(), true);
    assert!(destination.context().live_output_capable());

    let styled_pipe =
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always));
    assert!(!styled_pipe.live_output_capable());
    assert_eq!(
        terminal_adapter_choice(styled_pipe),
        anstream::ColorChoice::Always
    );

    for unadapted in [
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        RenderContext::for_test(
            TestContext::tty(StreamKind::Stdout, 80)
                .term_dumb(true)
                .color(ColorMode::Never),
        ),
    ] {
        assert!(!unadapted.live_output_capable());
        assert!(!unadapted.color_enabled());
        assert_eq!(
            terminal_adapter_choice(unadapted),
            anstream::ColorChoice::Never
        );
        let adapted = terminal_adapter(Vec::new(), unadapted);
        assert_eq!(adapted.current_choice(), anstream::ColorChoice::Never);
    }
}

#[test]
fn per_destination_terminal_control_resolution_gates_live_output() {
    let live = RenderContext::for_test(
        TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
    );
    assert!(resolve_terminal_controls(live, || true));
    assert!(!resolve_terminal_controls(live, || false));

    let pipe =
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always));
    assert!(pipe.color_enabled());
    let probed = Cell::new(false);
    assert!(!resolve_terminal_controls(pipe, || {
        probed.set(true);
        true
    }));
    assert!(!probed.get(), "redirected streams must not be probed");

    let unsupported = live.with_terminal_control_support(false);
    assert!(!unsupported.live_output_capable());
    assert_eq!(
        terminal_adapter_choice(unsupported),
        anstream::ColorChoice::Never
    );

    let styled_unsupported = RenderContext::for_test(
        TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Always),
    )
    .with_terminal_control_support(false);
    assert!(!styled_unsupported.live_output_capable());
    assert!(styled_unsupported.color_enabled());
    assert_eq!(
        terminal_adapter_choice(styled_unsupported),
        anstream::ColorChoice::Always
    );
}

#[test]
fn split_stdout_stderr_terminal_controls_are_independent() {
    let cases = [
        (
            RenderContext::for_test(
                TestContext::tty(StreamKind::Stdout, 80).color(ColorMode::Never),
            ),
            true,
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr)),
            false,
        ),
        (
            RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
            false,
            RenderContext::for_test(
                TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Never),
            ),
            true,
        ),
    ];

    for (stdout_context, stdout_controls, stderr_context, stderr_controls) in cases {
        let stdout = SharedWriter::default();
        let stdout_capture = stdout.clone();
        let stderr = SharedWriter::default();
        let stderr_capture = stderr.clone();
        let mut ui = Ui::with_writers_and_terminal_controls(
            stdout,
            stdout_context,
            stdout_controls,
            stderr,
            stderr_context,
            stderr_controls,
        );

        assert_eq!(ui.stdout_context().live_output_capable(), stdout_controls);
        assert_eq!(ui.stderr_context().live_output_capable(), stderr_controls);

        {
            let mut output = ui.stdout_live_output();
            output
                .write_frame(&document(&["stdout first", "stdout stale"]), false)
                .unwrap();
            output
                .write_frame(&document(&["stdout replacement"]), true)
                .unwrap();
        }
        {
            let mut output = ui.stderr_live_output();
            output
                .write_frame(&document(&["stderr first", "stderr stale"]), false)
                .unwrap();
            output
                .write_frame(&document(&["stderr replacement"]), true)
                .unwrap();
        }

        let stdout = stdout_capture.text();
        let stderr = stderr_capture.text();
        assert_eq!(stdout.contains("\x1b[A"), stdout_controls);
        assert_eq!(stdout.contains("\x1b[?25l"), stdout_controls);
        assert_eq!(stdout.contains("\x1b[?25h"), stdout_controls);
        assert_eq!(stderr.contains("\x1b[A"), stderr_controls);
        assert_eq!(stderr.contains("\x1b[?25l"), stderr_controls);
        assert_eq!(stderr.contains("\x1b[?25h"), stderr_controls);
        assert!(!stdout.contains("\x1b[1m"));
        assert!(!stderr.contains("\x1b[1m"));
    }
}

#[test]
fn append_controller_writes_documents_and_lines_exactly() {
    let context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_document(&document(&["plain"])).unwrap();
    output.write_line(r#"{"type":"ctx_progress"}"#).unwrap();
    assert_eq!(
        String::from_utf8(output.into_inner()).unwrap(),
        "plain\n{\"type\":\"ctx_progress\"}\n"
    );
}

#[test]
fn pipe_and_term_dumb_append_without_cursor_motion() {
    for context in [
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout)),
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80).term_dumb(true)),
    ] {
        let mut output = LiveOutput::new(Vec::new(), context);
        output.write_frame(&document(&["one"]), false).unwrap();
        output.write_frame(&document(&["two"]), false).unwrap();
        let rendered = String::from_utf8(output.into_inner()).unwrap();
        assert_eq!(rendered, "one\n\ntwo\n\n");
        assert!(!rendered.contains('\u{1b}'));
    }
}

#[test]
fn forced_color_on_a_pipe_never_enables_cursor_motion() {
    let context =
        RenderContext::for_test(TestContext::pipe(StreamKind::Stdout).color(ColorMode::Always));
    assert!(context.color_enabled());
    assert!(!context.live_output_capable());
    let mut output = LiveOutput::new(Vec::new(), context);
    output.write_frame(&document(&["one"]), false).unwrap();
    output.write_frame(&document(&["two"]), false).unwrap();
    let rendered = String::from_utf8(output.into_inner()).unwrap();
    assert!(rendered.contains("\x1b[1m"));
    assert!(!rendered.contains("\x1b[1A"));
    assert!(!rendered.contains("\x1b[2K"));
}

#[test]
fn dynamic_text_is_neutralized_before_live_control_bytes() {
    let context = RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 80));
    let mut output = LiveOutput::new(Vec::new(), context);
    output
        .write_frame(&document(&["source\x1b[999A\rname"]), false)
        .unwrap();
    let rendered = String::from_utf8(output.into_inner()).unwrap();
    assert_eq!(rendered, "\x1b[?25lsource\\x1b[999A\\rname\n\x1b[?25h");
}
