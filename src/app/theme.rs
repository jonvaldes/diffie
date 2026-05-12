//! Catppuccin Macchiato palette + imgui style application.
//!
//! Palette values from <https://catppuccin.com/palette/> — kept verbatim so
//! they round-trip back to hex cleanly. Use these constants (or
//! `with_alpha`) instead of inlining ad-hoc RGBA literals.

#![allow(dead_code)]

use imgui::{Context, StyleColor};

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 4] {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0,
    ]
}

pub const ROSEWATER: [f32; 4] = rgb(0xf4, 0xdb, 0xd6);
pub const FLAMINGO: [f32; 4] = rgb(0xf0, 0xc6, 0xc6);
pub const PINK: [f32; 4] = rgb(0xf5, 0xbd, 0xe6);
pub const MAUVE: [f32; 4] = rgb(0xc6, 0xa0, 0xf6);
pub const RED: [f32; 4] = rgb(0xed, 0x87, 0x96);
pub const MAROON: [f32; 4] = rgb(0xee, 0x99, 0xa0);
pub const PEACH: [f32; 4] = rgb(0xf5, 0xa9, 0x7f);
pub const YELLOW: [f32; 4] = rgb(0xee, 0xd4, 0x9f);
pub const GREEN: [f32; 4] = rgb(0xa6, 0xda, 0x95);
pub const TEAL: [f32; 4] = rgb(0x8b, 0xd5, 0xca);
pub const SKY: [f32; 4] = rgb(0x91, 0xd7, 0xe3);
pub const SAPPHIRE: [f32; 4] = rgb(0x7d, 0xc4, 0xe4);
pub const BLUE: [f32; 4] = rgb(0x8a, 0xad, 0xf4);
pub const LAVENDER: [f32; 4] = rgb(0xb7, 0xbd, 0xf8);

pub const TEXT: [f32; 4] = rgb(0xca, 0xd3, 0xf5);
pub const SUBTEXT1: [f32; 4] = rgb(0xb8, 0xc0, 0xe0);
pub const SUBTEXT0: [f32; 4] = rgb(0xa5, 0xad, 0xcb);
pub const OVERLAY2: [f32; 4] = rgb(0x93, 0x9a, 0xb7);
pub const OVERLAY1: [f32; 4] = rgb(0x80, 0x87, 0xa2);
pub const OVERLAY0: [f32; 4] = rgb(0x6e, 0x73, 0x8d);
pub const SURFACE2: [f32; 4] = rgb(0x5b, 0x60, 0x78);
pub const SURFACE1: [f32; 4] = rgb(0x49, 0x4d, 0x64);
pub const SURFACE0: [f32; 4] = rgb(0x36, 0x3a, 0x4f);
pub const BASE: [f32; 4] = rgb(0x24, 0x27, 0x3a);
pub const MANTLE: [f32; 4] = rgb(0x1e, 0x20, 0x30);
pub const CRUST: [f32; 4] = rgb(0x18, 0x19, 0x26);

/// Returns `c` with its alpha replaced by `a`.
pub const fn with_alpha(c: [f32; 4], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}

/// Apply the Catppuccin Macchiato palette across imgui's StyleColor table.
pub fn apply(ctx: &mut Context) {
    let style = ctx.style_mut();

    style.colors[StyleColor::Text as usize] = TEXT;
    style.colors[StyleColor::TextDisabled as usize] = OVERLAY1;

    style.colors[StyleColor::WindowBg as usize] = BASE;
    style.colors[StyleColor::ChildBg as usize] = BASE;
    style.colors[StyleColor::PopupBg as usize] = MANTLE;

    style.colors[StyleColor::Border as usize] = SURFACE1;
    style.colors[StyleColor::BorderShadow as usize] = [0.0, 0.0, 0.0, 0.0];

    style.colors[StyleColor::FrameBg as usize] = SURFACE0;
    style.colors[StyleColor::FrameBgHovered as usize] = SURFACE1;
    style.colors[StyleColor::FrameBgActive as usize] = SURFACE2;

    style.colors[StyleColor::TitleBg as usize] = MANTLE;
    style.colors[StyleColor::TitleBgActive as usize] = SURFACE0;
    style.colors[StyleColor::TitleBgCollapsed as usize] = MANTLE;
    style.colors[StyleColor::MenuBarBg as usize] = MANTLE;

    style.colors[StyleColor::ScrollbarBg as usize] = MANTLE;
    style.colors[StyleColor::ScrollbarGrab as usize] = SURFACE1;
    style.colors[StyleColor::ScrollbarGrabHovered as usize] = SURFACE2;
    style.colors[StyleColor::ScrollbarGrabActive as usize] = OVERLAY0;

    style.colors[StyleColor::CheckMark as usize] = LAVENDER;
    style.colors[StyleColor::SliderGrab as usize] = BLUE;
    style.colors[StyleColor::SliderGrabActive as usize] = LAVENDER;

    style.colors[StyleColor::Button as usize] = SURFACE0;
    style.colors[StyleColor::ButtonHovered as usize] = SURFACE1;
    style.colors[StyleColor::ButtonActive as usize] = SURFACE2;

    style.colors[StyleColor::Header as usize] = SURFACE0;
    style.colors[StyleColor::HeaderHovered as usize] = SURFACE1;
    style.colors[StyleColor::HeaderActive as usize] = SURFACE2;

    style.colors[StyleColor::Separator as usize] = SURFACE1;
    style.colors[StyleColor::SeparatorHovered as usize] = SURFACE2;
    style.colors[StyleColor::SeparatorActive as usize] = OVERLAY0;

    style.colors[StyleColor::ResizeGrip as usize] = SURFACE1;
    style.colors[StyleColor::ResizeGripHovered as usize] = SURFACE2;
    style.colors[StyleColor::ResizeGripActive as usize] = OVERLAY0;

    style.colors[StyleColor::Tab as usize] = SURFACE0;
    style.colors[StyleColor::TabHovered as usize] = SURFACE2;
    style.colors[StyleColor::TabActive as usize] = SURFACE1;
    style.colors[StyleColor::TabUnfocused as usize] = MANTLE;
    style.colors[StyleColor::TabUnfocusedActive as usize] = SURFACE0;

    style.colors[StyleColor::PlotLines as usize] = SUBTEXT0;
    style.colors[StyleColor::PlotLinesHovered as usize] = LAVENDER;
    style.colors[StyleColor::PlotHistogram as usize] = LAVENDER;
    style.colors[StyleColor::PlotHistogramHovered as usize] = PINK;

    style.colors[StyleColor::TableHeaderBg as usize] = MANTLE;
    style.colors[StyleColor::TableBorderStrong as usize] = SURFACE1;
    style.colors[StyleColor::TableBorderLight as usize] = SURFACE0;
    style.colors[StyleColor::TableRowBg as usize] = [0.0, 0.0, 0.0, 0.0];
    style.colors[StyleColor::TableRowBgAlt as usize] = with_alpha(SURFACE0, 0.3);

    style.colors[StyleColor::TextSelectedBg as usize] = with_alpha(BLUE, 0.40);
    style.colors[StyleColor::DragDropTarget as usize] = LAVENDER;
    style.colors[StyleColor::NavHighlight as usize] = LAVENDER;
    style.colors[StyleColor::NavWindowingHighlight as usize] = LAVENDER;
    style.colors[StyleColor::NavWindowingDimBg as usize] = with_alpha(CRUST, 0.70);
    style.colors[StyleColor::ModalWindowDimBg as usize] = with_alpha(CRUST, 0.70);
}
