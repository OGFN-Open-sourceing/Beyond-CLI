//! Animated pixel-art backdrops for the welcome screen — one per theme.
//!
//! Each terminal cell is split into two stacked "pixels" with half-block
//! characters, so a scene has twice the vertical resolution of the text
//! grid and its pixels come out roughly square. Scenes are pure functions
//! of (size, theme, time): nothing is stored between frames.

use crate::theme::{lerp_color, Scene, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// Backdrop used for fades when the theme keeps the terminal's background.
const SPACE: Color = Color::Rgb(12, 10, 26);

/// Paint `theme.scene` into `area`, leaving `keep` (the text block) untouched.
pub fn draw(buf: &mut Buffer, area: Rect, keep: Rect, theme: &Theme, t: f32) {
    if area.width < 40 || area.height < 10 {
        return; // too small for pixel art to read as anything
    }
    // Gas clouds are faint enough to sit behind the text; solid objects are not.
    let keep = if theme.scene == Scene::Nebula { Rect::default() } else { keep };
    let mut c = Canvas::new(area, keep, theme.bg.unwrap_or(SPACE));
    match theme.scene {
        Scene::None => return,
        Scene::Nebula => nebula(&mut c, theme, t),
        Scene::Galaxy => galaxy(&mut c, theme, t),
        Scene::Night => night(&mut c, theme, t),
        Scene::Ocean => ocean(&mut c, theme, t),
        Scene::Sun => sun(&mut c, theme, t),
        Scene::Aurora => aurora(&mut c, theme, t),
        Scene::Retro => retro(&mut c, theme, t),
        Scene::City => city(&mut c, theme, t),
        Scene::Sakura => sakura(&mut c, theme, t),
        Scene::Code => code(&mut c, theme, t),
        Scene::Embers => embers(&mut c, theme, t),
        Scene::Snow => snow(&mut c, theme, t),
        Scene::Dunes => dunes(&mut c, theme, t),
        Scene::Fireflies => fireflies(&mut c, theme, t),
    }
    c.blit(buf, theme.bg.is_some());
}

/// Does this scene want the twinkling starfield behind it?
pub fn wants_stars(scene: Scene) -> bool {
    matches!(
        scene,
        Scene::None | Scene::Nebula | Scene::Galaxy | Scene::Night | Scene::Aurora | Scene::Retro | Scene::Dunes | Scene::Fireflies
    )
}

// ------------------------------------------------------------------ canvas

/// A pixel grid over a text area (2 pixels per cell, vertically).
struct Canvas {
    area: Rect,
    w: i32,
    h: i32,
    px: Vec<Option<Color>>,
    /// Pixel-space box that stays clear: (x0, y0, x1, y1), end-exclusive.
    keep: (i32, i32, i32, i32),
    backdrop: Color,
}

impl Canvas {
    fn new(area: Rect, keep: Rect, backdrop: Color) -> Self {
        let (w, h) = (area.width as i32, area.height as i32 * 2);
        let kx = keep.x as i32 - area.x as i32;
        let ky = (keep.y as i32 - area.y as i32) * 2;
        Self {
            area,
            w,
            h,
            px: vec![None; (w * h) as usize],
            keep: (kx, ky, kx + keep.width as i32, ky + keep.height as i32 * 2),
            backdrop,
        }
    }

    fn set(&mut self, x: i32, y: i32, color: Color) {
        let (kx0, ky0, kx1, ky1) = self.keep;
        if x < 0 || y < 0 || x >= self.w || y >= self.h || (x >= kx0 && x < kx1 && y >= ky0 && y < ky1) {
            return;
        }
        self.px[(y * self.w + x) as usize] = Some(color);
    }

    /// Where a round object of radius `want` fits without touching the text:
    /// centred in the free strip right of it, or tucked into the top-right
    /// corner when that strip is too narrow. Returns (cx, cy, radius).
    fn perch(&self, want: f32) -> (f32, f32, f32) {
        let strip = (self.w - self.keep.2) as f32;
        let radius = want.min(strip / 2.0 - 2.0).min(self.h as f32 * 0.42);
        if radius >= 5.0 {
            (self.keep.2 as f32 + strip / 2.0, (self.h as f32 * 0.36).max(radius + 1.0), radius)
        } else {
            let radius = want.min(self.keep.1 as f32 / 2.0 - 1.0).max(3.0);
            (self.w as f32 - radius - 3.0, radius + 1.0, radius)
        }
    }

    fn has_keep(&self) -> bool {
        self.keep.2 > self.keep.0 && self.keep.3 > self.keep.1
    }

    /// Height of the free band above the text (pixels from the top).
    fn sky(&self) -> i32 {
        if self.has_keep() { self.keep.1.clamp(4, self.h) } else { self.h * 2 / 5 }
    }

    /// First pixel row of the free band below the text.
    fn floor(&self) -> i32 {
        if self.has_keep() { self.keep.3.clamp(0, self.h - 4) } else { self.h * 3 / 4 }
    }

    /// 0 above/below the text block, rising to 1 well to the side of it —
    /// lets ground objects grow taller where no text is in the way.
    fn side_lift(&self, x: i32) -> f32 {
        if !self.has_keep() {
            return 0.0;
        }
        let d = if x < self.keep.0 { self.keep.0 - x } else if x >= self.keep.2 { x - self.keep.2 + 1 } else { 0 };
        (d as f32 / 14.0).min(1.0)
    }

    /// 0 at the edge of the text block, 1 a few pixels away: falling things
    /// fade out as they approach it instead of being cut off by a rectangle.
    fn clearance(&self, x: i32, y: i32) -> f32 {
        if !self.has_keep() {
            return 1.0;
        }
        let dx = (self.keep.0 - x).max(x - self.keep.2 + 1).max(0);
        let dy = (self.keep.1 - y).max(y - self.keep.3 + 1).max(0);
        (dx.max(dy) as f32 / 7.0).min(1.0)
    }

    /// `color` faded towards the backdrop: `strength` 0 = invisible, 1 = full.
    fn tint(&self, color: Color, strength: f32) -> Color {
        lerp_color(self.backdrop, color, strength.clamp(0.0, 1.0))
    }

    /// Draw a bitmap (`#` = pixel) with its top-left at (x, y).
    fn sprite(&mut self, rows: &[&str], x: i32, y: i32, flip: bool, color: Color) {
        for (dy, row) in rows.iter().enumerate() {
            let n = row.len() as i32;
            for (dx, ch) in row.bytes().enumerate() {
                if ch != b' ' {
                    let dx = if flip { n - 1 - dx as i32 } else { dx as i32 };
                    self.set(x + dx, y + dy as i32, color);
                }
            }
        }
    }

    /// Write the pixels into the text buffer as half blocks.
    fn blit(self, buf: &mut Buffer, solid_bg: bool) {
        for cy in 0..self.area.height as i32 {
            for cx in 0..self.w {
                let top = self.px[(cy * 2 * self.w + cx) as usize];
                let bottom = self.px[((cy * 2 + 1) * self.w + cx) as usize];
                let (ch, style) = match (top, bottom) {
                    (None, None) => continue,
                    (Some(t), Some(b)) => ('▀', Style::new().fg(t).bg(b)),
                    // one lit pixel: the other half keeps whatever is behind it
                    (Some(t), None) if solid_bg => ('▀', Style::new().fg(t).bg(self.backdrop)),
                    (None, Some(b)) if solid_bg => ('▄', Style::new().fg(b).bg(self.backdrop)),
                    (Some(t), None) => ('▀', Style::new().fg(t)),
                    (None, Some(b)) => ('▄', Style::new().fg(b)),
                };
                let pos = (self.area.x + cx as u16, self.area.y + cy as u16);
                if let Some(cell) = buf.cell_mut(pos) {
                    cell.set_char(ch).set_style(style);
                }
            }
        }
    }
}

/// Snap a 0..=1 value to a few steps, so slow fades repaint cells only
/// when they visibly change (keeps terminal output small).
fn steps(v: f32, n: f32) -> f32 {
    (v.clamp(0.0, 1.0) * n).round() / n
}

/// Cheap deterministic 0..1 value per index (scene object placement).
fn hash(i: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x % 10_000) as f32 / 10_000.0
}

// ------------------------------------------------------------------ scenes

/// Beyond: faint clouds of colored gas drifting across the whole screen.
fn nebula(c: &mut Canvas, th: &Theme, t: f32) {
    let t = (t * 4.0).floor() / 4.0; // drift in small ticks, not every frame
    for y in 0..c.h {
        for x in 0..c.w {
            let (fx, fy) = (x as f32, y as f32);
            let n = (fx * 0.085 + t * 0.10).sin()
                + (fy * 0.17 - t * 0.07).sin()
                + ((fx + fy * 1.3) * 0.055 + t * 0.05).sin()
                + ((fx - fy * 1.7) * 0.10 - t * 0.04).cos();
            let v = (n - 1.5) / 2.5; // only the crests show
            if v <= 0.0 {
                continue;
            }
            let hue = th.gradient_color(((fx / c.w as f32) + (fy / c.h as f32) * 0.4).fract());
            c.set(x, y, c.tint(hue, 0.07 + steps(v, 4.0) * 0.16));
        }
    }
}

/// Galaxy: a tilted two-arm spiral turning slowly in the upper right.
fn galaxy(c: &mut Canvas, th: &Theme, t: f32) {
    let (cx, cy, radius) = c.perch((c.h as f32 * 0.40).min(c.w as f32 * 0.19).max(7.0));
    let reach = radius as i32 + 1;
    for y in (cy as i32 - reach)..=(cy as i32 + reach) {
        for x in (cx as i32 - reach)..=(cx as i32 + reach) {
            let dx = x as f32 - cx;
            let dy = (y as f32 - cy) * 1.55; // squash: we see the disc at an angle
            let r = (dx * dx + dy * dy).sqrt() / radius;
            if r > 1.0 {
                continue;
            }
            let angle = dy.atan2(dx);
            let arm = (2.0 * angle - 7.0 * (r + 0.05).ln() + t * 0.25).cos() * 0.5 + 0.5;
            let core = (-(r * 4.5).powi(2)).exp();
            let v = core + arm.powf(2.2) * (1.0 - r).powf(1.1) * 0.85;
            if v < 0.22 {
                continue;
            }
            let hue = lerp_color(th.gradient_color(r), Color::Rgb(255, 250, 235), core);
            c.set(x, y, c.tint(hue, steps(v, 6.0)));
        }
    }
    // a shooting star every few seconds
    let cycle = t / 6.0;
    let phase = cycle.fract();
    if phase < 0.12 {
        let n = cycle.floor() as u32;
        let x0 = hash(n) * c.w as f32 * 0.6;
        let y0 = hash(n + 7) * c.h as f32 * 0.4;
        let head = phase / 0.12 * 26.0;
        for k in 0..7 {
            let d = head - k as f32;
            c.set((x0 + d * 1.6) as i32, (y0 + d * 0.7) as i32, c.tint(Color::Rgb(255, 255, 255), 1.0 - k as f32 / 7.0));
        }
    }
}

const CLOUD: [&str; 5] = [
    "       ######            ",
    "    ############  #####  ",
    "  #######################",
    "#########################",
    "  ####################   ",
];
const CLOUD_SMALL: [&str; 3] = ["   #####      ", " ############ ", "##############"];

/// Midnight: a crescent moon with clouds sliding past underneath.
fn night(c: &mut Canvas, th: &Theme, t: f32) {
    let (cx, cy, radius) = c.perch((c.h as f32 * 0.17).clamp(4.0, 11.0));
    let moon = th.accent2;
    let reach = radius as i32 + 1;
    for y in -reach..=reach {
        for x in -reach..=reach {
            let (fx, fy) = (x as f32, y as f32);
            let inside = fx * fx + fy * fy <= radius * radius;
            // a second disc, shifted up-left, bites out the dark side
            let (bx, by) = (fx + radius * 0.55, fy + radius * 0.25);
            let bitten = bx * bx + by * by <= (radius * 0.95).powi(2);
            if inside && !bitten {
                let shade = 1.0 - 0.18 * hash((x * 31 + y * 17 + 4000) as u32).round();
                c.set(cx as i32 + x, cy as i32 + y, c.tint(moon, shade));
            }
        }
    }
    let cloud = th.fg_dim;
    for i in 0..5u32 {
        let small = i % 2 == 1;
        let shape: &[&str] = if small { &CLOUD_SMALL } else { &CLOUD };
        let span = c.w as f32 + 40.0;
        let speed = 0.9 + hash(i + 20) * 1.4;
        let x = (hash(i) * span + t * speed) % span - 30.0;
        let y = (hash(i + 40) * c.h as f32 * 0.75) as i32;
        c.sprite(shape, x as i32, y, false, c.tint(cloud, if small { 0.22 } else { 0.32 }));
    }
}

const FISH: [&str; 5] = ["  ###   #", " #####  ##", "######### ", " #####  ##", "  ###   #"];
const FISH_SMALL: [&str; 3] = [" ##  #", "##### ", " ##  #"];

/// Ocean: fish crossing at different depths, rising bubbles, swaying weed.
fn ocean(c: &mut Canvas, th: &Theme, t: f32) {
    // seaweed along the floor
    for i in 0..(c.w / 9).max(1) as u32 {
        let base = (hash(i + 300) * c.w as f32) as i32;
        let tall = 4 + (hash(i + 310) * c.h as f32 * 0.22) as i32;
        for k in 0..tall {
            let sway = ((t * 1.1 + k as f32 * 0.45 + i as f32).sin() * (k as f32 / tall as f32) * 2.2) as i32;
            c.set(base + sway, c.h - 1 - k, c.tint(th.success, 0.22 + 0.25 * (k as f32 / tall as f32)));
        }
    }
    // bubbles
    for i in 0..(c.w / 6).max(4) as u32 {
        let speed = 3.0 + hash(i + 100) * 5.0;
        let y = c.h as f32 - (hash(i + 110) * c.h as f32 + t * speed) % c.h as f32;
        let x = hash(i + 120) * c.w as f32 + (t * 1.3 + i as f32).sin() * 1.5;
        c.set(x as i32, y as i32, c.tint(th.accent2, 0.35 + 0.3 * hash(i + 130)));
    }
    // fish
    let palette = &th.starfield;
    for i in 0..7u32 {
        let small = i % 3 != 0;
        let shape: &[&str] = if small { &FISH_SMALL } else { &FISH };
        let leftward = i % 2 == 0;
        let span = c.w as f32 + 30.0;
        let speed = (if small { 7.0 } else { 4.0 }) + hash(i + 200) * 4.0;
        let travelled = (hash(i + 210) * span + t * speed) % span;
        let x = if leftward { span - travelled - 15.0 } else { travelled - 15.0 };
        let y = 2.0 + hash(i + 220) * (c.h as f32 - 12.0) + (t * 0.8 + i as f32).sin() * 1.5;
        let color = palette[i as usize % palette.len().max(1)];
        // sprites are drawn facing left; flip the ones swimming right
        c.sprite(shape, x as i32, y as i32, !leftward, c.tint(color, if small { 0.55 } else { 0.8 }));
    }
}

/// Solar: a sun in the upper right whose rays turn and breathe.
fn sun(c: &mut Canvas, th: &Theme, t: f32) {
    // the rays reach 2.4 radii out, so that is the size that has to fit
    let (cx, cy, extent) = c.perch((c.h as f32 * 0.15).clamp(4.0, 10.0) * 2.4);
    let radius = extent / 2.4;
    let reach = (radius * 2.5) as i32 + 1;
    const RAYS: f32 = 12.0;
    for y in -reach..=reach {
        for x in -reach..=reach {
            let (fx, fy) = (x as f32, y as f32);
            let r = (fx * fx + fy * fy).sqrt() / radius;
            let pos = (cx as i32 + x, cy as i32 + y);
            if r <= 1.0 {
                c.set(pos.0, pos.1, lerp_color(th.accent2, th.accent, r * 0.8));
                continue;
            }
            // which ray this direction falls on, and how far from its centre line
            let turn = (fy.atan2(fx) + t * 0.18) / std::f32::consts::TAU * RAYS;
            let ray = turn.floor();
            let off = (turn - ray - 0.5).abs();
            // alternate long and short rays; each one pulses on its own beat
            let long = (ray as i32).rem_euclid(2) == 0;
            let len = (if long { 2.3 } else { 1.8 }) + 0.15 * (t * 1.4 + ray).sin();
            if r > 1.25 && r < len && off < 0.2 {
                let fade = 1.0 - (r - 1.25) / (len - 1.25);
                c.set(pos.0, pos.1, c.tint(th.accent, 0.35 + steps(fade, 4.0) * 0.55));
            }
        }
    }
}

/// Aurora: curtains of northern lights rippling over a mountain ridge.
fn aurora(c: &mut Canvas, th: &Theme, t: f32) {
    let sky = c.sky() as f32;
    let tail = (sky * 0.55).max(4.0);
    for k in 0..3 {
        let kf = k as f32;
        for x in 0..c.w {
            let fx = x as f32;
            // the bright lower hem of the curtain wanders up and down
            let hem = sky * (0.55 + 0.17 * kf)
                + (fx * 0.045 + t * 0.22 + kf * 2.1).sin() * sky * 0.14
                + (fx * 0.13 - t * 0.31 + kf).sin() * 1.4;
            // light runs along the curtain in slow waves
            let glow = 0.5 + 0.5 * (fx * 0.08 - t * 0.55 + kf * 1.7).sin();
            let hue = th.gradient_color((fx / c.w as f32 * 0.8 + kf * 0.28 + t * 0.008).fract());
            for d in 0..tail as i32 {
                let y = hem as i32 - d;
                if y >= sky as i32 {
                    continue;
                }
                let fade = (1.0 - d as f32 / tail).powf(1.6);
                let v = fade * (0.25 + 0.6 * glow);
                if v > 0.08 {
                    c.set(x, y, c.tint(hue, steps(v, 7.0)));
                }
            }
        }
    }
    // the ridge: taller towards the sides, where no text is in the way
    let band = (c.h - c.floor()) as f32;
    for x in 0..c.w {
        let fx = x as f32;
        let ridge = (0.5 + 0.26 * (fx * 0.07).sin() + 0.16 * (fx * 0.19 + 1.3).sin() + 0.08 * (fx * 0.43).sin())
            .clamp(0.12, 1.0);
        let height = (band * 0.9 * ridge * (1.0 + c.side_lift(x) * 1.1)) as i32;
        for k in 0..height {
            let snowy = ridge > 0.62 && k >= height - 2;
            let color = if snowy { c.tint(th.fg, 0.55) } else { c.tint(th.fg_dim, 0.2 + 0.12 * (k as f32 / height as f32)) };
            c.set(x, c.h - 1 - k, color);
        }
    }
}

/// Synthwave: a striped sunset over a neon grid rushing towards you.
fn retro(c: &mut Canvas, th: &Theme, t: f32) {
    let (cx, cy, radius) = c.perch((c.h as f32 * 0.22).clamp(5.0, 13.0));
    let reach = radius as i32;
    for y in -reach..=reach {
        for x in -reach..=reach {
            let (fx, fy) = (x as f32, y as f32);
            if fx * fx + fy * fy > radius * radius {
                continue;
            }
            // slits scroll down through the lower half and widen as they go
            let low = fy / radius;
            if low > 0.05 && ((fy + t * 1.2).rem_euclid(3.0)) < 0.6 + low * 1.4 {
                continue;
            }
            c.set(cx as i32 + x, cy as i32 + y, th.gradient_color((low + 1.0) / 2.0));
        }
    }
    let floor = c.floor();
    let depth = (c.h - floor).max(1) as f32;
    let mid = c.w as f32 / 2.0;
    for y in floor..c.h {
        let d = (y - floor + 1) as f32 / depth; // 0 at the horizon, 1 at your feet
        for x in 0..c.w {
            // lines fan out from the vanishing point: one pixel wide, further
            // apart the closer they are; cross lines roll towards the viewer
            let gap = 3.0 + d * 15.0;
            let across = ((x as f32 - mid) / gap + 0.5).rem_euclid(1.0) < 1.0 / gap;
            let along = (d.sqrt() * 3.0 - t * 0.7).rem_euclid(1.0) < 0.22;
            let color = if y == floor {
                c.tint(th.accent, 0.85)
            } else if across || along {
                c.tint(th.accent3, 0.22 + 0.55 * d)
            } else {
                c.tint(th.accent, 0.05) // solid ground hides the stars
            };
            c.set(x, y, color);
        }
    }
}

/// Downpour: rain over a skyline of lit windows, and the odd bolt of lightning.
fn city(c: &mut Canvas, th: &Theme, t: f32) {
    // rain first, so the buildings stand in front of it
    for i in 0..((c.w * c.h) / 55).max(12) as u32 {
        let speed = 26.0 + hash(i + 700) * 22.0;
        let y = (hash(i + 710) * c.h as f32 + t * speed) % c.h as f32;
        let x = (hash(i + 720) * c.w as f32 + y * 0.3) % c.w as f32;
        let strength = 0.18 + 0.28 * hash(i + 730);
        for k in 0..3 {
            let (px, py) = ((x - k as f32 * 0.3) as i32, y as i32 - k);
            c.set(px, py, c.tint(th.accent, strength * c.clearance(px, py) * (1.0 - k as f32 * 0.25)));
        }
    }
    // lightning: a jagged bolt down one side every so often
    let cycle = t / 9.0;
    if cycle.fract() < 0.035 {
        let n = cycle.floor() as u32;
        let free_left = if c.has_keep() { c.keep.0.max(6) } else { c.w / 3 };
        let mut x = if n.is_multiple_of(2) { hash(n + 900) * free_left as f32 } else { c.w as f32 - hash(n + 900) * free_left as f32 };
        for y in 0..c.floor() {
            x += (hash(n * 53 + y as u32) - 0.5) * 2.6;
            c.set(x as i32, y, c.tint(th.fg, 0.95));
        }
    }
    // skyline
    let band = (c.h - c.floor()) as f32;
    let (mut x, mut id) = (0i32, 0u32);
    while x < c.w {
        let width = 4 + (hash(id + 800) * 7.0) as i32;
        let lift = c.side_lift(x).min(c.side_lift(x + width - 1));
        let height = (band * (0.35 + 0.6 * hash(id + 810)) * (1.0 + lift * 1.3)) as i32;
        let wall = c.tint(th.fg_dim, 0.15 + 0.07 * (id % 3) as f32);
        for bx in 0..width - 1 {
            for by in 0..height {
                let window = bx % 2 == 1 && by % 3 == 1 && by < height - 1;
                // each window keeps its own hours
                let slot = (t / 5.0 + hash(id * 31 + bx as u32 * 7 + by as u32)).floor() as u32;
                let lit = window && hash(id * 131 + bx as u32 * 17 + by as u32 * 7 + slot * 3) > 0.5;
                c.set(x + bx, c.h - 1 - by, if lit { c.tint(th.accent2, 0.8) } else { wall });
            }
        }
        x += width;
        id += 1;
    }
}

/// Sakura: a blossoming branch, and petals drifting down on the breeze.
fn sakura(c: &mut Canvas, th: &Theme, t: f32) {
    let palette = &th.starfield;
    let pick = |i: u32| palette[i as usize % palette.len().max(1)];
    // petals
    for i in 0..(c.w / 3).max(10) as u32 {
        let speed = 2.2 + hash(i + 400) * 3.2;
        let y = (hash(i + 410) * c.h as f32 + t * speed) % c.h as f32;
        let x = (hash(i + 420) * c.w as f32 + t * 1.4 + (t * 0.9 + i as f32).sin() * 3.0).rem_euclid(c.w as f32);
        let (px, py) = (x as i32, y as i32);
        let color = c.tint(pick(i), (0.45 + 0.45 * hash(i + 430)) * c.clearance(px, py));
        c.set(px, py, color);
        // a petal seen flat-on is two pixels wide; it tumbles as it falls
        if (t * 1.7 + i as f32 * 2.3).sin() > 0.2 {
            c.set(px + 1, py, color);
        }
    }
    // fallen petals gathering along the bottom
    for x in 0..c.w {
        if hash(x as u32 + 450) > 0.55 {
            c.set(x, c.h - 1, c.tint(pick(x as u32), 0.4));
        }
    }
    // the branch reaches in from the top-left corner
    let len = ((if c.has_keep() { c.keep.0 - 3 } else { c.w / 3 }).min(c.w / 3)).max(8);
    let bark = c.tint(th.fg_dim, 0.6);
    for s in 0..len {
        let fs = s as f32;
        let y = 2.0 + fs * 0.24 + (fs * 0.16).sin() * 1.6 + (t * 0.5).sin() * (fs / len as f32) * 0.8;
        c.set(s, y as i32, bark);
        if s < len / 2 {
            c.set(s, y as i32 + 1, bark);
        }
        // twigs and blossom clusters every few pixels
        if s % 6 == 4 {
            let up = (s / 6) % 2 == 0;
            for k in 1..4 {
                c.set(s + k, y as i32 + if up { -k } else { k + 1 }, bark);
            }
            let (bx, by) = (s + 4, y as i32 + if up { -4 } else { 5 });
            let n = (s / 6) as u32;
            for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1), (2, 1), (-1, -1)] {
                c.set(bx + dx, by + dy, c.tint(pick(n + (dx + dy * 3 + 4) as u32), 0.9));
            }
            c.set(bx, by, c.tint(th.accent2, 1.0));
        }
    }
}

/// Phosphor: glowing code raining down an old green screen.
fn code(c: &mut Canvas, th: &Theme, t: f32) {
    let tick = (t * 7.0) as u32; // the "characters" change a few times a second
    for x in (0..c.w).step_by(2) {
        let col = x as u32;
        let speed = 6.0 + hash(col + 600) * 15.0;
        let trail = 6 + (hash(col + 610) * 12.0) as i32;
        let lap = c.h as f32 * (1.15 + hash(col + 620) * 0.5);
        let head = ((hash(col + 630) * lap + t * speed) % lap) as i32;
        for k in 0..trail {
            let y = head - k;
            // gaps make the streak read as separate glyphs
            if k > 0 && hash(col.wrapping_mul(977).wrapping_add((y as u32).wrapping_mul(13)).wrapping_add(tick)) < 0.3 {
                continue;
            }
            let fade = (1.0 - k as f32 / trail as f32).powf(1.3) * c.clearance(x, y);
            let color = if k == 0 { c.tint(th.fg, fade) } else { c.tint(th.accent, steps(fade * 0.75, 6.0)) };
            c.set(x, y, color);
        }
    }
}

/// Inferno: a bed of flames along the bottom, and sparks climbing off it.
fn embers(c: &mut Canvas, th: &Theme, t: f32) {
    let band = (c.h - c.floor()) as f32;
    for x in 0..c.w {
        let fx = x as f32;
        // three waves at different speeds make the tongues lick unevenly
        let flicker = (0.55
            + 0.25 * (fx * 0.21 + t * 3.1).sin()
            + 0.15 * (fx * 0.53 - t * 4.3).sin()
            + 0.10 * (fx * 0.11 + t * 1.7).sin())
        .clamp(0.15, 1.0);
        let height = (band * 0.9 * flicker * (1.0 + c.side_lift(x) * 1.2)) as i32;
        for k in 0..height {
            let up = k as f32 / height as f32; // 0 at the base, 1 at the tip
            c.set(x, c.h - 1 - k, c.tint(th.gradient_color(1.0 - up), 0.95 - 0.6 * up));
        }
    }
    for i in 0..(c.w / 2).max(12) as u32 {
        let speed = 5.0 + hash(i + 1000) * 9.0;
        let rise = (hash(i + 1010) * c.h as f32 + t * speed) % c.h as f32;
        let x = hash(i + 1020) * c.w as f32 + (t * 1.1 + i as f32).sin() * 2.5;
        let (px, py) = (x as i32, c.h - 1 - rise as i32);
        // a spark cools and dims the higher it gets
        let glow = (1.0 - rise / c.h as f32).powf(1.4) * c.clearance(px, py);
        c.set(px, py, c.tint(lerp_color(th.accent, th.accent2, hash(i + 1030)), steps(glow, 6.0)));
    }
}

/// Frostbite: snow falling past pines onto a soft white bank.
fn snow(c: &mut Canvas, th: &Theme, t: f32) {
    for i in 0..((c.w * c.h) / 40).max(16) as u32 {
        // near flakes are brighter and fall faster than far ones
        let near = i.is_multiple_of(3);
        let speed = (if near { 5.0 } else { 2.5 }) + hash(i + 1100) * 2.5;
        let y = (hash(i + 1110) * c.h as f32 + t * speed) % c.h as f32;
        let x = (hash(i + 1120) * c.w as f32 + (t * 0.6 + i as f32).sin() * 2.0 + t * 0.8).rem_euclid(c.w as f32);
        let (px, py) = (x as i32, y as i32);
        c.set(px, py, c.tint(th.fg, (if near { 0.85 } else { 0.4 }) * c.clearance(px, py)));
    }
    // pines stand to the sides, where no text is in the way
    let tall = (c.h as f32 * 0.32).clamp(7.0, 18.0) as i32;
    for i in 0..(c.w / 10).max(2) as u32 {
        let x = (hash(i + 1130) * c.w as f32) as i32;
        if c.has_keep() && c.side_lift(x) < 0.7 {
            continue;
        }
        let height = tall - (hash(i + 1140) * tall as f32 * 0.4) as i32;
        let top = c.h - 2 - height;
        for r in 0..height {
            let trunk = r >= height - 2;
            // tiers: each one starts narrow and flares out, wider than the last
            let half = if trunk { 0 } else { r % 4 + r / 4 };
            for dx in -half..=half {
                let snowy = !trunk && (r == 0 || (r % 4 == 3 && dx.abs() >= half - 1));
                let color = if snowy { c.tint(th.fg, 0.7) } else { c.tint(th.success, if trunk { 0.15 } else { 0.3 }) };
                c.set(x + dx, top + r, color);
            }
        }
    }
    for x in 0..c.w {
        let fx = x as f32;
        let bank = 2.5 + 1.5 * (fx * 0.09).sin() + 0.8 * (fx * 0.23 + 1.0).sin() + c.side_lift(x) * 2.0;
        for k in 0..bank.max(1.0) as i32 {
            c.set(x, c.h - 1 - k, c.tint(th.fg, 0.6 - 0.08 * k as f32));
        }
    }
}

/// Mirage: three ridges of dunes under a low sun, with sand on the wind.
fn dunes(c: &mut Canvas, th: &Theme, t: f32) {
    let (cx, cy, radius) = c.perch((c.h as f32 * 0.14).clamp(4.0, 9.0));
    let reach = radius as i32;
    for y in -reach..=reach {
        for x in -reach..=reach {
            let r = ((x * x + y * y) as f32).sqrt() / radius;
            if r <= 1.0 {
                c.set(cx as i32 + x, cy as i32 + y, lerp_color(th.accent2, th.accent, r * 0.7));
            }
        }
    }
    // back ridge first: the tallest and faintest; each nearer one is brighter
    let band = (c.h - c.floor()) as f32;
    for layer in 0..3 {
        let lf = layer as f32;
        for x in 0..c.w {
            let fx = x as f32;
            let ridge = (0.5 + 0.3 * (fx * (0.035 + 0.012 * lf) + lf * 2.3).sin() + 0.12 * (fx * 0.11 + lf).sin())
                .clamp(0.1, 1.0);
            let height = (band * (1.0 - 0.28 * lf) * ridge * (1.0 + c.side_lift(x) * (1.0 - 0.3 * lf))) as i32;
            for k in 0..height {
                let crest = if k == height - 1 { 0.12 } else { 0.0 };
                c.set(x, c.h - 1 - k, c.tint(th.gradient_color(lf / 2.0), 0.16 + 0.14 * lf + crest));
            }
        }
    }
    for i in 0..(c.w / 4).max(8) as u32 {
        let speed = 9.0 + hash(i + 1200) * 10.0;
        let x = (hash(i + 1210) * c.w as f32 + t * speed) % c.w as f32;
        let y = c.floor() as f32 - 6.0 + hash(i + 1220) * (band + 6.0) + (t * 1.3 + i as f32).sin() * 1.5;
        let (px, py) = (x as i32, y as i32);
        c.set(px, py, c.tint(th.accent2, 0.35 * c.clearance(px, py)));
    }
}

/// Grove: fireflies wandering and blinking over swaying grass.
fn fireflies(c: &mut Canvas, th: &Theme, t: f32) {
    for x in 0..c.w {
        let blade = 2 + (hash(x as u32 + 1300) * 4.0) as i32 + (c.side_lift(x) * 3.0) as i32;
        for k in 0..blade {
            let up = k as f32 / blade as f32;
            let sway = ((t * 0.9 + x as f32 * 0.3).sin() * up * 1.4) as i32;
            c.set(x + sway, c.h - 1 - k, c.tint(th.accent3, 0.18 + 0.2 * up));
        }
    }
    for i in 0..(c.w / 3).max(10) as u32 {
        let fi = i as f32;
        // each fly loops lazily around its own spot
        let x = hash(i + 1310) * c.w as f32
            + (t * (0.25 + hash(i + 1320) * 0.3) + fi).sin() * 7.0
            + (t * 0.9 + fi * 1.7).sin() * 1.5;
        let y = c.h as f32 * (0.2 + hash(i + 1330) * 0.7) + (t * (0.2 + hash(i + 1340) * 0.3) + fi * 2.1).cos() * 4.0;
        let (px, py) = (x as i32, y as i32);
        let near = c.clearance(px, py);
        // ...and blinks on its own beat, with a soft halo at full glow
        let pulse = 0.5 + 0.5 * (t * (0.8 + hash(i + 1350)) + fi * 3.0).sin();
        if pulse * near > 0.5 {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                c.set(px + dx, py + dy, c.tint(th.accent, steps(pulse * 0.35, 4.0)));
            }
        }
        c.set(px, py, c.tint(th.accent2, steps((0.3 + 0.7 * pulse) * near, 6.0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_paints_and_respects_the_text_block() {
        let area = Rect::new(0, 1, 120, 30);
        let keep = Rect::new(30, 9, 60, 12);
        for scene in [
            Scene::Nebula, Scene::Galaxy, Scene::Night, Scene::Ocean, Scene::Sun,
            Scene::Aurora, Scene::Retro, Scene::City, Scene::Sakura, Scene::Code,
            Scene::Embers, Scene::Snow, Scene::Dunes, Scene::Fireflies,
        ] {
            let mut theme = crate::theme::galaxy();
            theme.scene = scene;
            for t in [0.0f32, 3.3, 61.7] {
                let mut buf = Buffer::empty(Rect::new(0, 0, 120, 32));
                draw(&mut buf, area, keep, &theme, t);
                let painted = |r: Rect| {
                    r.positions().filter(|p| matches!(buf[*p].symbol(), "▀" | "▄")).count()
                };
                assert!(painted(area) > 20, "{scene:?} drew nothing at t={t}");
                // gas clouds are the one scene allowed behind the text
                if scene != Scene::Nebula {
                    assert_eq!(painted(keep), 0, "{scene:?} drew over the text at t={t}");
                }
            }
        }
        // odd and tiny sizes must not panic
        for (w, h) in [(40, 10), (41, 11), (200, 60), (10, 3)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
            for scene in [
                Scene::Ocean, Scene::Aurora, Scene::Retro, Scene::City, Scene::Sakura, Scene::Code,
                Scene::Embers, Scene::Snow, Scene::Dunes, Scene::Fireflies,
            ] {
                let mut theme = crate::theme::galaxy();
                theme.scene = scene;
                draw(&mut buf, Rect::new(0, 0, w, h), Rect::new(0, 0, 0, 0), &theme, 9.0);
                draw(&mut buf, Rect::new(0, 0, w, h), Rect::new(w / 4, h / 3, w / 2, h / 3), &theme, 0.02);
            }
        }
    }
}
