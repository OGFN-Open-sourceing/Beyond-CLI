//! Theming engine: 25 built-in themes (see `BUILTINS`), each with an
//! animated welcome scene, JSON custom themes
//! loaded from `<config>/themes/*.json` via `/loadtheme`, and the gradient
//! machinery that makes the wordmark shimmer like a galaxy.
//!
//! A `Theme` is plain data; UI code reads the palette fields directly and
//! uses `gradient_color[_animated]` for banners.

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::Deserialize;
use std::path::Path;

/// The animated pixel-art backdrop a theme shows on the welcome screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scene {
    /// Stars only.
    None,
    /// Faint drifting clouds of colored gas.
    Nebula,
    /// A slowly turning spiral galaxy and the odd shooting star.
    #[default]
    Galaxy,
    /// Crescent moon over passing clouds.
    Night,
    /// Fish, bubbles and seaweed.
    Ocean,
    /// A sun with turning rays.
    Sun,
    /// Northern lights over a mountain ridge.
    Aurora,
    /// Striped sunset over a neon grid.
    Retro,
    /// Rain on a skyline of lit windows.
    City,
    /// A blossoming branch and falling petals.
    Sakura,
    /// Green code rain.
    Code,
    /// Flames along the bottom and rising sparks.
    Embers,
    /// Snowfall over pines.
    Snow,
    /// Desert dunes under a low sun.
    Dunes,
    /// Fireflies over tall grass.
    Fireflies,
}

/// Full palette for every kind of text the UI renders.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Unique id: builtin name, or `custom:<file stem>` for JSON themes.
    pub id: String,
    /// Human-friendly display name.
    pub name: String,
    /// `None` keeps the terminal's own background (transparency).
    pub bg: Option<Color>,
    pub fg: Color,
    pub fg_dim: Color,
    pub accent: Color,
    pub accent2: Color,
    pub accent3: Color,
    /// Gradient stops `(position 0.0..=1.0, color)`; sorted, ≥ 2 entries.
    pub gradient: Vec<(f32, Color)>,
    /// Palette the splash starfield draws stars from.
    pub starfield: Vec<Color>,
    /// Chat roles and tool activity feed.
    pub user: Color,
    pub assistant: Color,
    pub system: Color,
    pub tool: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub border: Color,
    pub border_focus: Color,
    pub selection_bg: Color,
    /// Animated backdrop of the welcome screen.
    pub scene: Scene,
}

impl Theme {
    /// Style with this theme's background (no-op when `bg` is `None`).
    pub fn style_bg(&self) -> ratatui::style::Style {
        let mut s = ratatui::style::Style::new();
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        s
    }
    /// Static gradient color at t in 0..=1
    pub fn gradient_color(&self, t: f32) -> Color {
        gradient_at(&self.gradient, t)
    }

    /// Animated gradient: slow hue drift so the wordmark shimmers like a galaxy.
    pub fn gradient_color_animated(&self, t: f32, time: f32) -> Color {
        if self.gradient.len() < 2 {
            return self.accent;
        }
        let drift = (time * 0.35).sin() * 0.06; // subtle wave
        let tt = (t + drift).clamp(0.0, 1.0);
        gradient_at(&self.gradient, tt)
    }
}

/// Sample a multi-stop gradient at position `t` (clamped to 0..=1).
pub fn gradient_at(stops: &[(f32, Color)], t: f32) -> Color {
    if stops.is_empty() {
        return Color::White;
    }
    if stops.len() == 1 {
        return stops[0].1;
    }
    let t = t.clamp(0.0, 1.0);
    // find surrounding stops
    let mut a = &stops[0];
    let mut b = &stops[stops.len() - 1];
    for w in stops.windows(2) {
        if t >= w[0].0 && t <= w[1].0 {
            a = &w[0];
            b = &w[1];
            break;
        }
    }
    let span = (b.0 - a.0).max(1e-6);
    let f = ((t - a.0) / span).clamp(0.0, 1.0);
    lerp_color(a.1, b.1, f)
}

/// Linear interpolation between two colors (fraction `f` in 0..=1).
pub fn lerp_color(c1: Color, c2: Color, f: f32) -> Color {
    let (r1, g1, b1) = rgb_of(c1);
    let (r2, g2, b2) = rgb_of(c2);
    Color::Rgb(
        (r1 as f32 + (r2 as f32 - r1 as f32) * f) as u8,
        (g1 as f32 + (g2 as f32 - g1 as f32) * f) as u8,
        (b1 as f32 + (b2 as f32 - b1 as f32) * f) as u8,
    )
}

/// Best-effort RGB approximation of any ratatui color (for interpolation).
fn rgb_of(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Red => (220, 50, 50),
        Color::Green => (80, 220, 120),
        Color::Blue => (70, 110, 240),
        Color::Yellow => (240, 210, 80),
        Color::Magenta => (220, 90, 220),
        Color::Cyan => (80, 220, 230),
        Color::Gray => (170, 170, 170),
        Color::DarkGray => (70, 70, 70),
        Color::LightRed => (255, 120, 120),
        Color::LightGreen => (150, 255, 180),
        Color::LightBlue => (140, 180, 255),
        Color::LightYellow => (255, 240, 150),
        Color::LightMagenta => (255, 150, 255),
        Color::LightCyan => (150, 240, 255),
        Color::Indexed(_) => (128, 128, 128),
        Color::Reset => (200, 200, 200),
    }
}

/// Parse `#RRGGBB` (with or without `#`) into a truecolor `Color`.
pub fn parse_hex(s: &str) -> Result<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        anyhow::bail!("invalid color '{s}' (expected #RRGGBB)");
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok().context("bad red")?;
    let g = u8::from_str_radix(&s[2..4], 16).ok().context("bad green")?;
    let b = u8::from_str_radix(&s[4..6], 16).ok().context("bad blue")?;
    Ok(Color::Rgb(r, g, b))
}

// ---------------------------------------------------------------- built-ins

/// The default theme: violet/aqua nebula, terminal-transparent background.
pub fn beyond() -> Theme {
    Theme {
        id: "beyond".into(),
        name: "Beyond".into(),
        bg: None, // keep terminal transparency
        fg: Color::Rgb(226, 232, 240),
        fg_dim: Color::Rgb(124, 134, 156),
        accent: Color::Rgb(167, 139, 250),   // violet
        accent2: Color::Rgb(56, 232, 214),   // aqua
        accent3: Color::Rgb(244, 114, 182),  // pink
        gradient: vec![
            (0.00, Color::Rgb(124, 58, 237)),
            (0.35, Color::Rgb(217, 70, 239)),
            (0.65, Color::Rgb(96, 165, 250)),
            (1.00, Color::Rgb(45, 212, 191)),
        ],
        starfield: vec![
            Color::Rgb(196, 181, 253),
            Color::Rgb(129, 140, 248),
            Color::Rgb(45, 212, 191),
            Color::Rgb(244, 114, 182),
            Color::Rgb(226, 232, 240),
        ],
        user: Color::Rgb(167, 139, 250),
        assistant: Color::Rgb(226, 232, 240),
        system: Color::Rgb(124, 134, 156),
        tool: Color::Rgb(56, 232, 214),
        success: Color::Rgb(74, 222, 128),
        warning: Color::Rgb(250, 204, 21),
        error: Color::Rgb(248, 113, 113),
        border: Color::Rgb(91, 74, 155),
        border_focus: Color::Rgb(167, 139, 250),
        selection_bg: Color::Rgb(49, 46, 129),
        scene: Scene::Nebula,
    }
}

/// The second builtin: deep-space background with neon pink/blue accents.
pub fn galaxy() -> Theme {
    Theme {
        id: "galaxy".into(),
        name: "Galaxy".into(),
        bg: Some(Color::Rgb(10, 8, 26)),
        fg: Color::Rgb(232, 228, 246),
        fg_dim: Color::Rgb(130, 122, 168),
        accent: Color::Rgb(255, 121, 198),   // neon pink
        accent2: Color::Rgb(126, 194, 255),  // stardust blue
        accent3: Color::Rgb(192, 132, 252),  // lavender
        gradient: vec![
            (0.00, Color::Rgb(255, 94, 158)),
            (0.40, Color::Rgb(186, 100, 255)),
            (0.70, Color::Rgb(120, 130, 255)),
            (1.00, Color::Rgb(86, 207, 255)),
        ],
        starfield: vec![
            Color::Rgb(255, 182, 226),
            Color::Rgb(192, 132, 252),
            Color::Rgb(126, 194, 255),
            Color::Rgb(255, 240, 200),
            Color::Rgb(180, 255, 250),
        ],
        user: Color::Rgb(255, 121, 198),
        assistant: Color::Rgb(232, 228, 246),
        system: Color::Rgb(130, 122, 168),
        tool: Color::Rgb(126, 194, 255),
        success: Color::Rgb(110, 231, 183),
        warning: Color::Rgb(253, 224, 71),
        error: Color::Rgb(251, 113, 133),
        border: Color::Rgb(86, 56, 138),
        border_focus: Color::Rgb(255, 121, 198),
        selection_bg: Color::Rgb(88, 28, 135),
        scene: Scene::Galaxy,
    }
}

/// Midnight: cool blues under a crescent moon.
pub fn midnight() -> Theme {
    Theme {
        id: "midnight".into(),
        name: "Midnight".into(),
        bg: Some(Color::Rgb(7, 11, 28)),
        fg: Color::Rgb(222, 229, 246),
        fg_dim: Color::Rgb(120, 134, 172),
        accent: Color::Rgb(129, 161, 255),  // moonlit blue
        accent2: Color::Rgb(250, 226, 160), // pale gold
        accent3: Color::Rgb(180, 160, 255), // lavender
        gradient: vec![
            (0.00, Color::Rgb(72, 100, 230)),
            (0.50, Color::Rgb(150, 170, 255)),
            (1.00, Color::Rgb(250, 226, 160)),
        ],
        starfield: vec![
            Color::Rgb(255, 255, 255),
            Color::Rgb(170, 190, 255),
            Color::Rgb(250, 226, 160),
            Color::Rgb(200, 190, 255),
        ],
        user: Color::Rgb(129, 161, 255),
        tool: Color::Rgb(250, 226, 160),
        border: Color::Rgb(44, 58, 110),
        border_focus: Color::Rgb(129, 161, 255),
        selection_bg: Color::Rgb(30, 44, 100),
        scene: Scene::Night,
        ..galaxy()
    }
}

/// Ocean: deep teal water with fish going about their day.
pub fn ocean() -> Theme {
    Theme {
        id: "ocean".into(),
        name: "Ocean".into(),
        bg: Some(Color::Rgb(4, 20, 34)),
        fg: Color::Rgb(218, 240, 244),
        fg_dim: Color::Rgb(104, 148, 164),
        accent: Color::Rgb(56, 214, 222),   // aqua
        accent2: Color::Rgb(110, 176, 255), // surface blue
        accent3: Color::Rgb(255, 142, 120), // coral
        gradient: vec![
            (0.00, Color::Rgb(20, 150, 170)),
            (0.50, Color::Rgb(56, 214, 222)),
            (1.00, Color::Rgb(110, 176, 255)),
        ],
        starfield: vec![
            Color::Rgb(255, 142, 120),
            Color::Rgb(255, 208, 110),
            Color::Rgb(120, 220, 232),
            Color::Rgb(170, 190, 255),
            Color::Rgb(240, 246, 250),
        ],
        user: Color::Rgb(56, 214, 222),
        tool: Color::Rgb(110, 176, 255),
        success: Color::Rgb(94, 214, 150),
        border: Color::Rgb(24, 78, 98),
        border_focus: Color::Rgb(56, 214, 222),
        selection_bg: Color::Rgb(12, 62, 84),
        scene: Scene::Ocean,
        ..galaxy()
    }
}

/// Solar: warm dusk colors around a turning sun.
pub fn solar() -> Theme {
    Theme {
        id: "solar".into(),
        name: "Solar".into(),
        bg: Some(Color::Rgb(24, 13, 10)),
        fg: Color::Rgb(250, 236, 222),
        fg_dim: Color::Rgb(168, 130, 112),
        accent: Color::Rgb(255, 138, 76),   // ember
        accent2: Color::Rgb(255, 200, 104), // amber
        accent3: Color::Rgb(242, 104, 118), // rose
        gradient: vec![
            (0.00, Color::Rgb(232, 84, 72)),
            (0.50, Color::Rgb(255, 138, 76)),
            (1.00, Color::Rgb(255, 208, 116)),
        ],
        starfield: vec![
            Color::Rgb(255, 200, 104),
            Color::Rgb(255, 138, 76),
            Color::Rgb(250, 236, 222),
        ],
        user: Color::Rgb(255, 138, 76),
        tool: Color::Rgb(255, 200, 104),
        border: Color::Rgb(104, 52, 36),
        border_focus: Color::Rgb(255, 138, 76),
        selection_bg: Color::Rgb(92, 40, 28),
        scene: Scene::Sun,
        ..galaxy()
    }
}

/// Aurora: northern lights in green and violet over a cold ridge.
pub fn aurora() -> Theme {
    Theme {
        id: "aurora".into(),
        name: "Aurora".into(),
        bg: Some(Color::Rgb(5, 13, 22)),
        fg: Color::Rgb(224, 244, 240),
        fg_dim: Color::Rgb(108, 146, 152),
        accent: Color::Rgb(94, 234, 170),   // aurora green
        accent2: Color::Rgb(120, 200, 255), // ice blue
        accent3: Color::Rgb(186, 140, 255), // violet fringe
        gradient: vec![
            (0.00, Color::Rgb(64, 224, 150)),
            (0.45, Color::Rgb(70, 210, 220)),
            (0.75, Color::Rgb(120, 150, 255)),
            (1.00, Color::Rgb(196, 130, 255)),
        ],
        starfield: vec![
            Color::Rgb(255, 255, 255),
            Color::Rgb(170, 240, 220),
            Color::Rgb(170, 200, 255),
        ],
        user: Color::Rgb(94, 234, 170),
        tool: Color::Rgb(120, 200, 255),
        border: Color::Rgb(30, 80, 86),
        border_focus: Color::Rgb(94, 234, 170),
        selection_bg: Color::Rgb(14, 60, 66),
        scene: Scene::Aurora,
        ..galaxy()
    }
}

/// Synthwave: hot pink and cyan, a striped sun and an endless grid.
pub fn synthwave() -> Theme {
    Theme {
        id: "synthwave".into(),
        name: "Synthwave".into(),
        bg: Some(Color::Rgb(17, 6, 31)),
        fg: Color::Rgb(248, 232, 255),
        fg_dim: Color::Rgb(152, 114, 180),
        accent: Color::Rgb(255, 84, 170),   // hot pink
        accent2: Color::Rgb(255, 214, 90),  // sunset yellow
        accent3: Color::Rgb(84, 222, 255),  // grid cyan
        gradient: vec![
            (0.00, Color::Rgb(255, 220, 96)),
            (0.40, Color::Rgb(255, 128, 88)),
            (0.70, Color::Rgb(255, 84, 170)),
            (1.00, Color::Rgb(156, 84, 255)),
        ],
        starfield: vec![
            Color::Rgb(255, 255, 255),
            Color::Rgb(255, 170, 220),
            Color::Rgb(150, 230, 255),
        ],
        user: Color::Rgb(255, 84, 170),
        tool: Color::Rgb(84, 222, 255),
        border: Color::Rgb(94, 40, 130),
        border_focus: Color::Rgb(255, 84, 170),
        selection_bg: Color::Rgb(76, 22, 104),
        scene: Scene::Retro,
        ..galaxy()
    }
}

/// Downpour: a rainy night in the city, steel blue with amber windows.
pub fn downpour() -> Theme {
    Theme {
        id: "downpour".into(),
        name: "Downpour".into(),
        bg: Some(Color::Rgb(10, 13, 21)),
        fg: Color::Rgb(220, 228, 240),
        fg_dim: Color::Rgb(112, 124, 148),
        accent: Color::Rgb(122, 162, 247),  // rain blue
        accent2: Color::Rgb(255, 196, 112), // window amber
        accent3: Color::Rgb(187, 154, 247), // neon lavender
        gradient: vec![
            (0.00, Color::Rgb(96, 140, 230)),
            (0.55, Color::Rgb(170, 160, 250)),
            (1.00, Color::Rgb(255, 196, 112)),
        ],
        starfield: vec![Color::Rgb(160, 180, 220)],
        user: Color::Rgb(122, 162, 247),
        tool: Color::Rgb(255, 196, 112),
        border: Color::Rgb(46, 58, 92),
        border_focus: Color::Rgb(122, 162, 247),
        selection_bg: Color::Rgb(32, 44, 80),
        scene: Scene::City,
        ..galaxy()
    }
}

/// Sakura: soft pinks on plum, with petals in the air.
pub fn sakura() -> Theme {
    Theme {
        id: "sakura".into(),
        name: "Sakura".into(),
        bg: Some(Color::Rgb(25, 12, 22)),
        fg: Color::Rgb(252, 236, 242),
        fg_dim: Color::Rgb(172, 128, 150),
        accent: Color::Rgb(255, 150, 190),  // blossom
        accent2: Color::Rgb(255, 222, 170), // pollen
        accent3: Color::Rgb(176, 216, 150), // new leaf
        gradient: vec![
            (0.00, Color::Rgb(255, 120, 170)),
            (0.50, Color::Rgb(255, 176, 206)),
            (1.00, Color::Rgb(255, 232, 226)),
        ],
        starfield: vec![
            Color::Rgb(255, 150, 190),
            Color::Rgb(255, 190, 214),
            Color::Rgb(255, 226, 234),
            Color::Rgb(244, 120, 168),
        ],
        user: Color::Rgb(255, 150, 190),
        tool: Color::Rgb(176, 216, 150),
        border: Color::Rgb(110, 52, 82),
        border_focus: Color::Rgb(255, 150, 190),
        selection_bg: Color::Rgb(88, 34, 62),
        scene: Scene::Sakura,
        ..galaxy()
    }
}

/// Phosphor: an old green monitor with code raining down it.
pub fn phosphor() -> Theme {
    Theme {
        id: "phosphor".into(),
        name: "Phosphor".into(),
        bg: Some(Color::Rgb(3, 10, 6)),
        fg: Color::Rgb(204, 255, 216),
        fg_dim: Color::Rgb(84, 142, 102),
        accent: Color::Rgb(64, 255, 128),   // phosphor green
        accent2: Color::Rgb(170, 255, 190), // bright trace
        accent3: Color::Rgb(255, 196, 80),  // amber warning lamp
        gradient: vec![
            (0.00, Color::Rgb(24, 160, 80)),
            (0.60, Color::Rgb(64, 255, 128)),
            (1.00, Color::Rgb(200, 255, 214)),
        ],
        starfield: vec![Color::Rgb(64, 255, 128)],
        user: Color::Rgb(64, 255, 128),
        assistant: Color::Rgb(204, 255, 216),
        system: Color::Rgb(84, 142, 102),
        tool: Color::Rgb(170, 255, 190),
        success: Color::Rgb(64, 255, 128),
        border: Color::Rgb(22, 88, 48),
        border_focus: Color::Rgb(64, 255, 128),
        selection_bg: Color::Rgb(10, 62, 30),
        scene: Scene::Code,
        ..galaxy()
    }
}

/// Compact description of a theme: colors as `0xRRGGBB`. Everything a
/// palette does not name (success / warning / error) comes from Galaxy.
struct Palette {
    id: &'static str,
    name: &'static str,
    scene: Scene,
    bg: u32,
    fg: u32,
    dim: u32,
    /// Main accent (your messages, focused borders), second accent (tool
    /// activity) and a third for highlights.
    accents: [u32; 3],
    /// Wordmark gradient, left to right, evenly spaced.
    gradient: &'static [u32],
    /// Stars — and whatever the scene scatters: fish, petals, leaves.
    stars: &'static [u32],
    border: u32,
    selection: u32,
}

/// `0xRRGGBB` → truecolor.
fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    fn build(self) -> Theme {
        let [accent, accent2, accent3] = self.accents.map(rgb);
        let last = (self.gradient.len().max(2) - 1) as f32;
        Theme {
            id: self.id.into(),
            name: self.name.into(),
            bg: Some(rgb(self.bg)),
            fg: rgb(self.fg),
            fg_dim: rgb(self.dim),
            accent,
            accent2,
            accent3,
            gradient: self.gradient.iter().enumerate().map(|(i, c)| (i as f32 / last, rgb(*c))).collect(),
            starfield: self.stars.iter().copied().map(rgb).collect(),
            user: accent,
            assistant: rgb(self.fg),
            system: rgb(self.dim),
            tool: accent2,
            border: rgb(self.border),
            border_focus: accent,
            selection_bg: rgb(self.selection),
            scene: self.scene,
            ..galaxy()
        }
    }
}

/// Inferno: a bed of flames with sparks climbing off it.
pub fn inferno() -> Theme {
    Palette {
        id: "inferno",
        name: "Inferno",
        scene: Scene::Embers,
        bg: 0x140705,
        fg: 0xFFE9DC,
        dim: 0xA06A58,
        accents: [0xFF5A1F, 0xFFC247, 0xFF2E4D],
        gradient: &[0xC81E1E, 0xFF5A1F, 0xFFA02E, 0xFFE66B],
        stars: &[0xFFC247, 0xFF8A3C, 0xFFF1C2],
        border: 0x6B2414,
        selection: 0x5A1A0E,
    }
    .build()
}

/// Frostbite: snowfall over pines, all ice blue and white.
pub fn frostbite() -> Theme {
    Palette {
        id: "frostbite",
        name: "Frostbite",
        scene: Scene::Snow,
        bg: 0x08121C,
        fg: 0xEAF6FF,
        dim: 0x7E9BB2,
        accents: [0x8FD8FF, 0xE6F4FF, 0xB6A8FF],
        gradient: &[0x5AB0F0, 0xA6E3FF, 0xFFFFFF],
        stars: &[0xFFFFFF, 0xCFEAFF, 0xA6D8FF],
        border: 0x2B4C66,
        selection: 0x183650,
    }
    .build()
}

/// Mirage: desert dusk — rolling dunes and sand on the wind.
pub fn mirage() -> Theme {
    Palette {
        id: "mirage",
        name: "Mirage",
        scene: Scene::Dunes,
        bg: 0x1A0F14,
        fg: 0xFBEBD8,
        dim: 0xA88470,
        accents: [0xF2A65A, 0xFFD98A, 0xE0607E],
        gradient: &[0xE0607E, 0xF2A65A, 0xFFD98A],
        stars: &[0xFFFFFF, 0xFFD98A, 0xF7B7A3],
        border: 0x6E3F35,
        selection: 0x54292A,
    }
    .build()
}

/// Grove: fireflies blinking over tall grass.
pub fn grove() -> Theme {
    Palette {
        id: "grove",
        name: "Grove",
        scene: Scene::Fireflies,
        bg: 0x060F0A,
        fg: 0xE4F2DC,
        dim: 0x7A9478,
        accents: [0xC8F25A, 0xFFE27A, 0x6FD6A0],
        gradient: &[0x3FA36B, 0x9BD95A, 0xF4F08A],
        stars: &[0xF4F08A, 0xC8F25A, 0xFFFFFF],
        border: 0x27543A,
        selection: 0x173D28,
    }
    .build()
}

/// Bloodmoon: Midnight's sky turned crimson.
pub fn bloodmoon() -> Theme {
    Palette {
        id: "bloodmoon",
        name: "Bloodmoon",
        scene: Scene::Night,
        bg: 0x0F0508,
        fg: 0xF6E4E4,
        dim: 0x966A70,
        accents: [0xFF4D5E, 0xFF8A5C, 0xC77DFF],
        gradient: &[0x8E1023, 0xFF4D5E, 0xFFB08A],
        stars: &[0xFFFFFF, 0xFFB3B3, 0xFF7A7A],
        border: 0x5E1A26,
        selection: 0x4A101C,
    }
    .build()
}

/// Abyss: the deep sea, lit only by glowing fish.
pub fn abyss() -> Theme {
    Palette {
        id: "abyss",
        name: "Abyss",
        scene: Scene::Ocean,
        bg: 0x03040F,
        fg: 0xDDE4FF,
        dim: 0x6C74A8,
        accents: [0x7C6CFF, 0x35F0C0, 0xFF5CD6],
        gradient: &[0x4B3BFF, 0x7C6CFF, 0x35F0C0],
        stars: &[0x35F0C0, 0xFF5CD6, 0x8FA2FF, 0xF6FF7A],
        border: 0x23286A,
        selection: 0x191C55,
    }
    .build()
}

/// Supernova: a galaxy burning white-hot gold.
pub fn supernova() -> Theme {
    Palette {
        id: "supernova",
        name: "Supernova",
        scene: Scene::Galaxy,
        bg: 0x0D0906,
        fg: 0xFFF3E0,
        dim: 0xA89074,
        accents: [0xFFB02E, 0xFFF0B8, 0xFF6B4A],
        gradient: &[0xD43A2A, 0xFF8A2E, 0xFFD45E, 0xFFFFFF],
        stars: &[0xFFFFFF, 0xFFE0A0, 0xFFB07A],
        border: 0x6A4A1E,
        selection: 0x4E3312,
    }
    .build()
}

/// Cotton Candy: pastel pink and blue clouds.
pub fn candy() -> Theme {
    Palette {
        id: "candy",
        name: "Cotton Candy",
        scene: Scene::Nebula,
        bg: 0x150F1F,
        fg: 0xFDF2FF,
        dim: 0xA694B8,
        accents: [0xFF9EDB, 0x9EDCFF, 0xC9A8FF],
        gradient: &[0xFF9EDB, 0xC9A8FF, 0x9EDCFF, 0xA8FFE6],
        stars: &[0xFF9EDB, 0x9EDCFF, 0xC9A8FF, 0xFFFFFF],
        border: 0x5A4578,
        selection: 0x44305E,
    }
    .build()
}

/// Noir: black, white and nothing but stars.
pub fn noir() -> Theme {
    Palette {
        id: "noir",
        name: "Noir",
        scene: Scene::None,
        bg: 0x0A0A0A,
        fg: 0xEDEDED,
        dim: 0x7A7A7A,
        accents: [0xFFFFFF, 0xBDBDBD, 0xE5484D],
        gradient: &[0x7A7A7A, 0xFFFFFF, 0x9A9A9A],
        stars: &[0xFFFFFF, 0x9A9A9A, 0x5A5A5A],
        border: 0x3A3A3A,
        selection: 0x2A2A2A,
    }
    .build()
}

/// Glacier: Aurora's ridge under cold blue light.
pub fn glacier() -> Theme {
    Palette {
        id: "glacier",
        name: "Glacier",
        scene: Scene::Aurora,
        bg: 0x050B14,
        fg: 0xEAF4FF,
        dim: 0x7890AA,
        accents: [0x6FE3FF, 0xB8C8FF, 0xDDF6FF],
        gradient: &[0x3C8CFF, 0x6FE3FF, 0xE8FBFF],
        stars: &[0xFFFFFF, 0xBFE4FF, 0x8FB8FF],
        border: 0x24476E,
        selection: 0x143258,
    }
    .build()
}

/// Toxic: acid-lime code rain on purple.
pub fn toxic() -> Theme {
    Palette {
        id: "toxic",
        name: "Toxic",
        scene: Scene::Code,
        bg: 0x07060D,
        fg: 0xF1FFD6,
        dim: 0x7F8A6A,
        accents: [0xB6FF2E, 0xC06BFF, 0xFF4FD8],
        gradient: &[0x7A2EFF, 0xC06BFF, 0xB6FF2E],
        stars: &[0xB6FF2E],
        border: 0x3F2A6A,
        selection: 0x2C1B52,
    }
    .build()
}

/// Miami: Synthwave's grid in teal and coral.
pub fn miami() -> Theme {
    Palette {
        id: "miami",
        name: "Miami",
        scene: Scene::Retro,
        bg: 0x06131A,
        fg: 0xEAFDFF,
        dim: 0x6FA0A8,
        accents: [0x2EE6D6, 0xFF9A6B, 0xFF5FA2],
        gradient: &[0xFFD27A, 0xFF8A7A, 0xFF5FA2, 0x2EE6D6],
        stars: &[0xFFFFFF, 0xA8FFF4, 0xFFC2DC],
        border: 0x1F5560,
        selection: 0x123E48,
    }
    .build()
}

/// Neon District: magenta rain and cyan windows.
pub fn neon() -> Theme {
    Palette {
        id: "neon",
        name: "Neon District",
        scene: Scene::City,
        bg: 0x0B0514,
        fg: 0xF4E9FF,
        dim: 0x8C76A8,
        accents: [0xFF3DCB, 0x3DF5FF, 0xFFE14D],
        gradient: &[0xFF3DCB, 0x9B5CFF, 0x3DF5FF],
        stars: &[0xFF9BE6],
        border: 0x4A2470,
        selection: 0x351556,
    }
    .build()
}

/// Maple: Sakura's branch in autumn, shedding orange leaves.
pub fn maple() -> Theme {
    Palette {
        id: "maple",
        name: "Maple",
        scene: Scene::Sakura,
        bg: 0x170C06,
        fg: 0xFBEBD9,
        dim: 0xA88566,
        accents: [0xFF8A3C, 0xFFD166, 0xD9482B],
        gradient: &[0xD9482B, 0xFF8A3C, 0xFFD166],
        stars: &[0xFF8A3C, 0xD9482B, 0xFFC247, 0xB5651D],
        border: 0x6B3A1C,
        selection: 0x522A12,
    }
    .build()
}

/// Goldrush: Solar's sun in pure gold.
pub fn goldrush() -> Theme {
    Palette {
        id: "goldrush",
        name: "Goldrush",
        scene: Scene::Sun,
        bg: 0x120D04,
        fg: 0xFFF6DC,
        dim: 0xA8946A,
        accents: [0xFFC83D, 0xFFF1B0, 0xFF8E3C],
        gradient: &[0xB8860B, 0xFFC83D, 0xFFF1B0],
        stars: &[0xFFF1B0, 0xFFC83D, 0xFFFFFF],
        border: 0x6B5216,
        selection: 0x4E3B0E,
    }
    .build()
}

/// Every built-in theme, in picker order.
const BUILTINS: [fn() -> Theme; 25] = [
    beyond, galaxy, midnight, ocean, solar, aurora, synthwave, downpour, sakura, phosphor,
    inferno, frostbite, mirage, grove, bloodmoon, abyss, supernova, candy, noir, glacier,
    toxic, miami, neon, maple, goldrush,
];

/// Look up a built-in theme by id.
pub fn builtin_by_id(id: &str) -> Option<Theme> {
    BUILTINS.iter().map(|make| make()).find(|t| t.id == id)
}

// ------------------------------------------------------------ custom theme

/// `serde` view of a user JSON theme file. Every field is optional; anything
/// omitted is inherited from the Galaxy base so theme files stay tiny:
///
/// ```json
/// { "name": "Aurora", "accent": "#7c3aed", "gradient": ["#7c3aed", "#22d3ee"] }
/// ```
#[derive(Debug, Deserialize)]
struct JsonTheme {
    name: Option<String>,
    bg: Option<String>,
    fg: Option<String>,
    fg_dim: Option<String>,
    accent: Option<String>,
    accent2: Option<String>,
    accent3: Option<String>,
    gradient: Option<Vec<String>>,
    starfield: Option<Vec<String>>,
    user: Option<String>,
    assistant: Option<String>,
    system: Option<String>,
    tool: Option<String>,
    success: Option<String>,
    warning: Option<String>,
    error: Option<String>,
    border: Option<String>,
    border_focus: Option<String>,
    selection_bg: Option<String>,
    /// Welcome-screen backdrop: none, nebula, galaxy, night, ocean, sun,
    /// aurora, retro, city, sakura, code, embers, snow, dunes or fireflies.
    scene: Option<Scene>,
}

/// Load a custom theme JSON, merged over the Galaxy base.
/// The resulting id is `custom:<file stem>` so it round-trips via `resolve`.
pub fn load_json_theme(path: &Path) -> Result<Theme> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading theme {}", path.display()))?;
    let json: JsonTheme =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    let base = galaxy();
    let mut t = base.clone();
    t.id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase().replace(' ', "-"))
        .unwrap_or_else(|| "custom".into());
    t.name = json.name.clone().unwrap_or_else(|| {
        path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    });
    t.id = format!("custom:{}", t.id);
    if let Some(s) = &json.bg {
        t.bg = if s.eq_ignore_ascii_case("default") { None } else { Some(parse_hex(s)?) };
    }
    if let Some(s) = &json.fg {
        t.fg = parse_hex(s)?;
    }
    if let Some(s) = &json.fg_dim {
        t.fg_dim = parse_hex(s)?;
    }
    if let Some(s) = &json.accent {
        t.accent = parse_hex(s)?;
    }
    if let Some(s) = &json.accent2 {
        t.accent2 = parse_hex(s)?;
    }
    if let Some(s) = &json.accent3 {
        t.accent3 = parse_hex(s)?;
    }
    if let Some(cols) = &json.gradient {
        let mut stops = Vec::new();
        let n = cols.len().max(1) as f32;
        for (i, c) in cols.iter().enumerate() {
            stops.push((i as f32 / (n - 1.0).max(1.0), parse_hex(c)?));
        }
        t.gradient = stops;
    }
    if let Some(cols) = &json.starfield {
        t.starfield = cols.iter().map(|c| parse_hex(c)).collect::<Result<Vec<_>>>()?;
    }
    if let Some(s) = &json.user {
        t.user = parse_hex(s)?;
    }
    if let Some(s) = &json.assistant {
        t.assistant = parse_hex(s)?;
    }
    if let Some(s) = &json.system {
        t.system = parse_hex(s)?;
    }
    if let Some(s) = &json.tool {
        t.tool = parse_hex(s)?;
    }
    if let Some(s) = &json.success {
        t.success = parse_hex(s)?;
    }
    if let Some(s) = &json.warning {
        t.warning = parse_hex(s)?;
    }
    if let Some(s) = &json.error {
        t.error = parse_hex(s)?;
    }
    if let Some(s) = &json.border {
        t.border = parse_hex(s)?;
    }
    if let Some(s) = &json.border_focus {
        t.border_focus = parse_hex(s)?;
    }
    if let Some(s) = &json.selection_bg {
        t.selection_bg = parse_hex(s)?;
    }
    if let Some(scene) = json.scene {
        t.scene = scene;
    }
    Ok(t)
}

/// Resolve a theme id -> Theme (built-in or custom themes dir / absolute path).
///
/// Accepts: builtin ids, `custom:<stem>` ids (from the picker), plain stems
/// looked up in `<config>/themes/`, or a direct path to a `.json` file.
pub fn resolve(id: &str) -> Result<Theme> {
    if let Some(t) = builtin_by_id(id) {
        return Ok(t);
    }
    let stem = id.strip_prefix("custom:").unwrap_or(id);
    let file = crate::config::Paths::custom_themes_dir().join(format!("{stem}.json"));
    if file.exists() {
        return load_json_theme(&file);
    }
    let p = Path::new(id);
    if p.exists() && p.extension().map(|e| e == "json").unwrap_or(false) {
        return load_json_theme(p);
    }
    anyhow::bail!("unknown theme '{id}' — use /theme to pick one, or /loadtheme <file.json>")
}

/// (id, display name) list for the `/theme` picker: builtins + customs.
pub fn available_themes() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = BUILTINS
        .iter()
        .map(|make| {
            let t = make();
            (t.id, t.name)
        })
        .collect();
    if let Ok(dir) = std::fs::read_dir(crate::config::Paths::custom_themes_dir()) {
        for e in dir.flatten() {
            let p = e.path();
            if p.extension().map(|x| x == "json").unwrap_or(false)
                && let Some(stem) = p.file_stem() {
                    out.push((
                        format!("custom:{}", stem.to_string_lossy().to_lowercase()),
                        format!("{} (custom)", stem.to_string_lossy()),
                    ));
                }
        }
    }
    out
}

// ---------------------------------------------------------------- wordmark

/// ANSI-shadow style BEYOND wordmark, 6 rows x 52 cols, painted with the
/// theme gradient by the splash screen and installer header.
pub const WORDMARK: [&str; 6] = [
    "██████╗ ███████╗██╗   ██╗ ██████╗ ███╗   ██╗██████╗ ",
    "██╔══██╗██╔════╝╚██╗ ██╔╝██╔═══██╗████╗  ██║██╔══██╗",
    "██████╔╝█████╗   ╚████╔╝ ██║   ██║██╔██╗ ██║██║  ██║",
    "██╔══██╗██╔══╝    ╚██╔╝  ██║   ██║██║╚██╗██║██║  ██║",
    "██████╔╝███████╗   ██║   ╚██████╔╝██║ ╚████║██████╔╝",
    "╚═════╝ ╚══════╝   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝╚═════╝ ",
];

/// Display width of one `WORDMARK` row (for centering math).
pub const WORDMARK_W: usize = 52;
