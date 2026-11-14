//! Animated splash / boot screen — the "Beyond" identity moment.
//!
//! Phases: starfield fade-in → gradient wordmark sweep-in over a drifting
//! nebula → real boot checklist (theme, skills, MCP servers, sessions,
//! system) → bottom-up wipe into the chat. All colors come from the active
//! theme so custom themes restyle the whole sequence.

use crate::theme::{lerp_color, Theme};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::Frame;
use tokio::sync::mpsc;

// ------------------------------------------------------------------ types

/// Lifecycle of one boot checklist entry.
#[derive(Debug, Clone)]
pub enum BootState {
    Pending,
    Done,
    Warn(String),
}

/// A boot step fed to the splash by the rest of the app.
#[derive(Debug, Clone)]
pub struct BootStep {
    pub id: &'static str,
    pub label: &'static str,
    pub state: BootState,
}

impl BootStep {
    pub fn pending(id: &'static str, label: &'static str) -> Self {
        Self { id, label, state: BootState::Pending }
    }
}

/// Updates streamed into the splash while it animates.
#[derive(Debug, Clone)]
pub struct BootUpdate {
    pub id: &'static str,
    pub state: BootState,
}

struct Star {
    x: f32,
    y: f32,
    speed: f32,
    phase: f32,
    color_idx: usize,
    ch: char,
}

struct Nebula {
    x: f32,
    y: f32,
    w: u16,
    h: u16,
    speed: f32,
    color_idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Stars,
    Logo,
    Steps,
    Out,
    Finished,
}

pub struct Splash {
    theme: Theme,
    t: f32,
    phase: Phase,
    stars: Vec<Star>,
    nebulae: Vec<Nebula>,
    steps: Vec<BootStep>,
    boot_rx: mpsc::UnboundedReceiver<BootUpdate>,
    wipe: f32,
    seeded_wh: (u16, u16),
    fast: bool,
}

const STARS_CHARS: [char; 5] = ['·', '✦', '*', '˙', '•'];
const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];
/// Total run time before the wipe even if steps are still pending.
const MAX_WAIT: f32 = 25.0;
/// How long the fully-done screen lingers before wiping.
const LINGER: f32 = 0.7;

impl Splash {
    pub fn new(theme: Theme, steps: Vec<BootStep>, boot_rx: mpsc::UnboundedReceiver<BootUpdate>, fast: bool) -> Self {
        Self {
            theme,
            t: 0.0,
            phase: Phase::Stars,
            stars: vec![],
            nebulae: vec![],
            steps,
            boot_rx,
            wipe: 0.0,
            seeded_wh: (0, 0),
            fast,
        }
    }

    /// Apply any queued boot updates (non-blocking).
    fn pump(&mut self) {
        while let Ok(u) = self.boot_rx.try_recv() {
            if let Some(s) = self.steps.iter_mut().find(|s| s.id == u.id) {
                s.state = u.state;
            }
        }
    }

    fn all_done(&self) -> bool {
        self.steps.iter().all(|s| !matches!(s.state, BootState::Pending))
    }

    /// Advance the animation by `dt` seconds; returns true when finished.
    pub fn update(&mut self, dt: f32) -> bool {
        self.pump();
        if self.fast {
            // instant mode: skip phases, single static frame
            for s in &mut self.steps {
                if matches!(s.state, BootState::Pending) {
                    s.state = BootState::Done;
                }
            }
            self.phase = Phase::Out;
            self.wipe += dt * 4.0;
            if self.wipe >= 1.1 {
                self.phase = Phase::Finished;
            }
            return self.phase == Phase::Finished;
        }

        self.t += dt;
        self.phase = match self.phase {
            Phase::Stars if self.t >= 0.9 => Phase::Logo,
            Phase::Logo if self.t >= 2.2 => Phase::Steps,
            Phase::Steps if self.all_done() && self.t >= 2.9 + LINGER => Phase::Out,
            Phase::Steps if self.t >= MAX_WAIT => Phase::Out,
            Phase::Out => {
                self.wipe += dt * 3.5;
                if self.wipe >= 1.05 {
                    self.phase = Phase::Finished;
                }
                return self.phase == Phase::Finished;
            }
            p => p,
        };

        if self.phase == Phase::Out {
            self.wipe += dt * 3.5;
            if self.wipe >= 1.05 {
                self.phase = Phase::Finished;
            }
        }
        self.phase == Phase::Finished
    }

    /// Seed starfield/nebula for the given area (re-seeds on resize).
    fn seed(&mut self, area: Rect) {
        let (w, h) = (area.width, area.height);
        if self.seeded_wh == (w, h) && !self.stars.is_empty() {
            return;
        }
        self.seeded_wh = (w, h);
        let mut rng = SimpleRng::new(0xB421_2026);

        self.stars = (0..(w as usize * h as usize / 14).min(220))
            .map(|_| Star {
                x: rng.next() as f32 * w as f32,
                y: rng.next() as f32 * h as f32,
                speed: 1.5 + rng.next() as f32 * 3.0,
                phase: rng.next() as f32 * std::f32::consts::TAU,
                color_idx: rng.next() as usize % self.theme.starfield.len().max(1),
                ch: STARS_CHARS[rng.next() as usize % STARS_CHARS.len()],
            })
            .collect();

        self.nebulae = (0..7)
            .map(|_| Nebula {
                x: rng.next() as f32 * w as f32,
                y: rng.next() as f32 * h as f32 * 0.5,
                w: 8 + (rng.next() % 14) as u16,
                h: 2 + (rng.next() % 3) as u16,
                speed: 0.8 + rng.next() as f32 * 1.6,
                color_idx: rng.next() as usize % self.theme.starfield.len().max(1),
            })
            .collect();
    }

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        self.seed(area);
        let t = self.t;

        // --- nebula (drifting soft clouds) --------------------------
        for n in &self.nebulae {
            let nx = (n.x + t * n.speed) % (area.width as f32 + n.w as f32) - n.w as f32;
            let base = self.theme.starfield[n.color_idx % self.theme.starfield.len()];
            let dim = lerp_color(base, Color::Rgb(12, 10, 26), 0.72);
            for dy in 0..n.h {
                for dx in 0..n.w {
                    let px = nx as i32 + dx as i32;
                    let py = n.y as i32 + dy as i32;
                    if px < 0 || py < 0 || px >= area.width as i32 || py >= area.height as i32 {
                        continue;
                    }
                    let ch = if (dx + dy) % 3 == 0 { '░' } else { '˙' };
                    let buf = f.buffer_mut();
                    if let Some(cell) = buf.cell_mut((px as u16, py as u16)) {
                        cell.set_char(ch).set_style(Style::new().fg(dim));
                    }
                }
            }
        }

        // --- starfield (twinkling) ----------------------------------
        for st in &self.stars {
            let tw = 0.30 + 0.70 * (0.5 + 0.5 * (t * st.speed + st.phase).sin());
            let base = self.theme.starfield[st.color_idx % self.theme.starfield.len()];
            let c = lerp_color(self.theme.fg_dim, base, tw);
            let x = st.x as u16 % area.width.max(1);
            let y = st.y as u16 % area.height.max(1);
            let buf = f.buffer_mut();
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(st.ch).set_style(Style::new().fg(c));
            }
        }

        // --- wordmark + gradient rule -------------------------------
        let wm_w = crate::theme::WORDMARK_W as u16;
        let x0 = area.width.saturating_sub(wm_w) / 2;
        let y0 = area.height.saturating_sub(15) / 2;

        // sweep highlight travels across the wordmark, looping slowly
        let sweep_px = ((t * 22.0) % (wm_w as f32 + 40.0)) - 20.0;

        for (row, line) in crate::theme::WORDMARK.iter().enumerate() {
            let mut spans: Vec<Span> = Vec::new();
            let chars: Vec<char> = line.chars().collect();
            let mut col = 0usize;
            let mut plain = String::new();
            for (ci, ch) in chars.iter().enumerate() {
                if *ch == ' ' {
                    plain.push(' ');
                    col += 1;
                    continue;
                }
                if !plain.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut plain)));
                }
                let gx = x0 as usize + ci;
                let tt = (ci as f32 / crate::theme::WORDMARK_W as f32).clamp(0.0, 1.0);
                // vertical blend adds a galaxy "depth" feel
                let vt = row as f32 / 4.0 * 0.18;
                let mut c = self.theme.gradient_color_animated((tt + vt).clamp(0.0, 1.0), t);
                // sweep sparkle
                let d = (gx as f32 - sweep_px).abs();
                if d < 4.0 {
                    c = lerp_color(c, Color::Rgb(255, 255, 255), (1.0 - d / 4.0) * 0.65);
                }
                plain.push(*ch);
                spans.push(Span::styled(std::mem::take(&mut plain), Style::new().fg(c)));
                col += 1;
            }
            if !plain.is_empty() {
                spans.push(Span::raw(plain));
            }
            let _ = col;
            f.render_widget(
                Line::from(spans),
                Rect::new(x0, y0 + row as u16, wm_w, 1),
            );
        }

        // gradient rule under the wordmark
        let rule_y = y0 + crate::theme::WORDMARK.len() as u16;
        let rule: Vec<Span> = (0..wm_w)
            .map(|i| {
                let tt = i as f32 / wm_w.max(1) as f32;
                Span::styled(
                    "─",
                    Style::new().fg(self.theme.gradient_color_animated(tt, t)),
                )
            })
            .collect();
        f.render_widget(Line::from(rule), Rect::new(x0, rule_y, wm_w, 1));

        // tagline + version
        let tag = Line::from(vec![
            Span::styled("✦ ", Style::new().fg(self.theme.accent3)),
            Span::styled(
                "a galaxy-grade coding agent",
                Style::new().fg(self.theme.accent2).add_modifier(Modifier::ITALIC),
            ),
            Span::styled(" ✦", Style::new().fg(self.theme.accent3)),
        ]);
        f.render_widget(
            tag.clone().centered(),
            Rect::new(0, rule_y + 2, area.width, 1),
        );
        let ver = Line::from(Span::styled(
            format!("v{} — powered by Claude", env!("CARGO_PKG_VERSION")),
            Style::new().fg(self.theme.fg_dim),
        ));
        f.render_widget(ver.centered(), Rect::new(0, rule_y + 3, area.width, 1));

        // --- boot checklist ------------------------------------------
        if self.phase == Phase::Steps || self.phase == Phase::Out {
            let list_y = rule_y + 5;
            for (i, step) in self.steps.iter().enumerate() {
                let y = list_y + i as u16;
                if y >= area.height {
                    break;
                }
                let (icon, style, note) = match &step.state {
                    BootState::Pending => {
                        let ic = SPINNER[((t * 5.0) as usize + i) % SPINNER.len()];
                        (ic.to_string(), Style::new().fg(self.theme.fg_dim), String::new())
                    }
                    BootState::Done => ("✓".to_string(), Style::new().fg(self.theme.success), String::new()),
                    BootState::Warn(m) => {
                        ("!".to_string(), Style::new().fg(self.theme.warning), m.clone())
                    }
                };
                let line = Line::from(vec![
                    Span::styled(format!("{icon} "), style),
                    Span::styled(step.label.to_string(), Style::new().fg(self.theme.fg)),
                    if note.is_empty() {
                        Span::raw("")
                    } else {
                        Span::styled(format!(" — {note}"), Style::new().fg(self.theme.fg_dim))
                    },
                ]);
                let width = area.width.min(64);
                let lx = area.width.saturating_sub(width) / 2;
                f.render_widget(line, Rect::new(lx, y, width, 1));
            }
        }

        // --- out phase: bottom-up wipe -------------------------------
        if self.phase == Phase::Out {
            let wipe_h = (area.height as f32 * self.wipe).ceil() as u16;
            let start = area.height.saturating_sub(wipe_h);
            for y in start..area.height {
                for x in 0..area.width {
                    let buf = f.buffer_mut();
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(' ').set_style(Style::new());
                    }
                }
            }
        }
    }
}

/// Tiny xorshift-ish RNG — enough for starfield seeding without a dep.
struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x % 1000
    }
}
