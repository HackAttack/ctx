use crate::ui::{diagnostic, Diagnostic, DiagnosticLevel, Document, RenderContext};

pub fn render_partial_deprecation(context: &RenderContext) -> Document {
    diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary: "--partial is deprecated",
            detail: Some(
                "It no longer changes import behavior because tolerant import is always enabled.",
            ),
            fields: &[],
            action: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    #[test]
    fn partial_deprecation_is_warning_first_and_style_equivalent() {
        let context = RenderContext::for_test(
            TestContext::tty(StreamKind::Stderr, 80).color(ColorMode::Always),
        );
        let document = render_partial_deprecation(&context);
        let plain = document.render_plain();
        assert_eq!(
            plain,
            "! --partial is deprecated\nIt no longer changes import behavior because tolerant import is always enabled.\n"
        );

        let mut stream = anstream::StripStream::new(Vec::new());
        stream
            .write_all(document.render(&context).as_bytes())
            .unwrap();
        assert_eq!(String::from_utf8(stream.into_inner()).unwrap(), plain);
    }
}
