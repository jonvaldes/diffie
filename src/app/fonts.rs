//! Embedded code-font assets + the `CodeFont` enum the user picks in
//! Preferences. Each variant maps to a TTF baked into the binary via
//! `include_bytes!`. Sizes (~290–1700 KB) are upfront in this module so the
//! cost of adding a font is obvious.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeFont {
    JetBrainsMono,
    FiraCode,
    CascadiaCode,
    NotoSansMono,
}

impl Default for CodeFont {
    fn default() -> Self {
        CodeFont::JetBrainsMono
    }
}

impl CodeFont {
    pub fn label(self) -> &'static str {
        match self {
            CodeFont::JetBrainsMono => "JetBrains Mono",
            CodeFont::FiraCode => "Fira Code",
            CodeFont::CascadiaCode => "Cascadia Code",
            CodeFont::NotoSansMono => "Noto Sans Mono",
        }
    }

    /// Codepoint to use as the "newline" marker in the visible-whitespace
    /// ghost layer. `⏎` (U+23CE) where the font ships it; `¶` (U+00B6) for
    /// Noto Sans Mono, whose arrows block stops at U+2195.
    pub fn eol_codepoint(self) -> u32 {
        match self {
            CodeFont::NotoSansMono => 0x00b6,
            _ => 0x23ce,
        }
    }

    pub fn bytes(self) -> &'static [u8] {
        match self {
            CodeFont::JetBrainsMono => JETBRAINS_MONO,
            CodeFont::FiraCode => FIRA_CODE,
            CodeFont::CascadiaCode => CASCADIA_CODE,
            CodeFont::NotoSansMono => NOTO_SANS_MONO,
        }
    }
}

pub const ALL: &[CodeFont] = &[
    CodeFont::JetBrainsMono,
    CodeFont::FiraCode,
    CodeFont::CascadiaCode,
    CodeFont::NotoSansMono,
];

pub static JETBRAINS_MONO: &[u8] = include_bytes!("../../assets/JetBrainsMono-Regular.ttf");
pub static FIRA_CODE: &[u8] = include_bytes!("../../assets/FiraCode-Regular.ttf");
pub static CASCADIA_CODE: &[u8] = include_bytes!("../../assets/CascadiaCode-Regular.ttf");
pub static NOTO_SANS_MONO: &[u8] = include_bytes!("../../assets/NotoSansMono-Regular.ttf");
