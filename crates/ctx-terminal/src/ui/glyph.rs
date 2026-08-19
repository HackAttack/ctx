use super::RenderContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Glyph {
    Bullet,
    Success,
    Failure,
    Warning,
    Progress,
    Rule,
    BoxTopLeft,
    BoxTopRight,
    BoxBottomLeft,
    BoxBottomRight,
    BoxVertical,
}

impl Glyph {
    pub(super) const fn render(self, context: &RenderContext) -> &'static str {
        match (self, context.unicode()) {
            (Self::Bullet, true) => "•",
            (Self::Bullet, false) => "-",
            (Self::Success, true) => "✓",
            (Self::Success, false) => "OK",
            (Self::Failure, true) => "✗",
            (Self::Failure, false) => "X",
            (Self::Warning, _) => "!",
            (Self::Progress, true) => "━",
            (Self::Progress, false) => "=",
            (Self::Rule, true) => "─",
            (Self::Rule, false) => "-",
            (Self::BoxTopLeft, true) => "╭",
            (Self::BoxTopLeft, false) => "+",
            (Self::BoxTopRight, true) => "╮",
            (Self::BoxTopRight, false) => "+",
            (Self::BoxBottomLeft, true) => "╰",
            (Self::BoxBottomLeft, false) => "+",
            (Self::BoxBottomRight, true) => "╯",
            (Self::BoxBottomRight, false) => "+",
            (Self::BoxVertical, true) => "│",
            (Self::BoxVertical, false) => "|",
        }
    }
}
