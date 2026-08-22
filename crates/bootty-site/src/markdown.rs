//! Terminal-friendly Markdown rendering for site content.

use std::sync::LazyLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use tuirealm::ratatui::style::{Color, Modifier, Style};
use tuirealm::ratatui::text::{Line, Span, Text};

const CODE_BG: Color = Color::Rgb(8, 10, 18);
const MUTED: Color = Color::Rgb(139, 149, 182);
const TEXT: Color = Color::Rgb(214, 222, 247);
const YELLOW: Color = Color::Rgb(255, 199, 119);
pub(super) const GREEN: Color = Color::Rgb(158, 220, 106);
pub(super) const CYAN: Color = Color::Rgb(125, 207, 255);
pub(super) const BLUE: Color = Color::Rgb(122, 162, 247);

pub(super) fn render_markdown(
    markdown: &'static str,
    accent: Color,
    code_width: usize,
) -> Text<'static> {
    let mut lines = Vec::new();
    let mut highlighter: Option<HighlightLines<'static>> = None;
    let mut code_language = "";

    for raw in markdown.lines() {
        if let Some(language) = raw.strip_prefix("```") {
            if highlighter.is_some() || !code_language.is_empty() {
                lines.push(Line::from(""));
                highlighter = None;
                code_language = "";
            } else {
                code_language = language.trim();
                lines.push(Line::from(""));
                highlighter = code_highlighter(code_language);
            }
            continue;
        }

        if !code_language.is_empty() {
            lines.push(highlighted_code_line(
                raw,
                code_language,
                highlighter.as_mut(),
                code_width,
            ));
            continue;
        }

        lines.push(markdown_line(raw, accent));
    }

    Text::from(lines)
}

fn markdown_line(raw: &'static str, accent: Color) -> Line<'static> {
    let trimmed = raw.trim_end();
    if trimmed.is_empty() {
        return Line::from("");
    }
    if let Some(text) = trimmed.strip_prefix("# ") {
        return heading_line(text, accent, Modifier::BOLD | Modifier::UNDERLINED);
    }
    if let Some(text) = trimmed.strip_prefix("## ") {
        return heading_line(text, accent, Modifier::BOLD);
    }
    if let Some(text) = trimmed.strip_prefix("### ") {
        return heading_line(text, MUTED, Modifier::BOLD);
    }
    if let Some(text) = trimmed.strip_prefix("- ") {
        let mut spans = vec![Span::styled("  - ", Style::default().fg(accent))];
        spans.extend(inline_spans(text, Style::default().fg(TEXT)));
        return Line::from(spans);
    }
    if let Some((prefix, text)) = trimmed.split_once(". ")
        && !prefix.is_empty()
        && prefix.chars().all(|ch| ch.is_ascii_digit())
    {
        let mut spans = vec![Span::styled(
            format!("{prefix}. "),
            Style::default().fg(accent),
        )];
        spans.extend(inline_spans(text, Style::default().fg(TEXT)));
        return Line::from(spans);
    }
    if let Some(text) = trimmed.strip_prefix("> ") {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(accent))];
        spans.extend(inline_spans(text, Style::default().fg(MUTED)));
        return Line::from(spans);
    }
    Line::from(inline_spans(trimmed, Style::default().fg(TEXT)))
}

fn heading_line(text: &'static str, color: Color, modifier: Modifier) -> Line<'static> {
    Line::from(Span::styled(
        text,
        Style::default().fg(color).add_modifier(modifier),
    ))
}

fn inline_spans(text: &'static str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, part) in text.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        let style = if index % 2 == 1 {
            Style::default().fg(YELLOW).bg(CODE_BG)
        } else {
            base
        };
        spans.push(Span::styled(part, style));
    }
    spans
}

fn highlighted_code_line(
    line: &'static str,
    language: &str,
    highlighter: Option<&mut HighlightLines<'static>>,
    code_width: usize,
) -> Line<'static> {
    if language.eq_ignore_ascii_case("toml") {
        return toml_code_line(line, code_width);
    }

    let mut spans = vec![Span::styled("  ", Style::default().bg(CODE_BG))];
    if let Some(ranges) =
        highlighter.and_then(|highlighter| highlighter.highlight_line(line, syntax_set()).ok())
    {
        spans.extend(
            ranges
                .into_iter()
                .map(|(style, text)| Span::styled(text, syntect_style(style))),
        );
    } else {
        spans.push(Span::styled(line, Style::default().fg(YELLOW).bg(CODE_BG)));
    }
    padded_code_line(spans, code_width)
}

fn toml_code_line(line: &'static str, code_width: usize) -> Line<'static> {
    let mut spans = vec![Span::styled("  ", Style::default().bg(CODE_BG))];
    let trimmed = line.trim_start();
    let leading = line.len().saturating_sub(trimmed.len());
    if leading > 0 {
        spans.push(Span::styled(&line[..leading], Style::default().bg(CODE_BG)));
    }
    if trimmed.is_empty() {
        return padded_code_line(spans, code_width);
    }
    if trimmed.starts_with('#') {
        spans.push(Span::styled(trimmed, code_style(MUTED)));
        return padded_code_line(spans, code_width);
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        spans.push(Span::styled(
            trimmed,
            code_style(CYAN).add_modifier(Modifier::BOLD),
        ));
        return padded_code_line(spans, code_width);
    }
    let Some((key, value)) = trimmed.split_once('=') else {
        spans.push(Span::styled(trimmed, code_style(TEXT)));
        return padded_code_line(spans, code_width);
    };
    spans.push(Span::styled(key.trim_end(), code_style(BLUE)));
    spans.push(Span::styled(" = ", code_style(MUTED)));
    spans.extend(toml_value_spans(value.trim_start()));
    padded_code_line(spans, code_width)
}

fn toml_value_spans(value: &'static str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut token = String::new();
    let mut in_string = false;
    for (index, ch) in value.char_indices() {
        if ch == '"' {
            token.push(ch);
            if in_string {
                spans.push(Span::styled(std::mem::take(&mut token), code_style(YELLOW)));
            }
            in_string = !in_string;
            continue;
        }
        if in_string {
            token.push(ch);
            continue;
        }
        if ch == '#' {
            if !token.is_empty() {
                spans.push(toml_value_token(&std::mem::take(&mut token)));
            }
            spans.push(Span::styled(&value[index..], code_style(MUTED)));
            return spans;
        }
        if matches!(ch, '[' | ']' | ',') {
            if !token.is_empty() {
                spans.push(toml_value_token(&std::mem::take(&mut token)));
            }
            spans.push(Span::styled(ch.to_string(), code_style(MUTED)));
        } else {
            token.push(ch);
        }
    }
    if !token.is_empty() {
        spans.push(toml_value_token(&token));
    }
    spans
}

fn toml_value_token(token: &str) -> Span<'static> {
    let trimmed = token.trim();
    let color = if trimmed.parse::<f64>().is_ok() || matches!(trimmed, "true" | "false") {
        GREEN
    } else {
        TEXT
    };
    Span::styled(token.to_owned(), code_style(color))
}

fn code_style(color: Color) -> Style {
    Style::default().fg(color).bg(CODE_BG)
}

fn padded_code_line(mut spans: Vec<Span<'static>>, code_width: usize) -> Line<'static> {
    let used = spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let pad = code_width.saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(
            "\u{00a0}".repeat(pad),
            Style::default().bg(CODE_BG),
        ));
    }
    Line::from(spans)
}

fn code_highlighter(language: &str) -> Option<HighlightLines<'static>> {
    let language = language.trim().to_ascii_lowercase();
    let candidates: &[&str] = match language.as_str() {
        "ts" | "typescript" => &["ts", "tsx", "TypeScript", "JavaScript"],
        "tsx" => &["tsx", "ts", "TypeScript", "JavaScript"],
        "js" | "jsx" | "javascript" => &["js", "jsx", "JavaScript"],
        "sh" | "shell" | "bash" => &["sh", "bash", "Bourne Again Shell (bash)"],
        "toml" => &["toml", "TOML"],
        "rust" | "rs" => &["rs", "rust", "Rust"],
        "text" => &["txt", "Plain Text"],
        other => &[other],
    };
    let syntaxes = syntax_set();
    let syntax = candidates.iter().find_map(|candidate| {
        syntaxes
            .find_syntax_by_extension(candidate)
            .or_else(|| syntaxes.find_syntax_by_token(candidate))
            .or_else(|| syntaxes.find_syntax_by_name(candidate))
    })?;
    let themes = &theme_set().themes;
    let theme = themes
        .get("base16-ocean.dark")
        .or_else(|| themes.get("Solarized (dark)"))?;
    Some(HighlightLines::new(syntax, theme))
}

fn syntect_style(style: SyntectStyle) -> Style {
    let color = boost_color(style.foreground.r, style.foreground.g, style.foreground.b);
    let mut ratatui_style = Style::default().fg(color).bg(CODE_BG);
    if style.font_style.contains(FontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
    }
    ratatui_style
}

fn boost_color(r: u8, g: u8, b: u8) -> Color {
    let boost = |value: u8| value.saturating_add(48);
    Color::Rgb(boost(r), boost(g), boost(b))
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    &SYNTAX_SET
}

fn theme_set() -> &'static ThemeSet {
    static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);
    &THEME_SET
}
