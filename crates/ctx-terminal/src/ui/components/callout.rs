use super::layout::{display_width, is_copyable_atom, line_width, wrap_line};
use crate::ui::document::neutralize_controls;
use crate::ui::{glyph::Glyph, Document, Line, RenderContext, Span, Token};

const MAX_WIDTH: usize = 80;
const MIN_BORDERED_WIDTH: usize = 12;
const BORDER_OVERHEAD: usize = 4;

/// A bounded human-terminal callout with a semantic title and styled body.
///
/// Machine-readable formats must bypass terminal components and write their
/// selected representation directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Callout<'a> {
    pub title: &'a str,
    pub body: &'a Document,
}

/// Owned, product-neutral callout facts that can be rendered again when a live
/// destination changes width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalloutPresentation {
    title: String,
    rows: Vec<CalloutRow>,
}

impl CalloutPresentation {
    pub fn new(title: impl Into<String>, rows: Vec<CalloutRow>) -> Self {
        let title = title.into();
        Self {
            title: neutralize_controls(&title),
            rows: rows.into_iter().map(CalloutRow::neutralized).collect(),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn rows(&self) -> &[CalloutRow] {
        &self.rows
    }

    pub fn render(&self, context: &RenderContext) -> Document {
        let body = self.body(context);
        callout(
            context,
            Callout {
                title: &self.title,
                body: &body,
            },
        )
    }

    pub fn plain_message(&self, context: &RenderContext) -> String {
        let mut document = Document::from_line(Line::text(&self.title));
        document.append(self.body(context));
        document.render_plain().trim_end_matches('\n').to_owned()
    }

    fn body(&self, context: &RenderContext) -> Document {
        let mut body = Document::new();
        for row in &self.rows {
            body.push_line(row.render(context));
        }
        body
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalloutRow {
    Blank,
    Text(String),
    Bullet(String),
    Status { level: CalloutStatus, text: String },
    Action(String),
    Reference(String),
    Command(String),
}

impl CalloutRow {
    fn neutralized(self) -> Self {
        match self {
            Self::Blank => Self::Blank,
            Self::Text(text) => Self::Text(neutralize_controls(&text)),
            Self::Bullet(text) => Self::Bullet(neutralize_controls(&text)),
            Self::Status { level, text } => Self::Status {
                level,
                text: neutralize_controls(&text),
            },
            Self::Action(text) => Self::Action(neutralize_controls(&text)),
            Self::Reference(text) => Self::Reference(neutralize_controls(&text)),
            Self::Command(text) => Self::Command(neutralize_controls(&text)),
        }
    }

    fn render(&self, context: &RenderContext) -> Line {
        match self {
            Self::Blank => Line::new(),
            Self::Text(text) | Self::Action(text) => Line::text(text),
            Self::Bullet(text) => prefixed_line(
                Glyph::Bullet.render(context),
                Token::Accent,
                text,
                Token::Text,
            ),
            Self::Status { level, text } => {
                let (glyph, token) = match level {
                    CalloutStatus::Neutral => (Glyph::Bullet, Token::Accent),
                    CalloutStatus::Success => (Glyph::Success, Token::Success),
                    CalloutStatus::Warning => (Glyph::Warning, Token::Warning),
                    CalloutStatus::Failure => (Glyph::Failure, Token::Error),
                };
                prefixed_line(glyph.render(context), token, text, Token::Text)
            }
            Self::Reference(reference) => Line::styled(reference, Token::Reference),
            Self::Command(command) => Line::styled(command, Token::Command),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutStatus {
    Neutral,
    Success,
    Warning,
    Failure,
}

fn prefixed_line(prefix: &str, prefix_token: Token, text: &str, text_token: Token) -> Line {
    Line::new()
        .with(Span::new(prefix, prefix_token))
        .with(Span::text(" "))
        .with(Span::new(text, text_token))
}

pub fn callout(context: &RenderContext, callout: Callout<'_>) -> Document {
    let title = Line::styled(callout.title, Token::Heading);
    let available_width = context.content_width().unwrap_or(MAX_WIDTH).min(MAX_WIDTH);
    let natural_width = std::iter::once(&title)
        .chain(callout.body.lines())
        .map(line_width)
        .max()
        .unwrap_or_default()
        .saturating_add(BORDER_OVERHEAD)
        .max(MIN_BORDERED_WIDTH);
    let outer_width = natural_width.min(available_width);

    if outer_width < MIN_BORDERED_WIDTH {
        return borderless(context, &title, callout.body);
    }
    let inner_width = outer_width.saturating_sub(BORDER_OVERHEAD);
    if std::iter::once(&title)
        .chain(callout.body.lines())
        .any(|line| has_oversized_copyable_atom(line, inner_width))
    {
        return borderless(context, &title, callout.body);
    }

    let mut document = Document::from_line(horizontal_border(
        context,
        Glyph::BoxTopLeft,
        Glyph::BoxTopRight,
        outer_width,
    ));
    for line in std::iter::once(&title).chain(callout.body.lines()) {
        for line in wrap_line(line, Some(inner_width)) {
            document.push_line(content_line(context, line, inner_width));
        }
    }
    document.push_line(horizontal_border(
        context,
        Glyph::BoxBottomLeft,
        Glyph::BoxBottomRight,
        outer_width,
    ));
    document
}

fn borderless(context: &RenderContext, title: &Line, body: &Document) -> Document {
    let mut document = Document::new();
    for line in std::iter::once(title).chain(body.lines()) {
        for line in wrap_line(line, context.content_width()) {
            document.push_line(line);
        }
    }
    document
}

fn horizontal_border(context: &RenderContext, left: Glyph, right: Glyph, width: usize) -> Line {
    Line::styled(
        format!(
            "{}{}{}",
            left.render(context),
            Glyph::Rule.render(context).repeat(width.saturating_sub(2)),
            right.render(context)
        ),
        Token::Label,
    )
}

fn content_line(context: &RenderContext, content: Line, width: usize) -> Line {
    let padding = width.saturating_sub(line_width(&content));
    let mut line = Line::new()
        .with(Span::new(Glyph::BoxVertical.render(context), Token::Label))
        .with(Span::text(" "));
    for span in content.spans() {
        line.push(span.clone());
    }
    line.push(Span::text(" ".repeat(padding.saturating_add(1))));
    line.push(Span::new(Glyph::BoxVertical.render(context), Token::Label));
    line
}

fn has_oversized_copyable_atom(line: &Line, width: usize) -> bool {
    if line_width(line) > width
        && line
            .spans()
            .iter()
            .any(|span| matches!(span.token(), Token::Command | Token::Reference))
    {
        return true;
    }
    let text: String = line.spans().iter().map(Span::content).collect();
    text.split_whitespace()
        .any(|word| is_copyable_atom(word) && display_width(word) > width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width))
    }

    fn body() -> Document {
        Document::from_line(
            Line::new()
                .with(Span::text("Local history remains "))
                .with(Span::new("on this device", Token::Accent))
                .with(Span::text(
                    " while ctx prepares the result. Additional neutral details make this body \
                     wide enough to exercise the component cap without changing its meaning.",
                )),
        )
    }

    #[test]
    fn wraps_at_standard_widths_and_caps_wide_callouts() {
        for width in [32, 48, 80, 120] {
            let context = context(width);
            let rendered = callout(
                &context,
                Callout {
                    title: "History stays local",
                    body: &body(),
                },
            );
            let plain = rendered.render_plain();
            assert!(plain.starts_with('╭'), "width {width}: {plain}");
            assert!(plain.trim_end().ends_with('╯'), "width {width}: {plain}");
            assert!(plain.contains("History stays local"), "width {width}");
            assert!(plain.contains("Local history remains"), "width {width}");
            for word in ["on", "this", "device"] {
                assert!(
                    plain.split_whitespace().any(|candidate| candidate == word),
                    "width {width}: {plain}"
                );
            }
            assert!(
                rendered
                    .lines()
                    .iter()
                    .all(|line| line_width(line) <= width.saturating_sub(1).min(MAX_WIDTH)),
                "width {width}: {plain}"
            );
            if width == 120 {
                assert_eq!(line_width(&rendered.lines()[0]), MAX_WIDTH, "{plain}");
            }
        }
    }

    #[test]
    fn uses_ascii_fallback_and_dim_semantic_borders() {
        let context =
            RenderContext::for_test(TestContext::tty(StreamKind::Stdout, 48).unicode(false));
        let document = callout(
            &context,
            Callout {
                title: "A note",
                body: &Document::from_line(Line::text("Complete static text.")),
            },
        );

        let plain = document.render_plain();
        assert!(plain.starts_with('+'), "{plain}");
        assert!(plain.contains("+\n| A note"), "{plain}");
        assert!(plain.trim_end().ends_with('+'), "{plain}");
        assert!(
            ['╭', '╮', '╰', '╯', '│']
                .into_iter()
                .all(|glyph| !plain.contains(glyph)),
            "{plain}"
        );
        assert_eq!(document.lines()[0].spans()[0].token(), Token::Label);
        assert_eq!(document.lines()[1].spans()[2].token(), Token::Heading);
    }

    #[test]
    fn falls_back_without_losing_narrow_or_copyable_content() {
        let url = "https://example.test/a/long/opaque/copyable/value";
        let body = Document::from_line(
            Line::new()
                .with(Span::text("Open "))
                .with(Span::new(url, Token::Reference)),
        );
        for context in [context(8), context(32)] {
            let document = callout(
                &context,
                Callout {
                    title: "Next step",
                    body: &body,
                },
            );
            let plain = document.render_plain();
            assert!(!plain.contains('│'), "{plain}");
            assert!(!plain.contains('|'), "{plain}");
            assert_eq!(plain.matches(url).count(), 1, "{plain}");
            let url_span = document
                .lines()
                .iter()
                .flat_map(Line::spans)
                .find(|span| span.content() == url)
                .expect("URL span");
            assert_eq!(url_span.token(), Token::Reference);
        }
    }

    #[test]
    fn semantic_commands_and_references_force_a_complete_borderless_fallback() {
        for token in [Token::Command, Token::Reference] {
            let atom = match token {
                Token::Command => "ctx search an intentionally long multi word command",
                Token::Reference => "0123456789abcdef0123456789abcdef01234567",
                _ => unreachable!(),
            };
            let body = Document::from_line(Line::styled(atom, token));
            let document = callout(
                &context(32),
                Callout {
                    title: "Copy this value",
                    body: &body,
                },
            );
            let plain = document.render_plain();

            assert!(!plain.contains('│'), "{plain}");
            assert_eq!(plain.matches(atom).count(), 1, "{plain}");
            let span = document
                .lines()
                .iter()
                .flat_map(Line::spans)
                .find(|span| span.content() == atom)
                .expect("semantic atom span");
            assert_eq!(span.token(), token);
        }
    }

    #[test]
    fn dynamic_values_remain_control_safe() {
        let attack = "\u{1b}[31mowned\u{1b}[0m\rrewrite\u{0000}\u{0085}\u{009b}2J\nnext\tcell";
        let body = Document::from_line(Line::styled(attack, Token::Accent));
        let plain = callout(
            &context(120),
            Callout {
                title: attack,
                body: &body,
            },
        )
        .render_plain();

        for control in ['\u{1b}', '\r', '\u{0000}', '\u{0085}', '\u{009b}'] {
            assert!(!plain.contains(control), "{plain:?}");
        }
        assert_eq!(plain.matches("\\x1b[31mowned\\x1b[0m").count(), 2);
        assert_eq!(plain.matches("\\u{009b}2J").count(), 2);
        assert_eq!(plain.matches("\\nnext\\tcell").count(), 2);
    }

    #[test]
    fn redirected_and_ansi_output_remain_complete_and_equivalent() {
        let context =
            RenderContext::for_test(TestContext::pipe(StreamKind::Stderr).color(ColorMode::Always));
        let body = body();
        let document = callout(
            &context,
            Callout {
                title: "History stays local",
                body: &body,
            },
        );
        let plain = document.render_plain();
        let styled = document.render(&context);

        assert!(plain.contains("while ctx prepares the result."));
        assert_eq!(anstream::adapter::strip_str(&styled).to_string(), plain);
        assert!(document.lines().iter().any(|line| line
            .spans()
            .iter()
            .any(|span| span.token() == Token::Accent)));
    }
}
