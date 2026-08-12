use super::layout::{display_width, wrap_text};
use crate::ui::{glyph::Glyph, Document, Line, RenderContext, Span, Token};

const MAX_BAR_WIDTH: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress<'a> {
    pub label: &'a str,
    pub current: u64,
    pub total: Option<u64>,
    pub detail: Option<&'a str>,
}

pub fn progress(context: &RenderContext, progress: Progress<'_>) -> Document {
    let percentage = progress.total.filter(|total| *total > 0).map(|total| {
        let complete = u128::from(progress.current.min(total));
        let total = u128::from(total);
        format!("{}%", complete.saturating_mul(100) / total)
    });
    let mut document = Document::new();
    push_heading(&mut document, context, progress.label);
    push_bar(
        &mut document,
        context,
        progress.current,
        progress.total,
        percentage.as_deref(),
    );

    if let Some(detail) = progress.detail {
        for line in wrap_text(detail, context.content_width()) {
            document.push_line(Line::styled(line, Token::Label));
        }
    }
    document
}

fn push_heading(document: &mut Document, context: &RenderContext, label: &str) {
    for line in wrap_text(label, context.content_width()) {
        document.push_line(Line::styled(line, Token::Heading));
    }
}

fn push_bar(
    document: &mut Document,
    context: &RenderContext,
    current: u64,
    total: Option<u64>,
    percentage: Option<&str>,
) {
    let suffix_width = percentage.map_or(0, |value| display_width(value).saturating_add(2));
    let bar_width = context
        .content_width()
        .map_or(MAX_BAR_WIDTH, |width| {
            width.saturating_sub(suffix_width).min(MAX_BAR_WIDTH)
        })
        .max(1);
    let Some(total) = total.filter(|total| *total > 0) else {
        let pulse_width = bar_width.min(8);
        let travel = bar_width.saturating_sub(pulse_width);
        let position = usize::try_from(current).unwrap_or(usize::MAX) % travel.saturating_add(1);
        document.push_line(
            Line::new()
                .with(Span::new(
                    Glyph::Rule.render(context).repeat(position),
                    Token::Label,
                ))
                .with(Span::new(
                    Glyph::Progress.render(context).repeat(pulse_width),
                    Token::Accent,
                ))
                .with(Span::new(
                    Glyph::Rule
                        .render(context)
                        .repeat(bar_width.saturating_sub(position + pulse_width)),
                    Token::Label,
                )),
        );
        return;
    };

    let filled = (u128::from(current.min(total)).saturating_mul(bar_width as u128)
        / u128::from(total)) as usize;
    let remaining = bar_width.saturating_sub(filled);
    let mut line = Line::new()
        .with(Span::new(
            Glyph::Progress.render(context).repeat(filled),
            Token::Accent,
        ))
        .with(Span::new(
            Glyph::Rule.render(context).repeat(remaining),
            Token::Label,
        ));
    if let Some(percentage) = percentage {
        line.push(Span::text("  "));
        line.push(Span::new(percentage, Token::Accent));
    }
    document.push_line(line);
}
