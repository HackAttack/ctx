use crate::ui::{Line, Span, Token};
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr;

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(crate::ui::document::neutralize_controls(text).as_str())
}

pub(super) fn pad(width: usize) -> String {
    " ".repeat(width)
}

pub(super) fn pad_after(text: &str, target_width: usize) -> String {
    pad(target_width.saturating_sub(display_width(text)))
}

pub(super) fn wrap_text(text: &str, width: Option<usize>) -> Vec<String> {
    let text = crate::ui::document::neutralize_controls(text);
    let Some(width) = width else {
        return vec![text];
    };
    let width = width.max(1);
    let mut wrapped = Vec::new();

    wrap_logical_line(&text, width, &mut wrapped);

    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

pub(super) fn line_width(line: &Line) -> usize {
    line.spans()
        .iter()
        .map(|span| display_width(span.content()))
        .sum()
}

pub(super) fn wrap_line(line: &Line, width: Option<usize>) -> Vec<Line> {
    let Some(width) = width else {
        return vec![line.clone()];
    };
    let width = width.max(1);
    let chunks = styled_chunks(line);
    let mut output = Vec::new();
    let mut current = StyledLine::default();
    let mut pending_whitespace = None;

    for chunk in chunks {
        if chunk.whitespace {
            pending_whitespace = Some(chunk);
            continue;
        }

        let separator_width = pending_whitespace
            .as_ref()
            .map_or(0, |whitespace| whitespace.width);
        if current.has_text
            && current
                .width
                .saturating_add(separator_width)
                .saturating_add(chunk.width)
                > width
        {
            output.push(std::mem::take(&mut current).into_line());
            if let Some(whitespace) = pending_whitespace.as_mut() {
                whitespace.consume_wrap_separator();
            }
        }
        if let Some(whitespace) = pending_whitespace.take() {
            push_wrappable(whitespace.fragments, width, &mut current, &mut output);
        }
        if chunk.unbreakable {
            current.extend(chunk.fragments);
        } else {
            push_wrappable(chunk.fragments, width, &mut current, &mut output);
        }
    }
    if let Some(whitespace) = pending_whitespace {
        push_wrappable(whitespace.fragments, width, &mut current, &mut output);
    }

    if !current.is_empty() {
        output.push(current.into_line());
    }
    if output.is_empty() {
        output.push(Line::new());
    }
    output
}

fn push_wrappable(
    fragments: Vec<StyledFragment>,
    width: usize,
    current: &mut StyledLine,
    output: &mut Vec<Line>,
) {
    for fragment in fragments {
        for grapheme in fragment.text.graphemes(true) {
            let grapheme_width = display_width(grapheme);
            if !current.is_empty() && current.width.saturating_add(grapheme_width) > width {
                output.push(std::mem::take(current).into_line());
            }
            current.push(grapheme, fragment.token);
        }
    }
}

#[derive(Debug)]
struct StyledChunk {
    fragments: Vec<StyledFragment>,
    width: usize,
    whitespace: bool,
    unbreakable: bool,
}

impl StyledChunk {
    fn text(&self) -> String {
        self.fragments
            .iter()
            .map(|fragment| fragment.text.as_str())
            .collect()
    }

    fn consume_wrap_separator(&mut self) {
        let Some(fragment) = self.fragments.first_mut() else {
            return;
        };
        let Some(separator) = fragment.text.graphemes(true).next() else {
            return;
        };
        self.width = self.width.saturating_sub(display_width(separator));
        fragment.text.drain(..separator.len());
        if fragment.text.is_empty() {
            self.fragments.remove(0);
        }
    }
}

#[derive(Debug)]
struct StyledFragment {
    text: String,
    token: Token,
}

#[derive(Debug, Default)]
struct StyledLine {
    fragments: Vec<StyledFragment>,
    width: usize,
    has_text: bool,
}

impl StyledLine {
    fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    fn push(&mut self, text: &str, token: Token) {
        self.width = self.width.saturating_add(display_width(text));
        self.has_text |= !text.chars().all(char::is_whitespace);
        if let Some(fragment) = self.fragments.last_mut().filter(|item| item.token == token) {
            fragment.text.push_str(text);
        } else {
            self.fragments.push(StyledFragment {
                text: text.to_owned(),
                token,
            });
        }
    }

    fn extend(&mut self, fragments: Vec<StyledFragment>) {
        for fragment in fragments {
            self.push(&fragment.text, fragment.token);
        }
    }

    fn into_line(self) -> Line {
        let mut line = Line::new();
        for fragment in self.fragments {
            line.push(Span::new(fragment.text, fragment.token));
        }
        line
    }
}

fn styled_chunks(line: &Line) -> Vec<StyledChunk> {
    let mut chunks = Vec::new();
    let mut atom = StyledLine::default();
    let mut whitespace = StyledLine::default();
    for span in line.spans() {
        if matches!(span.token(), Token::Command | Token::Reference) {
            flush_chunk(&mut atom, false, &mut chunks);
            flush_chunk(&mut whitespace, true, &mut chunks);
            let mut semantic = StyledLine::default();
            semantic.push(span.content(), span.token());
            chunks.push(StyledChunk {
                fragments: semantic.fragments,
                width: semantic.width,
                whitespace: false,
                unbreakable: true,
            });
            continue;
        }
        for grapheme in span.content().graphemes(true) {
            if grapheme.chars().all(char::is_whitespace) {
                flush_chunk(&mut atom, false, &mut chunks);
                whitespace.push(grapheme, span.token());
            } else {
                flush_chunk(&mut whitespace, true, &mut chunks);
                atom.push(grapheme, span.token());
            }
        }
    }
    flush_chunk(&mut atom, false, &mut chunks);
    flush_chunk(&mut whitespace, true, &mut chunks);
    for chunk in &mut chunks {
        if !chunk.whitespace {
            chunk.unbreakable |= is_copyable_atom(&chunk.text());
        }
    }
    chunks
}

fn flush_chunk(line: &mut StyledLine, whitespace: bool, chunks: &mut Vec<StyledChunk>) {
    if line.is_empty() {
        return;
    }
    chunks.push(StyledChunk {
        fragments: std::mem::take(&mut line.fragments),
        width: std::mem::take(&mut line.width),
        whitespace,
        unbreakable: false,
    });
    line.has_text = false;
}

fn wrap_logical_line(line: &str, width: usize, output: &mut Vec<String>) {
    let mut current = String::new();
    for word in line.split_whitespace() {
        if current.is_empty() {
            push_word(word, width, &mut current, output);
            continue;
        }

        let joined_width = display_width(&current)
            .saturating_add(1)
            .saturating_add(display_width(word));
        if joined_width <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            output.push(std::mem::take(&mut current));
            push_word(word, width, &mut current, output);
        }
    }

    if !current.is_empty() || line.trim().is_empty() {
        output.push(current);
    }
}

fn push_word(word: &str, width: usize, current: &mut String, output: &mut Vec<String>) {
    if is_copyable_atom(word) {
        current.push_str(word);
        return;
    }
    for grapheme in word.graphemes(true) {
        let grapheme_width = display_width(grapheme);
        let current_width = display_width(current);
        if !current.is_empty() && current_width.saturating_add(grapheme_width) > width {
            output.push(std::mem::take(current));
        }
        current.push_str(grapheme);
    }
}

pub fn is_copyable_atom(word: &str) -> bool {
    word.contains("://") || word.starts_with('-') || word.matches('-').count() >= 4
}

#[cfg(test)]
mod tests {
    use super::{wrap_line, wrap_text};
    use crate::ui::{Line, Span, Token};

    #[test]
    fn wrapping_preserves_graphemes_and_copyable_identifiers() {
        let family = "👨‍👩‍👧‍👦👨‍👩‍👧‍👦";
        assert_eq!(wrap_text(family, Some(2)), vec!["👨‍👩‍👧‍👦", "👨‍👩‍👧‍👦"]);

        let id = "00000000-0000-0000-0000-000000000001";
        assert_eq!(wrap_text(id, Some(8)), vec![id]);
    }

    #[test]
    fn styled_wrapping_preserves_span_tokens_and_authored_whitespace() {
        let line = Line::new()
            .with(Span::new("  plain  ", Token::Label))
            .with(Span::new("styled words  ", Token::Accent));

        let wrapped = wrap_line(&line, Some(80));

        assert_eq!(wrapped, vec![line]);
        assert_eq!(wrapped[0].spans()[0].token(), Token::Label);
        assert_eq!(wrapped[0].spans()[1].token(), Token::Accent);
    }

    #[test]
    fn semantic_copyable_spans_are_never_split() {
        for token in [Token::Command, Token::Reference] {
            let atom = "opaque-copyable-value-without-a-url-shape";
            let wrapped = wrap_line(&Line::styled(atom, token), Some(8));
            assert_eq!(wrapped.len(), 1);
            assert_eq!(wrapped[0].spans()[0].content(), atom);
            assert_eq!(wrapped[0].spans()[0].token(), token);
        }
    }

    #[test]
    fn wrapping_consumes_one_separator_and_preserves_remaining_space_style() {
        let line = Line::new()
            .with(Span::new("  first  ", Token::Label))
            .with(Span::new("second", Token::Accent));

        let wrapped = wrap_line(&line, Some(8));

        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].spans()[0].content(), "  first");
        assert_eq!(wrapped[0].spans()[0].token(), Token::Label);
        assert_eq!(wrapped[1].spans()[0].content(), " ");
        assert_eq!(wrapped[1].spans()[0].token(), Token::Label);
        assert_eq!(wrapped[1].spans()[1].content(), "second");
        assert_eq!(wrapped[1].spans()[1].token(), Token::Accent);
    }
}
