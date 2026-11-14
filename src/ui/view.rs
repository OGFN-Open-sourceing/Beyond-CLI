//! Drawing for the chat application: header, transcript, input box, status
//! bar, popups and the animated welcome screen.
//!
//! Everything here is a pure function of `App` state. The only state it
//! writes is render caches (`Entry::cache`) and the clamped scroll offset.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{App, Choice, Entry, EntryKind, Popup, Prompt, ToolState};
use super::render::{inline, md_to_lines, wrap_line};
use super::scene;
use crate::theme::{lerp_color, Theme, WORDMARK, WORDMARK_W};
use crate::util;

/// Tallest the input box grows before it scrolls internally.
const INPUT_MAX_ROWS: usize = 8;
const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];
/// Backdrop used for fades when the theme keeps the terminal's own background.
const SPACE: Color = Color::Rgb(12, 10, 26);

// ---------------------------------------------------------------- entries

impl Entry {
    /// Wrapped, styled lines for this entry at `width` (cached).
    pub(super) fn lines(&mut self, theme: &Theme, width: u16) -> &[Line<'static>] {
        if self.cache.as_ref().is_none_or(|(w, _)| *w != width) {
            self.cache = Some((width, build_lines(&self.kind, &self.text, theme, width)));
        }
        &self.cache.as_ref().unwrap().1
    }
}

/// Render one transcript entry: a 2-column gutter glyph + wrapped body.
fn build_lines(kind: &EntryKind, text: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let plain = |color: Color| -> Vec<Line<'static>> {
        text.lines().map(|l| Line::from(inline(l, color, theme))).collect()
    };
    let (glyph, glyph_color, body) = match kind {
        EntryKind::User => {
            let style = Style::new().fg(theme.fg).add_modifier(Modifier::BOLD);
            let body = text.lines().map(|l| Line::from(Span::styled(l.to_string(), style))).collect();
            ('❯', theme.user, body)
        }
        EntryKind::Assistant => ('✦', theme.accent, md_to_lines(text, theme)),
        EntryKind::Tool { name, state } => {
            let (glyph, color) = match state {
                ToolState::Running => ('◌', theme.tool),
                ToolState::Ok => ('✓', theme.success),
                ToolState::Failed => ('✗', theme.error),
            };
            let mut spans = vec![Span::styled(name.clone(), Style::new().fg(theme.tool))];
            if !text.is_empty() {
                spans.push(Span::styled(format!("  {text}"), Style::new().fg(theme.fg_dim)));
            }
            (glyph, color, vec![Line::from(spans)])
        }
        EntryKind::Info => ('·', theme.fg_dim, plain(theme.fg_dim)),
        EntryKind::Success => ('✓', theme.success, plain(theme.fg)),
        EntryKind::Warn => ('!', theme.warning, plain(theme.fg)),
        EntryKind::Error => ('✗', theme.error, plain(theme.fg)),
        EntryKind::Reminder => ('◷', theme.accent3, plain(theme.fg)),
    };

    // Tool calls stack tightly; everything else gets a blank line above.
    let mut out = Vec::new();
    if !matches!(kind, EntryKind::Tool { .. }) {
        out.push(Line::default());
    }
    let body_w = width.saturating_sub(2) as usize;
    let mut first = true;
    for line in &body {
        for wrapped in wrap_line(line, body_w) {
            let gutter = if first {
                Span::styled(format!("{glyph} "), Style::new().fg(glyph_color).add_modifier(Modifier::BOLD))
            } else {
                Span::raw("  ")
            };
            first = false;
            let mut spans = vec![gutter];
            spans.extend(wrapped.spans);
            out.push(Line::from(spans));
        }
    }
    out
}

// ------------------------------------------------------------------- draw

impl App {
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        if self.theme.bg.is_some() {
            f.render_widget(Block::new().style(self.theme.style_bg()), area);
        }
        if area.width < 30 || area.height < 10 {
            let msg = Paragraph::new("window too small").style(Style::new().fg(self.theme.fg_dim));
            f.render_widget(msg, area);
            return;
        }
        if let Some(w) = self.wizard.as_mut() {
            w.draw(f, &self.theme);
            return;
        }

        // The input box grows with its content, so lay it out first.
        let input_w = area.width.saturating_sub(7) as usize; // borders, padding, "❯ ", cursor column
        let (shown, shown_cursor) = self.shown_input();
        let (rows, cursor_pos) = layout_input(&shown, shown_cursor, input_w);
        let input_h = rows.len().clamp(1, INPUT_MAX_ROWS) as u16 + 2;

        let [header, chat, input, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(input_h),
            Constraint::Length(1),
        ])
        .areas(area);

        self.draw_header(f, header);
        if self.show_welcome() {
            self.draw_welcome(f, chat);
            self.draw_notices(f, chat);
        } else {
            self.draw_chat(f, chat);
        }
        self.draw_input(f, input, &rows, cursor_pos);
        self.draw_status(f, status);
        self.draw_popup(f, area, input);
    }

    /// Animated gradient color for position `pos` (0..=1) along a banner.
    fn shimmer(&self, pos: f32) -> Color {
        let t = (pos * 0.8 + self.frame_t * 0.12).rem_euclid(2.0);
        self.theme.gradient_color(if t > 1.0 { 2.0 - t } else { t })
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let title = "✦ BEYOND";
        let n = title.chars().count() as f32;
        let mut left: Vec<Span> = vec![Span::raw(" ")];
        for (i, ch) in title.chars().enumerate() {
            left.push(Span::styled(
                ch.to_string(),
                Style::new().fg(self.shimmer(i as f32 / n)).add_modifier(Modifier::BOLD),
            ));
        }
        left.push(Span::styled(format!("  {}", self.cfg.model), Style::new().fg(th.fg_dim)));
        if let Some(p) = &self.project {
            let room = area.width.saturating_sub(48) as usize;
            if room >= 12 {
                left.push(Span::styled(format!("  {}", project_tag(p, room)), Style::new().fg(th.accent2)));
            }
        }
        f.render_widget(Line::from(left), area);

        let right = Line::from(Span::styled(
            format!("session {} ", self.session.id),
            Style::new().fg(th.fg_dim),
        ));
        f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
    }

    fn draw_chat(&mut self, f: &mut Frame, area: Rect) {
        let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
        let height = inner.height as usize;

        // Pass 1: make sure every entry is laid out; total gives the scroll range.
        let lens: Vec<usize> =
            self.entries.iter_mut().map(|e| e.lines(&self.theme, inner.width).len()).collect();
        let total: usize = lens.iter().sum();
        let max_scroll = total.saturating_sub(height);
        self.scroll = self.scroll.min(max_scroll);
        let start = max_scroll - self.scroll;
        let end = start + height;

        // Pass 2: copy only the lines that are actually on screen.
        let mut visible: Vec<Line<'static>> = Vec::with_capacity(height);
        let mut offset = 0usize;
        for (entry, len) in self.entries.iter().zip(&lens) {
            let (lo, hi) = (offset.max(start), (offset + len).min(end));
            if lo < hi
                && let Some((_, lines)) = &entry.cache
            {
                visible.extend_from_slice(&lines[lo - offset..hi - offset]);
            }
            offset += len;
            if offset >= end {
                break;
            }
        }
        f.render_widget(Paragraph::new(visible), inner);

        if self.scroll > 0 {
            let hint = format!(" ↓ {} more · PgDn ", self.scroll);
            let w = hint.width() as u16;
            let r = Rect::new(area.right().saturating_sub(w + 1), area.bottom() - 1, w, 1);
            let style = Style::new().fg(self.theme.fg).bg(self.theme.selection_bg);
            f.render_widget(Paragraph::new(hint).style(style), r);
        }
    }

    /// The few short notices that may sit on the welcome screen ("model set
    /// to…"), pinned to the bottom so the scene stays up until a real chat starts.
    fn draw_notices(&mut self, f: &mut Frame, area: Rect) {
        let width = area.width.saturating_sub(2);
        let mut lines: Vec<Line<'static>> = Vec::new();
        for e in self.entries.iter_mut() {
            lines.extend_from_slice(e.lines(&self.theme, width));
        }
        // entries carry a blank spacer line; don't let it eat the last row
        while lines.last().is_some_and(|l| l.width() == 0) {
            lines.pop();
        }
        let h = (lines.len() as u16).min(area.height);
        if h == 0 {
            return;
        }
        let skip = lines.len() - h as usize;
        let rect = Rect::new(area.x + 1, area.bottom() - h, width, h);
        f.render_widget(Clear, rect);
        f.render_widget(Paragraph::new(lines.split_off(skip)).style(self.theme.style_bg()), rect);
    }

    /// Empty-conversation screen: twinkling starfield, gradient wordmark, tips.
    fn draw_welcome(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let t = self.frame_t;
        let backdrop = th.bg.unwrap_or(SPACE);

        let big = area.width as usize >= WORDMARK_W + 4 && area.height >= 15;
        let block_h: u16 = if big { 13 } else { 6 };
        let mut y = area.y + area.height.saturating_sub(block_h) / 2;

        // Stars: fixed pseudo-random positions, brightness on a sine per star.
        if !th.starfield.is_empty() && scene::wants_stars(th.scene) {
            const GLYPHS: [char; 5] = ['·', '˙', '✦', '·', '•'];
            let count = (area.width as usize * area.height as usize / 45).min(140);
            let mut seed: u32 = 0x9E37_79B9;
            let mut next = || {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed >> 8
            };
            for i in 0..count {
                let x = area.x + (next() % area.width as u32) as u16;
                let y = area.y + (next() % area.height as u32) as u16;
                let phase = (next() % 628) as f32 / 100.0;
                let speed = 0.6 + (i % 5) as f32 * 0.35;
                let glow = 0.5 + 0.5 * (t * speed + phase).sin();
                let color = lerp_color(backdrop, th.starfield[i % th.starfield.len()], 0.15 + glow * 0.7);
                if let Some(cell) = f.buffer_mut().cell_mut((x, y)) {
                    cell.set_char(GLYPHS[i % GLYPHS.len()]).set_style(Style::new().fg(color));
                }
            }
        }

        // The theme's pixel-art scene, kept clear of the text in the middle.
        let keep_w = (WORDMARK_W as u16 + 8).min(area.width);
        let keep = Rect::new(
            area.x + (area.width - keep_w) / 2,
            y.saturating_sub(1).max(area.y),
            keep_w,
            (block_h + 3).min(area.height),
        );
        scene::draw(f.buffer_mut(), area, keep, th, t);

        if big {
            let x0 = area.x + (area.width - WORDMARK_W as u16) / 2;
            for (row, line) in WORDMARK.iter().enumerate() {
                for (col, ch) in line.chars().enumerate().filter(|(_, c)| *c != ' ') {
                    let pos = col as f32 / WORDMARK_W as f32 + row as f32 * 0.04;
                    let style = Style::new().fg(self.shimmer(pos));
                    if let Some(cell) = f.buffer_mut().cell_mut((x0 + col as u16, y + row as u16)) {
                        cell.set_char(ch).set_style(style);
                    }
                }
            }
            y += WORDMARK.len() as u16 + 1;
        } else {
            let title = "✦ B E Y O N D ✦";
            let n = title.chars().count() as f32;
            let spans: Vec<Span> = title
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    Span::styled(c.to_string(), Style::new().fg(self.shimmer(i as f32 / n)).add_modifier(Modifier::BOLD))
                })
                .collect();
            f.render_widget(Line::from(spans).centered(), Rect::new(area.x, y, area.width, 1));
            y += 2;
        }

        let dim = Style::new().fg(th.fg_dim);
        let key = Style::new().fg(th.accent2);
        let mut lines = vec![
            Line::from(Span::styled(
                "a galaxy-grade coding agent",
                Style::new().fg(th.accent2).add_modifier(Modifier::ITALIC),
            )),
            Line::default(),
            Line::from(vec![
                Span::styled("/help", key),
                Span::styled(" commands   ", dim),
                Span::styled("/project", key),
                Span::styled(" tag a folder   ", dim),
                Span::styled("/theme", key),
                Span::styled(" restyle", dim),
            ]),
            Line::from(vec![
                Span::styled("/loadsession", key),
                Span::styled(" resume   ", dim),
                Span::styled("/later", key),
                Span::styled(" remind me   ", dim),
                Span::styled("F9", key),
                Span::styled(" dictate", dim),
            ]),
        ];
        lines.push(Line::default());
        if !self.signed_in() {
            lines.push(Line::from(Span::styled(
                "not signed in — type /login to get started",
                Style::new().fg(th.warning),
            )));
        } else if self.uses_claude() {
            let who = match &self.claude_auth {
                Some(a) if a.is_subscription() => "signed in with your Claude plan",
                Some(_) => "signed in through Claude Code",
                None => "checking your Claude Code sign-in…",
            };
            lines.push(Line::from(Span::styled(who, dim)));
        }
        for line in lines {
            if y >= area.bottom() {
                break;
            }
            f.render_widget(line.centered(), Rect::new(area.x, y, area.width, 1));
            y += 1;
        }
    }

    /// What the input box displays: the text itself, or dots for a secret.
    fn shown_input(&self) -> (String, usize) {
        match self.prompt {
            Some(Prompt::ApiKey { .. }) => {
                let before = self.input[..self.cursor].chars().count();
                ("•".repeat(self.input.chars().count()), before * '•'.len_utf8())
            }
            None => (self.input.clone(), self.cursor),
        }
    }

    fn draw_input(&self, f: &mut Frame, area: Rect, rows: &[String], cursor: (usize, usize)) {
        let th = &self.theme;
        let (title, border) = match (&self.prompt, self.listening) {
            (Some(Prompt::ApiKey { limited: true }), _) => {
                (" paste API key (48-hour login) · Enter to save · Esc to cancel ", th.warning)
            }
            (Some(Prompt::ApiKey { limited: false }), _) => {
                (" paste API key · Enter to save · Esc to cancel ", th.warning)
            }
            (None, true) => (" listening — speak now · F9 to stop ", th.accent3),
            (None, false) => ("", th.border_focus),
        };
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .padding(ratatui::widgets::Padding::horizontal(1))
            .title(Span::styled(title, Style::new().fg(border)));
        let inner = block.inner(area);
        f.render_widget(block, area);

        // Keep the cursor row inside the visible window.
        let view_h = inner.height as usize;
        let top = cursor.0.saturating_sub(view_h.saturating_sub(1));

        let mut lines: Vec<Line> = Vec::with_capacity(view_h);
        if self.input.is_empty() {
            let hint = if self.prompt.is_some() {
                "paste or type your key (it stays hidden)"
            } else {
                "ask anything  ·  / for commands  ·  Shift+Enter for a new line"
            };
            lines.push(Line::from(vec![
                Span::styled("❯ ", Style::new().fg(th.accent)),
                Span::styled(hint, Style::new().fg(th.fg_dim)),
            ]));
        } else {
            for (i, row) in rows.iter().enumerate().skip(top).take(view_h) {
                let gutter = if i == 0 { "❯ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(gutter, Style::new().fg(th.accent)),
                    Span::styled(row.clone(), Style::new().fg(th.fg)),
                ]));
            }
        }
        f.render_widget(Paragraph::new(lines), inner);

        // No text cursor while a modal popup has the keyboard.
        //
        // The cursor is a cell we paint ourselves, and the terminal's own
        // cursor stays hidden: the real one visibly jumps around the screen
        // while animated frames are being written, which reads as flicker.
        if matches!(self.popup, Popup::None | Popup::Autocomplete { .. }) {
            let x = (inner.x + 2 + cursor.1 as u16).min(inner.right().saturating_sub(1));
            let y = inner.y + (cursor.0 - top) as u16;
            if let Some(cell) = f.buffer_mut().cell_mut((x, y)) {
                cell.set_style(Style::new().fg(th.bg.unwrap_or(SPACE)).bg(th.accent));
            }
        }
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let th = &self.theme;
        let dim = Style::new().fg(th.fg_dim);

        // left: what the agent is doing
        let left = if self.agent.is_some() {
            let secs = self.turn_started.elapsed().as_secs();
            let frame = if self.cfg.animations { (self.frame_t * 8.0) as usize } else { secs as usize };
            let mut spans = vec![
                Span::styled(format!(" {} ", SPINNER[frame % SPINNER.len()]), Style::new().fg(th.accent)),
                Span::styled(format!("{} ", self.activity), Style::new().fg(th.fg)),
                Span::styled(format!("{secs}s · Esc to interrupt"), dim),
            ];
            if !self.queued.is_empty() {
                spans.push(Span::styled(format!(" · {} queued", self.queued.len()), Style::new().fg(th.accent3)));
            }
            Line::from(spans)
        } else {
            Line::from(Span::styled(" ready · /help · PgUp/PgDn scroll · Ctrl+C quit", dim))
        };
        let left_w = left.width() as u16;
        f.render_widget(left, area);

        // right: usage + machine meters, dropped piece by piece when cramped
        let used = self.cfg.usage.tokens_today();
        let usage = match self.cfg.usage.left_today(self.cfg.usage_budget_daily) {
            Some(left) => format!("{} left today", util::human_tokens(left)),
            None => format!("{} today", util::human_tokens(used)),
        };
        let (cpu, ram_used, ram_total) = self.sys.snapshot();
        let gb = |b: u64| b as f64 / 1_073_741_824.0;
        let mut parts = vec![usage];
        // money only matters when it is actually billed (not on a Claude plan)
        let on_plan = self.uses_claude() && self.claude_auth.as_ref().is_some_and(|a| a.is_subscription());
        if self.session_cost > 0.0 && !on_plan {
            parts.push(format!("${:.2}", self.session_cost));
        }
        parts.extend([
            format!("cpu {cpu:.0}%"),
            format!("ram {:.1}/{:.0} GB", gb(ram_used), gb(ram_total)),
            self.theme.name.clone(),
        ]);
        let room = area.width.saturating_sub(left_w + 2) as usize;
        while !parts.is_empty() && parts.join(" · ").width() + 1 > room {
            parts.pop();
        }
        if !parts.is_empty() {
            let right = Line::from(Span::styled(format!("{} ", parts.join(" · ")), dim));
            f.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
        }
    }

    // ---------------------------------------------------------------- popups

    fn draw_popup(&self, f: &mut Frame, area: Rect, input: Rect) {
        let th = &self.theme;
        let dim = Style::new().fg(th.fg_dim);
        match &self.popup {
            Popup::None => {}
            Popup::Autocomplete { items, sel } => {
                let rows: Vec<Line> = items
                    .iter()
                    .map(|c| {
                        Line::from(vec![
                            Span::styled(format!("/{}", c.name), Style::new().fg(th.accent2)),
                            Span::styled(format!(" {}", c.args), Style::new().fg(th.accent3)),
                            Span::styled(format!("  {}", c.desc), dim),
                        ])
                    })
                    .collect();
                let h = (rows.len().min(8) as u16 + 2).min(input.y.saturating_sub(area.y + 1));
                if h >= 3 {
                    let w = area.width.saturating_sub(2).min(96);
                    let rect = Rect::new(area.x + 1, input.y - h, w, h);
                    self.draw_list(f, rect, "", "", rows, *sel);
                }
            }
            Popup::ThemePicker { items, sel, .. } => {
                let rows = items.iter().map(|(_, name)| Line::from(Span::raw(name.clone()))).collect();
                let rect = centered(area, 44, items.len() as u16 + 2);
                self.draw_list(f, rect, " theme ", " ↑↓ preview · Enter apply · Esc cancel ", rows, *sel);
            }
            Popup::SessionPicker { items, sel } => {
                let rows = items
                    .iter()
                    .map(|s| {
                        Line::from(vec![
                            Span::styled(format!("{:<7}", s.source), Style::new().fg(th.accent3)),
                            Span::styled(format!("{:<17}", s.when), dim),
                            Span::styled(s.title.clone(), Style::new().fg(th.fg)),
                        ])
                    })
                    .collect();
                let rect = centered(area, 100, items.len() as u16 + 2);
                self.draw_list(f, rect, " resume a session ", " ↑↓ select · Enter load · Esc cancel ", rows, *sel);
            }
            Popup::ChoicePicker { kind, items, sel } => {
                let rows = items
                    .iter()
                    .map(|(id, what)| {
                        Line::from(vec![
                            Span::styled(format!("{id:<19}"), Style::new().fg(th.accent2)),
                            Span::styled(what.clone(), dim),
                        ])
                    })
                    .collect();
                let title = match kind {
                    Choice::Model => " model ",
                    Choice::Mode => " what Claude may do on its own ",
                };
                let rect = centered(area, 90, items.len() as u16 + 2);
                self.draw_list(f, rect, title, " ↑↓ select · Enter apply · Esc cancel ", rows, *sel);
            }
            Popup::QuitConfirm => {
                let rect = centered(area, 44, 5);
                f.render_widget(Clear, rect);
                let block = Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(th.accent))
                    .style(th.style_bg())
                    .title(Span::styled(
                        " close Beyond? ",
                        Style::new().fg(th.accent).add_modifier(Modifier::BOLD),
                    ));
                let inner = block.inner(rect);
                f.render_widget(block, rect);
                let note = if self.agent.is_some() { "Claude is still working" } else { "your session is saved" };
                let lines = vec![
                    Line::from(Span::styled(note, dim)).centered(),
                    Line::default(),
                    Line::from(vec![
                        Span::styled("[y]", Style::new().fg(th.success)),
                        Span::styled(" close     ", dim),
                        Span::styled("[n]", Style::new().fg(th.error)),
                        Span::styled(" stay", dim),
                    ])
                    .centered(),
                ];
                f.render_widget(Paragraph::new(lines), inner);
            }
            Popup::Confirm { title, detail, .. } => {
                let rect = centered(area, 72, 8);
                f.render_widget(Clear, rect);
                let block = Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(th.warning))
                    .style(th.style_bg())
                    .title(Span::styled(
                        format!(" permission · {title} "),
                        Style::new().fg(th.warning).add_modifier(Modifier::BOLD),
                    ));
                let inner = block.inner(rect).inner(ratatui::layout::Margin::new(1, 0));
                f.render_widget(block, rect);

                let mut lines = vec![Line::default()];
                let detail_line = Line::from(Span::styled(detail.clone(), Style::new().fg(th.fg)));
                lines.extend(wrap_line(&detail_line, inner.width as usize).into_iter().take(3));
                while lines.len() < inner.height.saturating_sub(1) as usize {
                    lines.push(Line::default());
                }
                lines.push(Line::from(vec![
                    Span::styled("[y]", Style::new().fg(th.success)),
                    Span::styled(" allow   ", dim),
                    Span::styled("[a]", Style::new().fg(th.accent2)),
                    Span::styled(" always this session   ", dim),
                    Span::styled("[n]", Style::new().fg(th.error)),
                    Span::styled(" deny", dim),
                ]));
                f.render_widget(Paragraph::new(lines), inner);
            }
        }
    }

    /// A bordered, scrolling selection list.
    fn draw_list(&self, f: &mut Frame, rect: Rect, title: &str, hint: &str, rows: Vec<Line>, sel: usize) {
        let th = &self.theme;
        f.render_widget(Clear, rect);
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(th.border))
            .style(th.style_bg())
            .title(Span::styled(title, Style::new().fg(th.accent).add_modifier(Modifier::BOLD)))
            .title_bottom(Span::styled(hint, Style::new().fg(th.fg_dim)));
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let view = inner.height as usize;
        let top = sel.saturating_sub(view.saturating_sub(1));
        for (i, row) in rows.into_iter().enumerate().skip(top).take(view) {
            let r = Rect::new(inner.x, inner.y + (i - top) as u16, inner.width, 1);
            let mut spans = vec![Span::raw(" ")];
            spans.extend(row.spans);
            let mut p = Paragraph::new(Line::from(spans));
            if i == sel {
                p = p.style(Style::new().bg(th.selection_bg).add_modifier(Modifier::BOLD));
            }
            f.render_widget(p, r);
        }
    }
}

// ------------------------------------------------------------------ helpers

/// A `w`×`h` rectangle centered in `area`, shrunk to fit.
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(4));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

/// `[@~/code/app]` — the project tag shown in the header.
pub(super) fn project_tag(path: &std::path::Path, max: usize) -> String {
    let mut shown = path.to_string_lossy().replace('\\', "/");
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy().replace('\\', "/");
        if let Some(rest) = shown.strip_prefix(&home) {
            shown = format!("~{rest}");
        }
    }
    // keep the tail: the folder name matters more than the drive
    let count = shown.chars().count();
    let budget = max.saturating_sub(4).max(8);
    if count > budget {
        let tail: String = shown.chars().skip(count - (budget - 1)).collect();
        shown = format!("…{tail}");
    }
    format!("[@{shown}]")
}

/// Hard-wrap the input text to `width` columns. Returns the rows and the
/// cursor's `(row, column)`; `cursor` is a byte offset into `text`.
fn layout_input(text: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(4);
    let mut rows = vec![String::new()];
    let mut col = 0usize;
    let mut pos = (0usize, 0usize);

    for (i, ch) in text.char_indices() {
        if i == cursor {
            pos = (rows.len() - 1, col);
        }
        if ch == '\n' {
            rows.push(String::new());
            col = 0;
            continue;
        }
        let w = ch.width().unwrap_or(0);
        if col + w > width {
            rows.push(String::new());
            col = 0;
            if i == cursor {
                pos = (rows.len() - 1, 0);
            }
        }
        rows.last_mut().unwrap().push(ch);
        col += w;
    }
    if cursor >= text.len() {
        pos = (rows.len() - 1, col);
    }
    (rows, pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_wraps_and_tracks_cursor() {
        let (rows, pos) = layout_input("abcdef", 6, 4);
        assert_eq!(rows, vec!["abcd", "ef"]);
        assert_eq!(pos, (1, 2));

        let (rows, pos) = layout_input("ab\ncd", 3, 10);
        assert_eq!(rows, vec!["ab", "cd"]);
        assert_eq!(pos, (1, 0));
    }

    #[test]
    fn chat_lines_wrap_with_gutter() {
        let theme = crate::theme::beyond();
        let lines = build_lines(&EntryKind::User, "one two three four five six", &theme, 12);
        // blank separator + several wrapped rows, none wider than the view
        assert!(lines.len() > 3);
        assert!(lines.iter().all(|l| l.width() <= 12));
    }

    /// Draw a whole frame into an in-memory terminal and return its text.
    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    use crate::ui::app::test_app;

    #[test]
    fn welcome_screen_renders() {
        let mut app = test_app();
        let s = screen(&mut app, 100, 30);
        println!("{s}");
        assert!(s.contains("BEYOND"), "header missing");
        assert!(s.contains("██████╗"), "wordmark missing");
        assert!(s.contains("ask anything"), "input hint missing");
        assert!(s.contains("ready"), "status bar missing");
    }

    /// Not a real test: writes each built-in theme's welcome screen to
    /// `target/preview/<id>.html` as a column of animation frames, which
    /// `docs/make-previews.ps1` turns into the README's GIFs.
    /// Run with `cargo test preview_welcome -- --ignored`.
    #[test]
    #[ignore]
    fn preview_welcome_as_html() {
        const FRAMES: usize = 20;
        const STEP: f32 = 0.15; // seconds of animation between frames
        let (w, h) = (110u16, 30u16);
        let css = |c: Color, fallback: &str| match c {
            Color::Rgb(r, g, b) => format!("rgb({r},{g},{b})"),
            _ => fallback.to_string(),
        };
        std::fs::create_dir_all("target/preview").unwrap();
        for (id, _) in crate::theme::available_themes().into_iter().filter(|(id, _)| !id.starts_with("custom:")) {
            let mut app = test_app();
            app.theme = crate::theme::resolve(&id).unwrap();
            let backdrop = css(app.theme.bg.unwrap_or(Color::Rgb(12, 10, 26)), "#0c0a1a");
            // every frame is exactly h × 16px tall, so the strip splits cleanly
            let mut html = format!(
                "<body style='margin:0;background:{backdrop};color:#ccc;font:14px/16px Cascadia Mono,Consolas,monospace'>"
            );
            for frame in 0..FRAMES {
                app.frame_t = 7.3 + frame as f32 * STEP;
                let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                term.draw(|f| app.draw(f)).unwrap();
                let buf = term.backend().buffer().clone();
                html.push_str(&format!("<pre style='margin:0;height:{}px;overflow:hidden'>", h * 16));
                for y in 0..h {
                    for x in 0..w {
                        let cell = &buf[(x, y)];
                        let sym = cell.symbol().replace('&', "&amp;").replace('<', "&lt;");
                        html.push_str(&format!(
                            "<span style='color:{};background:{}'>{sym}</span>",
                            css(cell.fg, "#ccc"),
                            css(cell.bg, "transparent"),
                        ));
                    }
                    html.push('\n');
                }
                html.push_str("</pre>");
            }
            std::fs::write(format!("target/preview/{id}.html"), html).unwrap();
        }
    }

    #[test]
    fn transcript_renders_and_scrolls() {
        let mut app = test_app();
        app.push(EntryKind::User, "fix the parser");
        app.push(EntryKind::Assistant, "Sure.\n\n```rust\nfn main() {}\n```\n- one\n- **two**");
        app.push(EntryKind::Tool { name: "bash".into(), state: ToolState::Ok }, "cargo test");
        for i in 0..60 {
            app.push(EntryKind::Info, format!("line {i}"));
        }
        let s = screen(&mut app, 80, 24);
        println!("{s}");
        assert!(s.contains("line 59") && !s.contains("fix the parser"), "should follow the newest output");

        app.scroll = usize::MAX; // way past the top: must clamp, not panic
        let s = screen(&mut app, 80, 24);
        println!("{s}");
        assert!(s.contains("❯ fix the parser") && s.contains("✓ bash  cargo test"));
        assert!(s.contains("│ fn main() {}"), "code block missing");
        assert!(s.contains("more · PgDn"), "scroll hint missing");
    }

    #[test]
    fn tiny_and_odd_sizes_do_not_panic() {
        let mut app = test_app();
        app.push(EntryKind::Assistant, "hello ".repeat(80));
        app.apply_cmd(crate::commands::Cmd::Help(None));
        for (w, h) in [(1, 1), (20, 5), (30, 10), (31, 11), (45, 12), (200, 60)] {
            screen(&mut app, w, h);
        }
    }

    #[test]
    fn project_tag_shortens_long_paths() {
        let tag = project_tag(std::path::Path::new("C:/a/very/long/path/to/some/project"), 16);
        assert!(tag.starts_with("[@…") && tag.ends_with("project]"));
        assert!(tag.chars().count() <= 16);
    }
}
