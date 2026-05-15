//! Tree-sitter–driven syntax highlighting.
//!
//! We parse the full source for a side, then walk the resulting tree to map
//! node kinds onto a small set of `SyntaxKind` categories that the diff view
//! paints via the Catppuccin palette. Results are cached per source-hash so
//! re-parses only happen when the buffer actually changes.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

use tree_sitter::{Language, Parser};

use super::theme;

/// Coarse category we map tree-sitter node kinds onto. Each variant resolves
/// to a single palette color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxKind {
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Function,
    Preproc,
    Constant,
    /// Matched bracket pair colored by nesting depth (`d % BRACKET_PALETTE.len()`).
    Bracket(u8),
}

/// Rainbow palette for nested brackets. Walks the hue wheel so adjacent
/// depths are easy to tell apart.
const BRACKET_PALETTE: [[f32; 4]; 7] = [
    theme::YELLOW,
    theme::MAUVE,
    theme::SKY,
    theme::PEACH,
    theme::GREEN,
    theme::PINK,
    theme::TEAL,
];

impl SyntaxKind {
    pub fn color(self) -> [f32; 4] {
        match self {
            SyntaxKind::Keyword => theme::MAUVE,
            SyntaxKind::Type => theme::YELLOW,
            SyntaxKind::String => theme::GREEN,
            SyntaxKind::Number => theme::PEACH,
            SyntaxKind::Comment => theme::OVERLAY1,
            SyntaxKind::Function => theme::BLUE,
            SyntaxKind::Preproc => theme::PINK,
            SyntaxKind::Constant => theme::PEACH,
            SyntaxKind::Bracket(d) => {
                let c = BRACKET_PALETTE[(d as usize) % BRACKET_PALETTE.len()];
                boost_saturation(c, 1.25)
            }
        }
    }
}

/// Which row/hl background sits under a piece of text. Splits Delete and
/// Insert into "row-tint only" (non-hl chars) and "row tint + hl rect" (the
/// saturated per-char highlights), since both shift the visible bg color
/// enough to need their own contrast tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlBg {
    None,
    DeleteRow,
    DeleteHl,
    InsertRow,
    InsertHl,
}

/// Per-`SyntaxKind` foreground colors precomputed for one background.
/// `default` covers un-tokenized stretches.
#[derive(Clone, Copy, Debug)]
pub struct ColorTable {
    pub keyword: [f32; 4],
    pub type_: [f32; 4],
    pub string: [f32; 4],
    pub number: [f32; 4],
    pub comment: [f32; 4],
    pub function: [f32; 4],
    pub preproc: [f32; 4],
    pub constant: [f32; 4],
    pub default: [f32; 4],
}

impl ColorTable {
    pub fn get(&self, kind: Option<SyntaxKind>) -> [f32; 4] {
        match kind {
            Some(SyntaxKind::Keyword) => self.keyword,
            Some(SyntaxKind::Type) => self.type_,
            Some(SyntaxKind::String) => self.string,
            Some(SyntaxKind::Number) => self.number,
            Some(SyntaxKind::Comment) => self.comment,
            Some(SyntaxKind::Function) => self.function,
            Some(SyntaxKind::Preproc) => self.preproc,
            Some(SyntaxKind::Constant) => self.constant,
            Some(SyntaxKind::Bracket(d)) => SyntaxKind::Bracket(d).color(),
            None => self.default,
        }
    }
}

/// Scale a color's distance from its luma (Rec. 601 weights) by `factor` —
/// `>1.0` boosts saturation, `<1.0` desaturates. Channels clamp to [0, 1].
fn boost_saturation(c: [f32; 4], factor: f32) -> [f32; 4] {
    let luma = 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];
    [
        (luma + (c[0] - luma) * factor).clamp(0.0, 1.0),
        (luma + (c[1] - luma) * factor).clamp(0.0, 1.0),
        (luma + (c[2] - luma) * factor).clamp(0.0, 1.0),
        c[3],
    ]
}

/// sRGB → linear conversion (per-channel gamma decode).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear sRGB → CIE XYZ (D65).
fn rgb_to_xyz(c: [f32; 4]) -> [f32; 3] {
    let r = srgb_to_linear(c[0]);
    let g = srgb_to_linear(c[1]);
    let b = srgb_to_linear(c[2]);
    [
        0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b,
        0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b,
        0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b,
    ]
}

/// XYZ → CIE Lab (D65 reference white).
fn xyz_to_lab(xyz: [f32; 3]) -> [f32; 3] {
    const XN: f32 = 0.95047;
    const YN: f32 = 1.0;
    const ZN: f32 = 1.08883;
    let f = |t: f32| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let fx = f(xyz[0] / XN);
    let fy = f(xyz[1] / YN);
    let fz = f(xyz[2] / ZN);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_of(c: [f32; 4]) -> [f32; 3] {
    xyz_to_lab(rgb_to_xyz(c))
}

/// Inverse of `xyz_to_lab` (D65).
fn lab_to_xyz(lab: [f32; 3]) -> [f32; 3] {
    const XN: f32 = 0.95047;
    const YN: f32 = 1.0;
    const ZN: f32 = 1.08883;
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = lab[1] / 500.0 + fy;
    let fz = fy - lab[2] / 200.0;
    let inv = |f: f32| {
        let f3 = f * f * f;
        if f3 > 0.008856 {
            f3
        } else {
            (f - 16.0 / 116.0) / 7.787
        }
    };
    [XN * inv(fx), YN * inv(fy), ZN * inv(fz)]
}

/// Linear-RGB → sRGB (gamma encode), per channel.
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// CIE XYZ → linear sRGB, clamped to [0, 1] after the matrix.
fn xyz_to_rgb_linear(xyz: [f32; 3]) -> [f32; 3] {
    let r = 3.240_454_2 * xyz[0] - 1.537_138_5 * xyz[1] - 0.498_531_4 * xyz[2];
    let g = -0.969_266_0 * xyz[0] + 1.876_010_8 * xyz[1] + 0.041_556_0 * xyz[2];
    let b = 0.055_643_4 * xyz[0] - 0.204_025_9 * xyz[1] + 1.057_225_2 * xyz[2];
    [r, g, b]
}

fn lab_to_rgb(lab: [f32; 3], alpha: f32) -> [f32; 4] {
    let xyz = lab_to_xyz(lab);
    let lin = xyz_to_rgb_linear(xyz);
    [
        linear_to_srgb(lin[0]),
        linear_to_srgb(lin[1]),
        linear_to_srgb(lin[2]),
        alpha,
    ]
}

/// Relative luminance in [0, 1] per WCAG 2.x — sRGB → linear, then the
/// 0.2126 / 0.7152 / 0.0722 weighted sum on the linearized channels.
fn wcag_luminance(c: [f32; 4]) -> f32 {
    let l = |x: f32| {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * l(c[0]) + 0.7152 * l(c[1]) + 0.0722 * l(c[2])
}

/// WCAG contrast ratio in [1, 21].
fn wcag_ratio(a_y: f32, b_y: f32) -> f32 {
    let (hi, lo) = if a_y >= b_y { (a_y, b_y) } else { (b_y, a_y) };
    (hi + 0.05) / (lo + 0.05)
}

/// Push `fg`'s lightness away from `bg`'s — keeping `fg`'s a\*/b\* (its hue
/// + chroma identity) — until the WCAG contrast ratio against `bg` clears
/// `TARGET_CONTRAST` or we hit the L\* range edge. Starts at a 40 L\* push
/// and increments by 3 each iteration, which is enough resolution for
/// readable text without burning many cycles.
fn shift_lightness(fg: [f32; 4], bg: [f32; 4]) -> [f32; 4] {
    const TARGET_CONTRAST: f32 = 4.5;
    const STEP: f32 = 3.0;
    const MAX_ITER: usize = 32;

    let bg_lab = lab_of(bg);
    let bg_y = wcag_luminance(bg);
    let direction = if bg_lab[0] < 50.0 { 1.0 } else { -1.0 };
    let fg_lab = lab_of(fg);

    let mut target_l = (bg_lab[0] + direction * 40.0).clamp(0.0, 100.0);
    let mut current = lab_to_rgb([target_l, fg_lab[1], fg_lab[2]], fg[3]);
    for _ in 0..MAX_ITER {
        if wcag_ratio(wcag_luminance(current), bg_y) >= TARGET_CONTRAST {
            return current;
        }
        let next = target_l + direction * STEP;
        if next <= 0.0 || next >= 100.0 {
            target_l = next.clamp(0.0, 100.0);
            return lab_to_rgb([target_l, fg_lab[1], fg_lab[2]], fg[3]);
        }
        target_l = next;
        current = lab_to_rgb([target_l, fg_lab[1], fg_lab[2]], fg[3]);
    }
    current
}

/// CIE ΔE*76 — Euclidean distance in Lab. Ranges roughly 0–110+ for
/// sRGB; values ≥ ~25 are clearly distinguishable, ≥ ~50 are very different.
fn perceptual_distance(a: [f32; 4], b: [f32; 4]) -> f32 {
    let la = lab_of(a);
    let lb = lab_of(b);
    ((la[0] - lb[0]).powi(2) + (la[1] - lb[1]).powi(2) + (la[2] - lb[2]).powi(2)).sqrt()
}

fn negate(c: [f32; 4]) -> [f32; 4] {
    [1.0 - c[0], 1.0 - c[1], 1.0 - c[2], c[3]]
}

/// Pick a readable foreground color for `fg` over `bg`. The gatekeeper is
/// **WCAG contrast** (luminance-based), not perceptual ΔE — readability
/// hinges on lightness separation; equal-lightness color pairs at different
/// hues look distinct on a color picker but visually merge when used as
/// text on a saturated background.
///
/// 1. Keep `fg` when WCAG ratio ≥ `MIN_WCAG`.
/// 2. Otherwise try `negate(fg)` — flips to the opposite end of the L\* axis.
/// 3. Fallback: `shift_lightness` iterates until WCAG clears the threshold
///    (or hits the L\* range edge).
fn adapt(fg: [f32; 4], bg: [f32; 4]) -> [f32; 4] {
    const MIN_WCAG: f32 = 3.0;
    let bg_y = wcag_luminance(bg);
    if wcag_ratio(wcag_luminance(fg), bg_y) >= MIN_WCAG {
        return fg;
    }
    let neg = negate(fg);
    if wcag_ratio(wcag_luminance(neg), bg_y) >= MIN_WCAG {
        return neg;
    }
    shift_lightness(fg, bg)
}

/// Alpha-over composite of `b` on `a`, performed in **linear** RGB to match
/// imgui's gamma-correct blending. Doing the blend in sRGB (the naive
/// approach) shifts the resulting Lab L\* by several units against the
/// truly-rendered pixel, which is enough to flip the adapt direction
/// (`< 50` test) and to push readability targets in the wrong direction.
fn composite(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let alpha = b[3];
    let ar = srgb_to_linear(a[0]);
    let ag = srgb_to_linear(a[1]);
    let ab = srgb_to_linear(a[2]);
    let br = srgb_to_linear(b[0]);
    let bg = srgb_to_linear(b[1]);
    let bb = srgb_to_linear(b[2]);
    [
        linear_to_srgb(ar * (1.0 - alpha) + br * alpha),
        linear_to_srgb(ag * (1.0 - alpha) + bg * alpha),
        linear_to_srgb(ab * (1.0 - alpha) + bb * alpha),
        1.0,
    ]
}

/// RGBA literals must match what diff_view actually paints. If you change
/// the hunk colors in diff_view, mirror them here so the contrast tables
/// stay accurate.
const ROW_BG_DELETE: [f32; 4] = [0.55, 0.18, 0.18, 0.30];
const HL_DELETE: [f32; 4] = [0.85, 0.18, 0.18, 0.20];
const ROW_BG_INSERT: [f32; 4] = [0.18, 0.50, 0.22, 0.30];
const HL_INSERT: [f32; 4] = [0.18, 0.70, 0.30, 0.20];

fn delete_row_bg() -> [f32; 4] {
    composite(theme::BASE, ROW_BG_DELETE)
}
fn delete_hl_bg() -> [f32; 4] {
    composite(delete_row_bg(), HL_DELETE)
}
fn insert_row_bg() -> [f32; 4] {
    composite(theme::BASE, ROW_BG_INSERT)
}
fn insert_hl_bg() -> [f32; 4] {
    composite(insert_row_bg(), HL_INSERT)
}

fn build_table(bg: [f32; 4]) -> ColorTable {
    ColorTable {
        keyword: adapt(theme::MAUVE, bg),
        type_: adapt(theme::YELLOW, bg),
        string: adapt(theme::GREEN, bg),
        number: adapt(theme::PEACH, bg),
        comment: adapt(theme::OVERLAY1, bg),
        function: adapt(theme::BLUE, bg),
        preproc: adapt(theme::PINK, bg),
        constant: adapt(theme::PEACH, bg),
        default: adapt(theme::TEXT, bg),
    }
}

fn normal_table() -> ColorTable {
    ColorTable {
        keyword: theme::MAUVE,
        type_: theme::YELLOW,
        string: theme::GREEN,
        number: theme::PEACH,
        comment: theme::OVERLAY1,
        function: theme::BLUE,
        preproc: theme::PINK,
        constant: theme::PEACH,
        default: theme::TEXT,
    }
}

static NORMAL_TABLE: std::sync::OnceLock<ColorTable> = std::sync::OnceLock::new();
static DELETE_ROW_TABLE: std::sync::OnceLock<ColorTable> = std::sync::OnceLock::new();
static DELETE_HL_TABLE: std::sync::OnceLock<ColorTable> = std::sync::OnceLock::new();
static INSERT_ROW_TABLE: std::sync::OnceLock<ColorTable> = std::sync::OnceLock::new();
static INSERT_HL_TABLE: std::sync::OnceLock<ColorTable> = std::sync::OnceLock::new();

/// Look up the precomputed color table for a given background. The
/// underlying `OnceLock`s populate on first call, so all five tables are
/// computed exactly once.
pub fn table_for(bg: HlBg) -> &'static ColorTable {
    match bg {
        HlBg::None => NORMAL_TABLE.get_or_init(normal_table),
        HlBg::DeleteRow => DELETE_ROW_TABLE.get_or_init(|| build_table(delete_row_bg())),
        HlBg::DeleteHl => DELETE_HL_TABLE.get_or_init(|| build_table(delete_hl_bg())),
        HlBg::InsertRow => INSERT_ROW_TABLE.get_or_init(|| build_table(insert_row_bg())),
        HlBg::InsertHl => INSERT_HL_TABLE.get_or_init(|| build_table(insert_hl_bg())),
    }
}

/// Force the OnceLocks to populate now. Called once at app startup so the
/// contrast math doesn't run on the first frame's hot path.
pub fn prime_tables() {
    let _ = table_for(HlBg::None);
    let _ = table_for(HlBg::DeleteRow);
    let _ = table_for(HlBg::DeleteHl);
    let _ = table_for(HlBg::InsertRow);
    let _ = table_for(HlBg::InsertHl);
}

/// A colored run within a line. `start_col` / `end_col` are **char** indices
/// (not bytes) so the diff view's column-major layout can paint directly.
#[derive(Clone, Debug)]
pub struct LineSpan {
    pub start_col: usize,
    pub end_col: usize,
    pub kind: SyntaxKind,
}

/// One line's worth of highlight spans, in left-to-right order, non-overlapping.
pub type LineSpans = Vec<LineSpan>;

/// Supported languages — used as the cache's identity (so swapping languages
/// for the same session invalidates) and as the dispatch for `Parser::set_language`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Cpp,
    CSharp,
    Hlsl,
    Rust,
}

impl Lang {
    fn language(self) -> Language {
        match self {
            Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Lang::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Lang::Hlsl => tree_sitter_hlsl::LANGUAGE_HLSL.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }
}

/// Look up a language by file extension. Returns `None` for unknown
/// extensions; callers fall back to plain (uncolored) text in that case.
pub fn lang_for_path(path: &Path) -> Option<Lang> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())?;
    match ext.as_str() {
        "c" | "h" | "cpp" | "cxx" | "cc" | "hpp" | "hh" | "hxx" | "ipp" | "tpp" | "inl" => {
            Some(Lang::Cpp)
        }
        "cs" => Some(Lang::CSharp),
        "hlsl" | "hlsli" | "fx" | "fxh" | "vsh" | "psh" | "csh" | "shader" | "compute" => {
            Some(Lang::Hlsl)
        }
        "rs" => Some(Lang::Rust),
        _ => None,
    }
}

/// Per-side highlighter cache. Keyed by `(SessionId, side_is_left)` to keep
/// the two panes' results independent.
#[derive(Default)]
pub struct HighlightCache {
    entries: HashMap<u64, Cached>,
    parser: Option<Parser>,
}

struct Cached {
    content_hash: u64,
    language_id: u64,
    lines: Vec<LineSpans>,
}

impl HighlightCache {
    /// Return per-line highlight spans for `lines` under `lang`, reusing the
    /// cached result when the content hash matches. Returns an empty slice
    /// when there's no language or parsing fails.
    pub fn highlights<'a>(
        &'a mut self,
        key: u64,
        lang: Option<Lang>,
        lines: &[String],
    ) -> &'a [LineSpans] {
        let lang_id = match lang {
            Some(Lang::Cpp) => 1,
            Some(Lang::CSharp) => 2,
            Some(Lang::Hlsl) => 3,
            Some(Lang::Rust) => 4,
            None => 0,
        };
        let content_hash = hash_lines(lines);

        let needs_refresh = match self.entries.get(&key) {
            Some(c) => c.content_hash != content_hash || c.language_id != lang_id,
            None => true,
        };

        if needs_refresh {
            let lines_out = match lang {
                Some(l) => self.compute(l, lines),
                None => Vec::new(),
            };
            self.entries.insert(
                key,
                Cached {
                    content_hash,
                    language_id: lang_id,
                    lines: lines_out,
                },
            );
        }
        &self.entries.get(&key).unwrap().lines
    }

    /// Drop the cache entry for `key`. Called when a session closes.
    pub fn forget(&mut self, key: u64) {
        self.entries.remove(&key);
    }

    fn compute(&mut self, lang: Lang, lines: &[String]) -> Vec<LineSpans> {
        let source: String = lines.join("\n");
        let parser = self.parser.get_or_insert_with(Parser::new);
        if parser.set_language(&lang.language()).is_err() {
            return vec![Vec::new(); lines.len()];
        }
        let Some(tree) = parser.parse(&source, None) else {
            return vec![Vec::new(); lines.len()];
        };
        let bytes = source.as_bytes();
        let line_starts = compute_line_starts(bytes);
        let mut out: Vec<LineSpans> = (0..lines.len()).map(|_| Vec::new()).collect();
        let mut masked: Vec<(usize, usize)> = Vec::new();
        let mut cursor = tree.walk();
        walk(&mut cursor, &mut out, &mut masked, &line_starts, bytes);
        masked.sort_by_key(|r| r.0);
        augment_with_brackets(bytes, &masked, &line_starts, &mut out);
        // Sort each line's spans by start_col so painters can scan left-right.
        for l in &mut out {
            l.sort_by_key(|s| s.start_col);
        }
        out
    }
}

fn hash_lines(lines: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lines.len().hash(&mut hasher);
    for l in lines {
        l.hash(&mut hasher);
        b'\n'.hash(&mut hasher);
    }
    hasher.finish()
}

fn compute_line_starts(bytes: &[u8]) -> Vec<usize> {
    let mut v = vec![0usize];
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'\n' {
            v.push(i + 1);
        }
    }
    v
}

fn walk(
    cursor: &mut tree_sitter::TreeCursor<'_>,
    out: &mut [LineSpans],
    masked: &mut Vec<(usize, usize)>,
    line_starts: &[usize],
    bytes: &[u8],
) {
    let node = cursor.node();
    if let Some((kind, atomic)) = classify(&node) {
        emit_spans(
            node.start_byte(),
            node.end_byte(),
            kind,
            out,
            line_starts,
            bytes,
        );
        if matches!(kind, SyntaxKind::String | SyntaxKind::Comment) {
            masked.push((node.start_byte(), node.end_byte()));
        }
        if atomic {
            return;
        }
    }
    if cursor.goto_first_child() {
        loop {
            walk(cursor, out, masked, line_starts, bytes);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Scan source bytes for `()`/`[]`/`{}` outside string/comment ranges and
/// emit one `Bracket(depth)` span per bracket. Depth is shared across all
/// bracket flavors. Closers decrement first so the closer paints the same
/// color as its opener.
fn augment_with_brackets(
    bytes: &[u8],
    masked: &[(usize, usize)],
    line_starts: &[usize],
    out: &mut [LineSpans],
) {
    let palette_len = BRACKET_PALETTE.len();
    let mut depth: i32 = 0;
    let mut mi = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        while mi < masked.len() && masked[mi].1 <= i {
            mi += 1;
        }
        if mi < masked.len() && i >= masked[mi].0 && i < masked[mi].1 {
            i = masked[mi].1;
            continue;
        }
        let b = bytes[i];
        match b {
            b'(' | b'[' | b'{' => {
                let d = (depth.max(0) as usize) % palette_len;
                emit_bracket(i, d as u8, line_starts, bytes, out);
                depth += 1;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                let d = (depth.max(0) as usize) % palette_len;
                emit_bracket(i, d as u8, line_starts, bytes, out);
            }
            _ => {}
        }
        i += 1;
    }
}

fn emit_bracket(byte: usize, d: u8, line_starts: &[usize], bytes: &[u8], out: &mut [LineSpans]) {
    let (line, col) = byte_to_line_col(byte, line_starts, bytes);
    if line < out.len() {
        out[line].push(LineSpan {
            start_col: col,
            end_col: col + 1,
            kind: SyntaxKind::Bracket(d),
        });
    }
}

/// Map a tree-sitter node onto a `SyntaxKind`, returning `(kind, atomic)`.
/// `atomic == true` stops recursion so inner tokens don't overwrite the
/// span (e.g. an `escape_sequence` inside `string_literal`).
fn classify(node: &tree_sitter::Node) -> Option<(SyntaxKind, bool)> {
    let k = node.kind();
    if !node.is_named() {
        // Anonymous nodes are token literals — e.g. "if", "+", "int", "{".
        // Treat alphanumeric runs of length ≥ 2 as keywords; everything else
        // (operators, punctuation) gets no color.
        let chars = k.chars();
        let all_word = chars.clone().all(|c| c.is_ascii_alphanumeric() || c == '_');
        let starts_letter = chars.clone().next().map_or(false, |c| c.is_ascii_alphabetic());
        if all_word && starts_letter && k.len() >= 2 {
            return Some((SyntaxKind::Keyword, false));
        }
        return None;
    }
    match k {
        // Comments
        "comment" | "line_comment" | "block_comment" => Some((SyntaxKind::Comment, true)),
        // Strings & char literals (cpp, cs, hlsl variants)
        "string_literal"
        | "raw_string_literal"
        | "char_literal"
        | "character_literal"
        | "verbatim_string_literal"
        | "interpolated_string_text"
        | "string_content"
        | "system_lib_string" => Some((SyntaxKind::String, true)),
        // Numerics
        "number_literal" | "integer_literal" | "real_literal" | "float_literal" => {
            Some((SyntaxKind::Number, true))
        }
        // Types
        "primitive_type"
        | "type_identifier"
        | "sized_type_specifier"
        | "predefined_type"
        | "implicit_type" => Some((SyntaxKind::Type, true)),
        // Constants
        "true" | "false" | "null_literal" | "boolean_literal" | "this_expression" => {
            Some((SyntaxKind::Constant, true))
        }
        _ => None,
    }
}

/// Split a node's byte range across line boundaries, recording one span per
/// line that overlaps. Char-column math counts UTF-8 character boundaries so
/// the result aligns with the diff view's char-grid layout.
fn emit_spans(
    start_byte: usize,
    end_byte: usize,
    kind: SyntaxKind,
    out: &mut [LineSpans],
    line_starts: &[usize],
    bytes: &[u8],
) {
    if start_byte >= end_byte || out.is_empty() {
        return;
    }
    let (mut line, mut col) = byte_to_line_col(start_byte, line_starts, bytes);
    let (end_line, end_col) = byte_to_line_col(end_byte, line_starts, bytes);
    while line < end_line {
        if line < out.len() {
            let line_end_col = chars_to_eol(line, line_starts, bytes);
            if line_end_col > col {
                out[line].push(LineSpan {
                    start_col: col,
                    end_col: line_end_col,
                    kind,
                });
            }
        }
        line += 1;
        col = 0;
    }
    if line < out.len() && end_col > col {
        out[line].push(LineSpan {
            start_col: col,
            end_col,
            kind,
        });
    }
}

/// Char column of `byte` within its line. `byte` is treated as a character
/// boundary; the count excludes UTF-8 continuation bytes (`0b10xxxxxx`).
fn byte_to_line_col(byte: usize, line_starts: &[usize], bytes: &[u8]) -> (usize, usize) {
    // partition_point: first index i where line_starts[i] > byte.
    let line = line_starts.partition_point(|&s| s <= byte).saturating_sub(1);
    let line_start = line_starts[line];
    let mut col = 0usize;
    for &b in &bytes[line_start..byte.min(bytes.len())] {
        if (b & 0b1100_0000) != 0b1000_0000 {
            col += 1;
        }
    }
    (line, col)
}

fn chars_to_eol(line: usize, line_starts: &[usize], bytes: &[u8]) -> usize {
    let start = line_starts[line];
    let end = line_starts.get(line + 1).copied().unwrap_or(bytes.len());
    // Exclude the trailing '\n' if present.
    let end = if end > start && bytes[end - 1] == b'\n' {
        end - 1
    } else {
        end
    };
    let mut col = 0usize;
    for &b in &bytes[start..end] {
        if (b & 0b1100_0000) != 0b1000_0000 {
            col += 1;
        }
    }
    col
}
