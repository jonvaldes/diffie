//! Catppuccin palette + imgui style application.
//!
//! Two flavors are supported at runtime: **Macchiato** (dark, default) and
//! **Latte** (light). Palette values from <https://catppuccin.com/palette/>.
//!
//! Rendering code reads colors through the lowercase accessor functions
//! (`theme::base()`, `theme::blue()`, …) which dispatch to the active
//! flavor. `apply(ctx)` writes the active flavor's colors into imgui's
//! style table; call it after `set_flavor` to live-switch themes.

#![allow(dead_code, non_snake_case)]

use std::sync::atomic::{AtomicU8, Ordering};

use imgui::{Context, StyleColor};
use serde::{Deserialize, Serialize};

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

/// One Catppuccin flavor's 26 named colors, all opaque RGBA in 0..=1.
pub struct Palette {
    pub rosewater: [f32; 4],
    pub flamingo: [f32; 4],
    pub pink: [f32; 4],
    pub mauve: [f32; 4],
    pub red: [f32; 4],
    pub maroon: [f32; 4],
    pub peach: [f32; 4],
    pub yellow: [f32; 4],
    pub green: [f32; 4],
    pub teal: [f32; 4],
    pub sky: [f32; 4],
    pub sapphire: [f32; 4],
    pub blue: [f32; 4],
    pub lavender: [f32; 4],
    pub text: [f32; 4],
    pub subtext1: [f32; 4],
    pub subtext0: [f32; 4],
    pub overlay2: [f32; 4],
    pub overlay1: [f32; 4],
    pub overlay0: [f32; 4],
    pub surface2: [f32; 4],
    pub surface1: [f32; 4],
    pub surface0: [f32; 4],
    pub base: [f32; 4],
    pub mantle: [f32; 4],
    pub crust: [f32; 4],
}

pub const MACCHIATO: Palette = Palette {
    rosewater: rgb(0xf4, 0xdb, 0xd6),
    flamingo: rgb(0xf0, 0xc6, 0xc6),
    pink: rgb(0xf5, 0xbd, 0xe6),
    mauve: rgb(0xc6, 0xa0, 0xf6),
    red: rgb(0xed, 0x87, 0x96),
    maroon: rgb(0xee, 0x99, 0xa0),
    peach: rgb(0xf5, 0xa9, 0x7f),
    yellow: rgb(0xee, 0xd4, 0x9f),
    green: rgb(0xa6, 0xda, 0x95),
    teal: rgb(0x8b, 0xd5, 0xca),
    sky: rgb(0x91, 0xd7, 0xe3),
    sapphire: rgb(0x7d, 0xc4, 0xe4),
    blue: rgb(0x8a, 0xad, 0xf4),
    lavender: rgb(0xb7, 0xbd, 0xf8),
    text: rgb(0xca, 0xd3, 0xf5),
    subtext1: rgb(0xb8, 0xc0, 0xe0),
    subtext0: rgb(0xa5, 0xad, 0xcb),
    overlay2: rgb(0x93, 0x9a, 0xb7),
    overlay1: rgb(0x80, 0x87, 0xa2),
    overlay0: rgb(0x6e, 0x73, 0x8d),
    surface2: rgb(0x5b, 0x60, 0x78),
    surface1: rgb(0x49, 0x4d, 0x64),
    surface0: rgb(0x36, 0x3a, 0x4f),
    base: rgb(0x24, 0x27, 0x3a),
    mantle: rgb(0x1e, 0x20, 0x30),
    crust: rgb(0x18, 0x19, 0x26),
};

pub const LATTE: Palette = Palette {
    rosewater: rgb(0xdc, 0x8a, 0x78),
    flamingo: rgb(0xdd, 0x78, 0x78),
    pink: rgb(0xea, 0x76, 0xcb),
    mauve: rgb(0x88, 0x39, 0xef),
    red: rgb(0xd2, 0x0f, 0x39),
    maroon: rgb(0xe6, 0x45, 0x53),
    peach: rgb(0xfe, 0x64, 0x0b),
    yellow: rgb(0xdf, 0x8e, 0x1d),
    green: rgb(0x40, 0xa0, 0x2b),
    teal: rgb(0x17, 0x92, 0x99),
    sky: rgb(0x04, 0xa5, 0xe5),
    sapphire: rgb(0x20, 0x9f, 0xb5),
    blue: rgb(0x1e, 0x66, 0xf5),
    lavender: rgb(0x72, 0x87, 0xfd),
    text: rgb(0x4c, 0x4f, 0x69),
    subtext1: rgb(0x5c, 0x5f, 0x77),
    subtext0: rgb(0x6c, 0x6f, 0x85),
    overlay2: rgb(0x7c, 0x7f, 0x93),
    overlay1: rgb(0x8c, 0x8f, 0xa1),
    overlay0: rgb(0x9c, 0xa0, 0xb0),
    surface2: rgb(0xac, 0xb0, 0xbe),
    surface1: rgb(0xbc, 0xc0, 0xcc),
    surface0: rgb(0xcc, 0xd0, 0xda),
    base: rgb(0xef, 0xf1, 0xf5),
    mantle: rgb(0xe6, 0xe9, 0xef),
    crust: rgb(0xdc, 0xe0, 0xe8),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flavor {
    Macchiato,
    Latte,
}

impl Default for Flavor {
    fn default() -> Self {
        Flavor::Macchiato
    }
}

impl Flavor {
    pub fn label(self) -> &'static str {
        match self {
            Flavor::Macchiato => "Catppuccin Macchiato (dark)",
            Flavor::Latte => "Catppuccin Latte (light)",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_flavor(f: Flavor) {
    let v = match f {
        Flavor::Macchiato => 0,
        Flavor::Latte => 1,
    };
    CURRENT.store(v, Ordering::Relaxed);
}

pub fn current_flavor() -> Flavor {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Flavor::Latte,
        _ => Flavor::Macchiato,
    }
}

pub fn current() -> &'static Palette {
    match current_flavor() {
        Flavor::Macchiato => &MACCHIATO,
        Flavor::Latte => &LATTE,
    }
}

// Backwards-compatible color accessors. The all-caps names mirror the
// pre-refactor `pub const` exports — call sites only need parens added.
#[inline] pub fn ROSEWATER() -> [f32; 4] { current().rosewater }
#[inline] pub fn FLAMINGO() -> [f32; 4] { current().flamingo }
#[inline] pub fn PINK() -> [f32; 4] { current().pink }
#[inline] pub fn MAUVE() -> [f32; 4] { current().mauve }
#[inline] pub fn RED() -> [f32; 4] { current().red }
#[inline] pub fn MAROON() -> [f32; 4] { current().maroon }
#[inline] pub fn PEACH() -> [f32; 4] { current().peach }
#[inline] pub fn YELLOW() -> [f32; 4] { current().yellow }
#[inline] pub fn GREEN() -> [f32; 4] { current().green }
#[inline] pub fn TEAL() -> [f32; 4] { current().teal }
#[inline] pub fn SKY() -> [f32; 4] { current().sky }
#[inline] pub fn SAPPHIRE() -> [f32; 4] { current().sapphire }
#[inline] pub fn BLUE() -> [f32; 4] { current().blue }
#[inline] pub fn LAVENDER() -> [f32; 4] { current().lavender }
#[inline] pub fn TEXT() -> [f32; 4] { current().text }
#[inline] pub fn SUBTEXT1() -> [f32; 4] { current().subtext1 }
#[inline] pub fn SUBTEXT0() -> [f32; 4] { current().subtext0 }
#[inline] pub fn OVERLAY2() -> [f32; 4] { current().overlay2 }
#[inline] pub fn OVERLAY1() -> [f32; 4] { current().overlay1 }
#[inline] pub fn OVERLAY0() -> [f32; 4] { current().overlay0 }
#[inline] pub fn SURFACE2() -> [f32; 4] { current().surface2 }
#[inline] pub fn SURFACE1() -> [f32; 4] { current().surface1 }
#[inline] pub fn SURFACE0() -> [f32; 4] { current().surface0 }
#[inline] pub fn BASE() -> [f32; 4] { current().base }
#[inline] pub fn MANTLE() -> [f32; 4] { current().mantle }
#[inline] pub fn CRUST() -> [f32; 4] { current().crust }

/// Returns `c` with its alpha replaced by `a`.
pub const fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

/// Apply the current Catppuccin flavor across imgui's StyleColor table.
/// Call after `set_flavor` to make a live theme switch visible.
pub fn apply(ctx: &mut Context) {
    let p = current();
    let style = ctx.style_mut();

    style.colors[StyleColor::Text as usize] = p.text;
    style.colors[StyleColor::TextDisabled as usize] = p.overlay1;

    style.colors[StyleColor::WindowBg as usize] = p.base;
    style.colors[StyleColor::ChildBg as usize] = p.base;
    style.colors[StyleColor::PopupBg as usize] = p.mantle;

    style.colors[StyleColor::Border as usize] = p.surface1;
    style.colors[StyleColor::BorderShadow as usize] = [0.0, 0.0, 0.0, 0.0];

    style.colors[StyleColor::FrameBg as usize] = p.surface0;
    style.colors[StyleColor::FrameBgHovered as usize] = p.surface1;
    style.colors[StyleColor::FrameBgActive as usize] = p.surface2;

    style.colors[StyleColor::TitleBg as usize] = p.mantle;
    style.colors[StyleColor::TitleBgActive as usize] = p.surface0;
    style.colors[StyleColor::TitleBgCollapsed as usize] = p.mantle;
    style.colors[StyleColor::MenuBarBg as usize] = p.mantle;

    style.colors[StyleColor::ScrollbarBg as usize] = p.mantle;
    style.colors[StyleColor::ScrollbarGrab as usize] = p.surface1;
    style.colors[StyleColor::ScrollbarGrabHovered as usize] = p.surface2;
    style.colors[StyleColor::ScrollbarGrabActive as usize] = p.overlay0;

    style.colors[StyleColor::CheckMark as usize] = p.lavender;
    style.colors[StyleColor::SliderGrab as usize] = p.blue;
    style.colors[StyleColor::SliderGrabActive as usize] = p.lavender;

    style.colors[StyleColor::Button as usize] = p.surface0;
    style.colors[StyleColor::ButtonHovered as usize] = p.surface1;
    style.colors[StyleColor::ButtonActive as usize] = p.surface2;

    style.colors[StyleColor::Header as usize] = p.surface0;
    style.colors[StyleColor::HeaderHovered as usize] = p.surface1;
    style.colors[StyleColor::HeaderActive as usize] = p.surface2;

    style.colors[StyleColor::Separator as usize] = p.surface1;
    style.colors[StyleColor::SeparatorHovered as usize] = p.surface2;
    style.colors[StyleColor::SeparatorActive as usize] = p.overlay0;

    style.colors[StyleColor::ResizeGrip as usize] = p.surface1;
    style.colors[StyleColor::ResizeGripHovered as usize] = p.surface2;
    style.colors[StyleColor::ResizeGripActive as usize] = p.overlay0;

    style.colors[StyleColor::Tab as usize] = p.surface0;
    style.colors[StyleColor::TabHovered as usize] = p.surface2;
    style.colors[StyleColor::TabActive as usize] = p.surface1;
    style.colors[StyleColor::TabUnfocused as usize] = p.mantle;
    style.colors[StyleColor::TabUnfocusedActive as usize] = p.surface0;

    style.colors[StyleColor::PlotLines as usize] = p.subtext0;
    style.colors[StyleColor::PlotLinesHovered as usize] = p.lavender;
    style.colors[StyleColor::PlotHistogram as usize] = p.lavender;
    style.colors[StyleColor::PlotHistogramHovered as usize] = p.pink;

    style.colors[StyleColor::TableHeaderBg as usize] = p.mantle;
    style.colors[StyleColor::TableBorderStrong as usize] = p.surface1;
    style.colors[StyleColor::TableBorderLight as usize] = p.surface0;
    style.colors[StyleColor::TableRowBg as usize] = [0.0, 0.0, 0.0, 0.0];
    style.colors[StyleColor::TableRowBgAlt as usize] = with_alpha(p.surface0, 0.3);

    style.colors[StyleColor::TextSelectedBg as usize] = with_alpha(p.blue, 0.40);
    style.colors[StyleColor::DragDropTarget as usize] = p.lavender;
    style.colors[StyleColor::NavHighlight as usize] = p.lavender;
    style.colors[StyleColor::NavWindowingHighlight as usize] = p.lavender;
    style.colors[StyleColor::NavWindowingDimBg as usize] = with_alpha(p.crust, 0.70);
    style.colors[StyleColor::ModalWindowDimBg as usize] = with_alpha(p.crust, 0.70);
}
