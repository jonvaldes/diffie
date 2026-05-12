//! Plain-data snapshot of per-frame input state, plus pure functions that
//! consume it. The `from_ui` adapter lives in `input_imgui.rs` so this file
//! has no imgui dependency and is testable without the `gui` feature.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseButtons {
    pub left_down: bool,
    pub left_clicked: bool,
    pub right_clicked: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_: bool,
}

/// One frame's worth of input. Everything the view logic needs to make
/// decisions, with nothing it doesn't. Coordinates are in imgui screen
/// space (pixels, origin top-left of the OS window).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct InputFrame {
    pub mouse_pos: [f32; 2],
    pub mouse_buttons: MouseButtons,
    pub modifiers: Modifiers,
    /// Vertical wheel delta in line units (already normalized from pixel
    /// deltas by the winit handler in `app::mod`).
    pub wheel_v: f32,
    pub wheel_h: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_input_frame_is_neutral() {
        let f = InputFrame::default();
        assert_eq!(f.mouse_pos, [0.0, 0.0]);
        assert!(!f.mouse_buttons.left_down);
        assert!(!f.mouse_buttons.left_clicked);
        assert!(!f.mouse_buttons.right_clicked);
        assert!(!f.modifiers.shift);
        assert_eq!(f.wheel_v, 0.0);
    }
}
