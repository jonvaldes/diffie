//! Adapter from imgui's per-frame input state to the engine-agnostic
//! `InputFrame`. This is the only file in `app::input*` that depends on
//! imgui.

use super::input::{InputFrame, Modifiers, MouseButtons};

impl InputFrame {
    pub fn from_ui(ui: &imgui::Ui) -> Self {
        let io = ui.io();
        InputFrame {
            mouse_pos: io.mouse_pos,
            mouse_buttons: MouseButtons {
                left_down: ui.is_mouse_down(imgui::MouseButton::Left),
                left_clicked: ui.is_mouse_clicked(imgui::MouseButton::Left),
                right_clicked: ui.is_mouse_clicked(imgui::MouseButton::Right),
            },
            modifiers: Modifiers {
                shift: io.key_shift,
                ctrl: io.key_ctrl,
                alt: io.key_alt,
                super_: io.key_super,
            },
            wheel_v: io.mouse_wheel,
            wheel_h: io.mouse_wheel_h,
        }
    }
}
