//! Lightweight markdown renderer for assistant output → styled `Line`s.
//!
//! Supports the subset that matters in a terminal chat: fenced code blocks,
//! inline code, bold/italic, headers, bullets and block quotes. Everything is
//! rendered as owned spans so `Line<'static>` can be cached in chat history.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;

use crate::theme::Theme;

/// Convert a markdown string into styled lines.
pub fn md_to_lines(md: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_code = false;

    for raw in md.lines() {
        // fenced code block toggles
        if let Some(rest) = raw.trim().strip_prefix("```") {
            if in_code {
                in_code = false;
                out.push(code_border_bottom(theme));
                continue;
            }
            in_code = true;
            let lang = rest.trim().to_string();
            out.push(code_border_top(&lang, theme));
            continue;
        }

        if in_code {
            out.push(code_line(raw, theme));
            continue;
        }

        // headers
        let trimmed = raw.trim_start();
        if let Some(h) = trimmed.strip_prefix("### ") {
            out.push(header_line(h, 3, theme));
            continue;
        }
        if let Some(h) = trimmed.strip_prefix("## ") {
            out.push(header_line(h, 2, theme));
            continue;
        }
        if let Some(h) = trimmed.strip_prefix("# ") {
            out.push(header_line(h, 1, theme));
            continue;
        }

        // block quote
        if let Some(q) = trimmed.strip_prefix("> ") {
            let inner = inline(q, theme.fg_dim, theme);
            let mut spans = vec![Span::styled("│ ", Style::new().fg(theme.fg_dim))];
            spans.extend(inner);
            out.push(Line::from(spans));
            continue;
        }

        // bullets
        if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let inner = inline(b, theme.fg, theme);
            let mut spans = vec![Span::styled("  • ", Style::new().fg(theme.accent))];
            spans.extend(inner);
            out.push(Line::from(spans));
            continue;
        }

        // numbered list
        if let Some((num, rest)) = split_numbered(trimmed) {
            let inner = inline(rest, theme.fg, theme);
            let mut spans = vec![Span::styled(format!("  {num} "), Style::new().fg(theme.accent))];
            spans.extend(inner);
            out.push(Line::from(spans));
            continue;
        }

        // empty line → spacing
        if trimmed.is_empty() {
            out.push(Line::from(""));
            continue;
        }

        // plain paragraph
        out.push(Line::from(inline(trimmed, theme.fg, theme)));
    }

    out
}

/// Word-wrap one styled line to `width` display columns, keeping span styles.
///
/// Breaks at whitespace where possible; a single token wider than the line
/// (long paths, URLs) is hard-split. Leading indentation is preserved so code
/// keeps its shape. Doing this ourselves (instead of `Paragraph::wrap`) gives
/// the chat view an exact line count to scroll by.
pub fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    let width = width.max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;

    for span in &line.spans {
        let style = span.style;
        for tok in split_keep_ws(&span.content) {
            let w = UnicodeWidthStr::width(tok);
            if col + w <= width {
                cur.push(Span::styled(tok.to_string(), style));
                col += w;
            } else if tok.chars().all(char::is_whitespace) {
                // whitespace at the break point is swallowed by the newline
                out.push(Line::from(std::mem::take(&mut cur)));
                col = 0;
            } else if w <= width {
                out.push(Line::from(std::mem::take(&mut cur)));
                cur.push(Span::styled(tok.to_string(), style));
                col = w;
            } else {
                // token longer than a whole line: split by character
                let mut piece = String::new();
                for ch in tok.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if col + cw > width {
                        if !piece.is_empty() {
                            cur.push(Span::styled(std::mem::take(&mut piece), style));
                        }
                        out.push(Line::from(std::mem::take(&mut cur)));
                        col = 0;
                    }
                    piece.push(ch);
                    col += cw;
                }
                if !piece.is_empty() {
                    cur.push(Span::styled(piece, style));
                }
            }
        }
    }
    out.push(Line::from(cur));
    out
}

/// Split into alternating runs of whitespace / non-whitespace.
fn split_keep_ws(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut prev: Option<bool> = None;
    for (i, c) in s.char_indices() {
        let ws = c.is_whitespace();
        if prev.is_some_and(|p| p != ws) {
            out.push(&s[start..i]);
            start = i;
        }
        prev = Some(ws);
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Split "12. text" into ("12.", "text") if present.
fn split_numbered(s: &str) -> Option<(String, &str)> {
    let idx = s.find(". ")?;
    let num: &str = &s[..idx];
    if !num.chars().all(|c| c.is_ascii_digit()) || num.is_empty() || num.len() > 3 {
        return None;
    }
    Some((format!("{num}."), &s[idx + 2..]))
}

fn header_line(text: &str, level: usize, theme: &Theme) -> Line<'static> {
    let style = Style::new()
        .fg(if level == 1 { theme.accent } else { theme.assistant })
        .add_modifier(Modifier::BOLD);
    Line::from(Span::styled(text.to_string(), style))
}

fn code_border_top(lang: &str, theme: &Theme) -> Line<'static> {
    let label = if lang.is_empty() { "code".to_string() } else { lang.to_string() };
    Line::from(Span::styled(
        format!("╭─── {label} "),
        Style::new().fg(theme.accent2).add_modifier(Modifier::DIM),
    ))
}

fn code_border_bottom(theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        "╰───".to_string(),
        Style::new().fg(theme.accent2).add_modifier(Modifier::DIM),
    ))
}

fn code_line(raw: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("│ {raw}"),
        Style::new().fg(theme.accent2),
    ))
}

/// Inline formatting: **bold**, `code`, *italic*.
pub fn inline(text: &str, base: ratatui::style::Color, theme: &Theme) -> Vec<Span<'static>> {
    thread_local! {
        static INLINE_RE: Regex = Regex::new(r"(\*\*[^*]+\*\*|`[^`]+`|\*[^*\s][^*]*\*)").unwrap();
    }
    let base_style = Style::new().fg(base);
    let mut spans = Vec::new();
    let mut last = 0usize;

    INLINE_RE.with(|re| {
        for m in re.find_iter(text) {
            if m.start() > last {
                spans.push(Span::styled(text[last..m.start()].to_string(), base_style));
            }
            let tok = m.as_str();
            let span = if let Some(inner) = tok.strip_prefix("**") {
                Span::styled(
                    inner.strip_suffix("**").unwrap_or(inner).to_string(),
                    base_style.add_modifier(Modifier::BOLD),
                )
            } else if let Some(inner) = tok.strip_prefix('`') {
                Span::styled(
                    inner.strip_suffix('`').unwrap_or(inner).to_string(),
                    Style::new().fg(theme.accent2),
                )
            } else if let Some(inner) = tok.strip_prefix('*') {
                Span::styled(
                    inner.strip_suffix('*').unwrap_or(inner).to_string(),
                    base_style.add_modifier(Modifier::ITALIC),
                )
            } else {
                Span::styled(tok.to_string(), base_style)
            };
            spans.push(span);
            last = m.end();
        }
    });

    if last < text.len() {
        spans.push(Span::styled(text[last..].to_string(), base_style));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }
    spans
}
