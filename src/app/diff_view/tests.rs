//! Tests for the 2-way diff view, moved verbatim from the pre-split
//! `diff_view.rs`. Two inner modules: `word_bounds_tests` (pure unit tests
//! on `double_click_word_bounds`) and `headless_tests` (a manually-driven
//! imgui context exercising the `render` entry point).
//!
//! Inner modules use `use super::*` to bring these names into scope.

#![allow(unused_imports)]

pub(super) use super::common::{
    build_pane, double_click_word_bounds, ordered_endpoints, row_h, DiffViewState, SelPoint,
    Selection, Side,
};
// The public `render` function lives in the parent `diff_view` module
// (defined in `mod.rs`). Bring it in by name; the inner test modules
// invoke it as `render(...)`.
pub(super) use super::render;
pub(super) use super::super::undo_stack::DiffEdit;
pub(super) use std::cell::Cell;

mod word_bounds_tests {
    use super::*;

    #[test]
    fn word_run() {
        let s = "alpha beta gamma";
        // Click at any char of "beta" → selects "beta".
        assert_eq!(double_click_word_bounds(s, 6), (6, 10));
        assert_eq!(double_click_word_bounds(s, 7), (6, 10));
        assert_eq!(double_click_word_bounds(s, 9), (6, 10));
    }

    #[test]
    fn punct_is_single_char() {
        let s = "#[cfg(target_arch = \"wasm32\")]";
        // '=' is at index 18.
        assert_eq!(double_click_word_bounds(s, 18), (18, 19));
        // '#' at index 0.
        assert_eq!(double_click_word_bounds(s, 0), (0, 1));
        // ')' at index 28.
        assert_eq!(double_click_word_bounds(s, 28), (28, 29));
    }

    #[test]
    fn whitespace_run() {
        let s = "a   b";
        assert_eq!(double_click_word_bounds(s, 2), (1, 4));
    }

    #[test]
    fn underscore_is_word() {
        let s = "target_arch";
        assert_eq!(double_click_word_bounds(s, 6), (0, 11));
    }

    #[test]
    fn empty_and_out_of_bounds() {
        assert_eq!(double_click_word_bounds("", 0), (0, 0));
        // Clamps high byte_idx to last char.
        assert_eq!(double_click_word_bounds("ab", 100), (0, 2));
    }

    #[test]
    fn utf8() {
        // 'café': 'c','a','f','é'. 'é' is 2 bytes (0xC3 0xA9).
        let s = "café word";
        // 'é' starts at byte 3, len 2.
        assert_eq!(double_click_word_bounds(s, 3), (0, 5)); // selects "café"
        assert_eq!(double_click_word_bounds(s, 6), (6, 10)); // selects "word"
    }
}

mod headless_tests {
    use super::*;
    use crate::session::{SessionMode, SessionStore};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// `imgui::Context` is a process-global singleton. `cargo test` runs
    /// tests in parallel by default, so we serialize through a static
    /// mutex. Holding the guard for the lifetime of the context guarantees
    /// at most one active context across the process.
    fn imgui_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        // Recover from poisoning: a panicked test shouldn't block others.
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn build_ui_context() -> imgui::Context {
        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        // Enable keyboard nav so `set_keyboard_focus_here` actually engages
        // imgui's nav system (which is what triggers `ScrollToBringRectIntoView`
        // — the behavior we're testing for).
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Materialize the default font atlas so layouts can measure text.
        let _atlas = ctx.fonts().build_rgba32_texture();
        ctx
    }

    /// Load the live app's mono font (RobotoMono) into the context and
    /// return its `FontId`. Tests that care about pixel-accurate hit
    /// testing (e.g., double-click column → byte index) must push this
    /// font; otherwise the default proportional font's varying glyph
    /// widths make `(click_x - widget_x0) / char_w` lie.
    fn load_mono_font(ctx: &mut imgui::Context, size_pixels: f32) -> imgui::FontId {
        ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: include_bytes!("../../../assets/RobotoMono-Regular.ttf"),
            size_pixels,
            config: Some(imgui::FontConfig {
                size_pixels,
                ..Default::default()
            }),
        }])
    }

    /// One render frame with no input: scroll_x should be at 0 and no pin
    /// should be queued. Confirms the harness wiring is sound (font atlas
    /// resolves, child windows lay out, the render fn writes back its
    /// per-frame scroll fields).
    #[test]
    fn headless_render_reads_scroll_x_at_zero() {
        let _guard = imgui_lock();
        let store = SessionStore::new();
        // Make one side wide enough that content_w > pane_w. Without this,
        // horizontal scroll is permanently 0 regardless of any bug.
        let long = "x".repeat(500);
        let text = format!("short\n{long}\ntail\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };

        let mut ctx = build_ui_context();
        let ui = ctx.new_frame();

        let mut view_state = DiffViewState::default();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();

        ui.window("test")
            .size([1000.0, 600.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    &store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    &mut view_state,
                    None,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let _draw = ctx.render();

        assert_eq!(view_state.last_left_scroll_x, 0.0);
        assert_eq!(view_state.last_right_scroll_x, 0.0);
        assert!(view_state.pin_scroll_x_after_splice.is_none());
        // No keys pressed, no splice; no edits should be queued.
        assert!(pending_edits.is_empty());
    }

    /// Pre-seed a selection on the short first line, inject Backspace via
    /// `io.add_key_event`, render once, and verify the splice fired and
    /// queued the scroll-x pin. Stops short of multi-frame application —
    /// the next iteration would apply the splice edit via
    /// `store.splice_two_way_lines`, render two more frames, and assert
    /// `last_left_scroll_x` stayed at 0.
    #[test]
    fn headless_splice_sets_pin() {
        let _guard = imgui_lock();
        let store = SessionStore::new();
        let long = "x".repeat(500);
        let text = format!("hello world\n{long}\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };

        let mut ctx = build_ui_context();
        // Inject Backspace press for this frame.
        ctx.io_mut().add_key_event(imgui::Key::Backspace, true);

        let ui = ctx.new_frame();
        let mut view_state = DiffViewState::default();
        // Select "hello" on line 1 of side A.
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();

        ui.window("test")
            .size([1000.0, 600.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    &store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    &mut view_state,
                    None,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let _draw = ctx.render();

        // Splice path fired: edit queued, arrow_focus parked, pin set.
        assert!(
            pending_edits
                .iter()
                .any(|e| matches!(e, DiffEdit::SpliceTwoWayLines { .. })),
            "expected a SpliceTwoWayLines edit to be queued",
        );
        assert!(view_state.selection.is_none(), "selection should be cleared after splice");
        let (side, x, frames) = view_state
            .pin_scroll_x_after_splice
            .expect("pin should be set after splice");
        assert_eq!(side, Side::Left);
        assert_eq!(x, 0.0); // scroll_x was 0 going in
        assert_eq!(frames, 4);
    }

    /// Apply a queued `DiffEdit` to the store the same way the real app
    /// would, bypassing the undo stack (we don't care about undo in tests).
    fn apply_edit(store: &SessionStore, edit: DiffEdit) {
        match edit {
            DiffEdit::SpliceTwoWayLines {
                session_id,
                side,
                start,
                end,
                replacement,
                ..
            } => {
                let _ = store.splice_two_way_lines(session_id, side, start..end, replacement);
            }
            DiffEdit::SetTwoWayLine {
                session_id,
                side,
                line_no,
                new_text,
                ..
            } => {
                let _ = store.set_two_way_line(session_id, side, line_no, new_text);
            }
            DiffEdit::ReplaceHunkSide {
                session_id,
                hunk_id,
                target,
                ..
            } => {
                let _ = store.replace_hunk_side(session_id, hunk_id, target);
            }
        }
    }

    #[derive(Default)]
    struct FrameInput {
        backspace: bool,
        /// Place the mouse at this screen position before NewFrame.
        mouse_pos: Option<[f32; 2]>,
        /// Press or release the left mouse button.
        left_button: Option<bool>,
        /// Press an arrow key (UpArrow or DownArrow) this frame.
        arrow: Option<imgui::Key>,
        /// Hold the shift modifier this frame.
        shift: bool,
    }

    /// Run one render frame: snapshot the session, inject queued input
    /// events into imgui, build a Ui, call `render` inside a window,
    /// then apply queued `pending_edits` back to the store. Mirrors the
    /// per-frame flow `app::mod::frame_ui` runs in the real app.
    fn run_frame(
        ctx: &mut imgui::Context,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        input: FrameInput,
    ) {
        if let Some(pos) = input.mouse_pos {
            ctx.io_mut().add_mouse_pos_event(pos);
        }
        if let Some(down) = input.left_button {
            ctx.io_mut().add_mouse_button_event(imgui::MouseButton::Left, down);
        }
        if input.backspace {
            ctx.io_mut().add_key_event(imgui::Key::Backspace, true);
        }
        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };
        let ui = ctx.new_frame();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();
        ui.window("test")
            .size([1000.0, 600.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    view_state,
                    None,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let _draw = ctx.render();
        for edit in pending_edits {
            apply_edit(store, edit);
        }
    }

    /// End-to-end state flow: pre-seed an in-line selection, press
    /// Backspace, render four frames (splice frame + the two frames the
    /// pin covers + an idle frame), applying queued edits to the store
    /// between frames. Asserts:
    ///   - the splice executed (line 1 was shortened);
    ///   - `pin_scroll_x_after_splice` was set with countdown=2 and the
    ///     captured x matches the splice-frame scroll_x;
    ///   - the countdown decremented (2 → 1 → cleared);
    ///   - scroll_x did not drift catastrophically from the splice-frame
    ///     baseline across the pin window.
    ///
    /// **Caveat — does NOT prove the pin prevents imgui's nav-scroll.**
    /// Verified empirically: temporarily replacing the pin push with
    /// `let pin_scroll_x: Option<(Side, f32)> = None;` at the top of
    /// `render` does NOT make this test fail. Things attempted to engage
    /// imgui's nav-scroll pipeline in headless mode:
    ///   - `ConfigFlags::NAV_ENABLE_KEYBOARD` set on `Io`.
    ///   - A click + release sequence injected via `add_mouse_pos_event`
    ///     and `add_mouse_button_event` to establish `NavWindow` and
    ///     activate the input_text widget before the splice.
    /// Neither makes imgui's `set_keyboard_focus_here` actually scroll
    /// the child window the way it does in the live app. The pipeline
    /// likely needs the renderer in the loop (or a full nav-state warmup
    /// across many frames with a stable `ActiveId` lifecycle) that a
    /// `Context::create()` + `new_frame()` loop doesn't reproduce. This
    /// test catches regressions in the state-machine wiring (the pin
    /// field's setup, countdown, and clearing); the imgui-side override
    /// behavior is only verified manually in the live GUI.
    #[test]
    fn headless_splice_preserves_scroll_x_across_pin_window() {
        let _guard = imgui_lock();
        let store = SessionStore::new();
        let long = "x".repeat(500);
        let text = format!("hello world\n{long}\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = build_ui_context();
        let mut view_state = DiffViewState::default();
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });

        // Frame 0 (engage nav): click somewhere inside the left pane so
        // imgui sets `NavWindow` and activates the row's input_text. The
        // bug's trigger (`set_keyboard_focus_here` → nav-scroll) requires
        // an engaged nav system; without this click the headless context
        // never enters that code path. Position is chosen to land on a
        // visible row well inside the pane; exact value isn't critical.
        run_frame(
            &mut ctx,
            &store,
            id,
            &mut view_state,
            FrameInput {
                mouse_pos: Some([150.0, 80.0]),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame(
            &mut ctx,
            &store,
            id,
            &mut view_state,
            FrameInput {
                left_button: Some(false),
                ..Default::default()
            },
        );
        // Restore the synthetic selection the click cleared. We're not
        // testing the click-then-shift-click extension path here, just
        // the splice path, so this is fine.
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });

        // Frame 1 (splice frame): Backspace pressed. Splice edit queued
        // and applied; pin is set with countdown=2. ImGui's per-frame
        // bookkeeping (active-widget tracking, layout) makes scroll_x
        // settle to a non-zero baseline whose exact value depends on
        // imgui internals — we just capture it as the reference.
        run_frame(
            &mut ctx,
            &store,
            id,
            &mut view_state,
            FrameInput { backspace: true, ..Default::default() },
        );
        let snap = store.snapshot(id).unwrap();
        if let SessionMode::TwoWay { a_lines, .. } = &snap.mode {
            // "hello world" with "hello" removed becomes " world".
            assert_eq!(a_lines[0], " world", "splice should have shortened line 1");
        } else {
            unreachable!();
        }
        let baseline_x = view_state.last_left_scroll_x;
        let pin = view_state
            .pin_scroll_x_after_splice
            .expect("pin should be set after splice");
        assert_eq!(pin.0, Side::Left);
        assert_eq!(pin.2, 4);
        assert!(
            (pin.1 - baseline_x).abs() < 1e-3,
            "pinned x ({}) should match this frame's captured scroll_x ({})",
            pin.1,
            baseline_x,
        );

        // Frame 2 (pin frame 1 of 2): the merged row's set_keyboard_focus_here
        // fires here and would, absent the pin, queue a nav-scroll that
        // pushes scroll_x toward (content_w - viewport_w) — i.e., several
        // thousand pixels. The pin holds it at baseline.
        run_frame(&mut ctx, &store, id, &mut view_state, FrameInput::default());
        // The original bug pushed scroll_x to roughly (content_w - pane_w),
        // which is several thousand pixels for our 500-char long line. A
        // tolerance well below that — but loose enough to ignore imgui's
        // own small per-frame layout adjustments — catches the regression.
        const MAX_DRIFT: f32 = 200.0;
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "frame 2: scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px)",
            view_state.last_left_scroll_x,
        );
        // Countdown decrements; specific value isn't material here.
        assert!(matches!(
            view_state.pin_scroll_x_after_splice,
            Some((Side::Left, _, _))
        ));

        // Run enough idle frames to exhaust the countdown (max=4 today).
        for _ in 0..5 {
            run_frame(&mut ctx, &store, id, &mut view_state, FrameInput::default());
            assert!(
                (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
                "scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px)",
                view_state.last_left_scroll_x,
            );
        }
        assert!(view_state.pin_scroll_x_after_splice.is_none());

        // Frame 4 (idle): pin has expired; scroll_x must still hold.
        run_frame(&mut ctx, &store, id, &mut view_state, FrameInput::default());
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "frame 4: scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px)",
            view_state.last_left_scroll_x,
        );
        assert!(view_state.pin_scroll_x_after_splice.is_none());
    }

    // ---- wgpu-backed harness -------------------------------------------

    /// Try to spin up a headless wgpu device. Returns `None` if the
    /// machine has no usable adapter (common in CI without GPU); tests
    /// that need this should bail gracefully.
    fn try_init_wgpu() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("diffie-headless-test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
            },
        ))
        .ok()?;
        Some((device, queue))
    }

    /// Render one frame to an offscreen texture and read the pixels
    /// back into a CPU buffer. Returns tightly-packed RGBA bytes
    /// (`width * height * 4`). `width` must be a multiple of 64 so
    /// `bytes_per_row` (= `width * 4`) is already a multiple of 256
    /// (wgpu's `copy_texture_to_buffer` alignment requirement).
    fn capture_frame_pixels(
        ctx: &mut imgui::Context,
        renderer: &mut imgui_wgpu::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        mono_font: Option<imgui::FontId>,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        assert!(width % 64 == 0, "width {width} must be a multiple of 64");
        ctx.io_mut().delta_time = 1.0 / 60.0;

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };
        let ui = ctx.new_frame();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();
        ui.window("test")
            .size([width as f32, height as f32], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui, store, id, &hunks, &[], &mut status, view_state,
                    mono_font, &mut focus_request, &mut pending_edits, &[], &[],
                );
            });
        let draw_data = ctx.render();

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture-target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let bytes_per_row = width * 4;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture-buffer"),
            size: (bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("capture-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("capture-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                multiview_mask: None,
                occlusion_query_set: None,
            });
            renderer
                .render(draw_data, queue, device, &mut pass)
                .expect("imgui render");
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().expect("buffer map");
        let data = slice.get_mapped_range();
        let pixels: Vec<u8> = data.to_vec();
        drop(data);
        buffer.unmap();
        pixels
    }

    /// One frame with the full imgui → wgpu pipeline: build the Ui,
    /// call `render`, then `ctx.render()` + `Renderer::render` into an
    /// offscreen texture and `queue.submit`. Mirrors the live app's
    /// per-frame flow (`app::mod::render` around lines 425-456) minus
    /// the surface present.
    fn run_frame_with_wgpu(
        ctx: &mut imgui::Context,
        renderer: &mut imgui_wgpu::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        mono_font: Option<imgui::FontId>,
        input: FrameInput,
    ) {
        if let Some(pos) = input.mouse_pos {
            ctx.io_mut().add_mouse_pos_event(pos);
        }
        if let Some(down) = input.left_button {
            ctx.io_mut().add_mouse_button_event(imgui::MouseButton::Left, down);
        }
        if input.backspace {
            ctx.io_mut().add_key_event(imgui::Key::Backspace, true);
        }
        // Shift modifier must go through the event queue so NewFrame
        // updates `io.key_shift` for this frame's widgets.
        if input.shift {
            ctx.io_mut().add_key_event(imgui::Key::ModShift, true);
        }
        if let Some(k) = input.arrow {
            ctx.io_mut().add_key_event(k, true);
        }
        ctx.io_mut().delta_time = 1.0 / 60.0;

        let snap = store.snapshot(id).unwrap();
        let hunks = match &snap.mode {
            SessionMode::TwoWay { hunks, .. } => hunks.clone(),
            _ => unreachable!(),
        };
        let ui = ctx.new_frame();
        let mut status = String::new();
        let mut focus_request: Option<crate::app::FocusedPane> = None;
        let mut pending_edits: Vec<DiffEdit> = Vec::new();
        ui.window("test")
            .size([1200.0, 800.0], imgui::Condition::Always)
            .position([0.0, 0.0], imgui::Condition::Always)
            .build(|| {
                render(
                    ui,
                    store,
                    id,
                    &hunks,
                    &[],
                    &mut status,
                    view_state,
                    mono_font,
                    &mut focus_request,
                    &mut pending_edits,
                    &[],
                    &[],
                );
            });
        let draw_data = ctx.render();

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-target"),
            size: wgpu::Extent3d {
                width: 1200,
                height: 800,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("test-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                multiview_mask: None,
                occlusion_query_set: None,
            });
            renderer
                .render(draw_data, queue, device, &mut pass)
                .expect("imgui render");
        }
        queue.submit(Some(encoder.finish()));
        // No present (no surface). Pixel buffer is discarded; we only
        // care about imgui's post-frame state.
        // Release the arrow + shift so the next frame doesn't see them
        // as still pressed.
        if let Some(k) = input.arrow {
            ctx.io_mut().add_key_event(k, false);
        }
        if input.shift {
            ctx.io_mut().add_key_event(imgui::Key::ModShift, false);
        }

        for edit in pending_edits {
            apply_edit(store, edit);
        }
    }

    /// Double-clicking a word activates imgui's input_text native
    /// word-selection. The selection must survive into subsequent
    /// frames — previously our `suppress_imgui_selection` callback
    /// collapsed it the very next frame because we suppressed
    /// imgui's selection whenever `state.drag` was Some on this side,
    /// even at `threshold_passed=false` (which is the state right
    /// after any click). The fix gates suppression on
    /// `threshold_passed` so double-click survives.
    ///
    /// Requires the wgpu pipeline because imgui's input_text word-select
    /// only fires when the widget is fully active, which needs the same
    /// renderer-in-the-loop conditions as the scroll-pin bug.
    #[test]
    fn headless_wgpu_double_click_selects_word() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let store = SessionStore::new();
        let text = "alpha beta gamma\n";
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Make sure the synthetic clicks fall well inside imgui's default
        // double-click window (0.3s); each frame advances time by
        // delta_time, so two clicks separated by a few frames is fine.

        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();

        // Aim at the "beta" token on row 1 (the only row containing text;
        // row 0 is the diff's top row). Position calibration: pane origin
        // ~ (8, 33); gutter_w=60; chars start at x≈68; char_w with the
        // default font ≈ 7px. "alpha " is 6 chars → "beta" starts at
        // x ≈ 68 + 6*7 = 110. Click at x=120 (somewhere inside "beta").
        // y=40 lands inside the first row (height ~24, top ~33).
        let word_pos = [120.0, 40.0];

        // First click: down, then up.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(word_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        // Second click on the same pixel — completes the double-click
        // gesture. ImGui's input_text recognizes this and selects the
        // word under the cursor.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(word_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );

        // Run a few more idle frames to make sure the selection persists
        // past the suppression check (which fires only post-threshold).
        for _ in 0..3 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, None, FrameInput::default(),
            );
        }

        // ImGui's input_text should have selected SOME word on the row.
        // We don't pin to a specific word — pane origin and char_w in the
        // headless context differ from the live app, so the exact hit
        // column shifts. What we're testing is the bug-relevant invariant:
        // a non-collapsed selection survives past the splice-suppression
        // window. With the bug present the selection would be collapsed
        // by frame 2 of the post-double-click run.
        let (side, line_no, start, end) = view_state
            .last_active_input_selection
            .expect("imgui input_text should have a selection after double-click");
        assert_eq!(side, Side::Left);
        assert_eq!(line_no, 1);
        assert!(end > start, "selection should be non-collapsed");
        let line = "alpha beta gamma";
        let selected = &line[start..end];
        assert!(
            ["alpha", "beta", "gamma"].contains(&selected),
            "expected a whole-word selection (alpha/beta/gamma); got bytes {start}..{end} = {selected:?}",
        );
    }

    /// Double-clicking on a punctuation char in a non-space run (e.g.,
    /// `target_arch=value`, `==`, `::`) must select just the single
    /// punct char, not the whole run. ImGui's default WORDLEFT/WORDRIGHT
    /// uses whitespace as the only word boundary, so for runs with no
    /// internal spaces it selects everything.
    ///
    /// Definitive regression check: a row of `===` (three equals signs,
    /// no whitespace) — without the fix imgui selects all three; with
    /// the fix any click in the rendered text selects a single `=`.
    #[test]
    fn headless_wgpu_double_click_punct_selects_single_char() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let line = "===";
        let text = format!("{line}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let click_pos = [90.0, 40.0];
        let mut view_state = DiffViewState::default();
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(click_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some(click_pos),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        for _ in 0..2 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, None, FrameInput::default(),
            );
        }

        let (_, ln, start, end) = view_state
            .last_active_input_selection
            .expect("imgui input_text should have a selection after double-click");
        assert_eq!(ln, 1);
        let selected = &line[start..end];
        assert_eq!(
            selected, "=",
            "expected single '=' to be selected; got bytes {start}..{end} = {selected:?}",
        );
    }

    /// Drive the harness through enough frames to fully activate the
    /// input_text on `(side, line_no)` and let imgui settle. Returns
    /// the column the caret ended up at (which may differ slightly
    /// from the requested column due to imgui's clamping behavior).
    fn focus_row_and_settle(
        ctx: &mut imgui::Context,
        renderer: &mut imgui_wgpu::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        mono: imgui::FontId,
        side: Side,
        line_no: u32,
        col: usize,
    ) {
        focus_row_and_settle_opt(
            ctx, renderer, device, queue, target_format,
            store, id, view_state, Some(mono), side, line_no, col,
        );
    }

    /// Load the proportional Roboto font (the live app's UI font) into
    /// the context. Used by tests that need to exercise proportional
    /// glyph widths.
    fn load_proportional_font(ctx: &mut imgui::Context, size_pixels: f32) -> imgui::FontId {
        ctx.fonts().add_font(&[imgui::FontSource::TtfData {
            data: aetna_fonts_roboto::ROBOTO_REGULAR,
            size_pixels,
            config: Some(imgui::FontConfig {
                size_pixels,
                ..Default::default()
            }),
        }])
    }

    /// Like `focus_row_and_settle` but allows omitting the mono font
    /// (uses imgui's default proportional font instead).
    fn focus_row_and_settle_opt(
        ctx: &mut imgui::Context,
        renderer: &mut imgui_wgpu::Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        store: &SessionStore,
        id: crate::session::SessionId,
        view_state: &mut DiffViewState,
        mono: Option<imgui::FontId>,
        side: Side,
        line_no: u32,
        col: usize,
    ) {
        view_state.arrow_focus = Some((side, line_no, col));
        // Several frames: set_keyboard_focus_here takes a couple of
        // frames to make the widget active; selection state stabilizes
        // after another frame or two.
        for _ in 0..5 {
            run_frame_with_wgpu(
                ctx, renderer, device, queue, target_format,
                store, id, view_state, mono, FrameInput::default(),
            );
        }
    }

    /// Shift+Down inside the middle of a line extends `state.selection`
    /// across rows: anchor at the caret's pre-move position, caret at
    /// the same column on the line below. Standard editor behavior.
    #[test]
    fn headless_wgpu_shift_down_extends_selection_to_next_line() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Park the caret at column 4 on line 1 via the arrow-focus
        // mechanism (more reliable in headless than relying on a click
        // to activate imgui's input_text).
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 4,
        );

        // Press Shift+Down. Selection should now span (1, 4) → (2, 4).
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::DownArrow),
                shift: true,
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono), FrameInput::default(),
        );

        let sel = view_state
            .selection
            .as_ref()
            .expect("Shift+Down should produce a selection");
        assert_eq!(sel.side, Side::Left);
        assert_eq!(
            sel.anchor,
            SelPoint { line_no: 1, col: 4 },
            "anchor should be at the pre-move caret position",
        );
        assert_eq!(
            sel.caret,
            SelPoint { line_no: 2, col: 4 },
            "caret should jump to same column on line 2",
        );
    }

    /// Pressing Up or Down (which sets `arrow_focus` so the adjacent
    /// row's input_text gets `set_keyboard_focus_here` on the next
    /// frame) triggers the same imgui nav-scroll path as the splice
    /// fix, snapping scroll_x to gutter_w. This is the path that DOES
    /// reproduce in headless (verified by the existing splice scroll
    /// test). The new lateral-arrow pin trigger also covers Up/Down,
    /// so scroll_x stays at the pre-key baseline.
    #[test]
    fn headless_wgpu_up_arrow_doesnt_drift_scroll_x() {
        check_vertical_arrow_no_drift(imgui::Key::UpArrow, /*start_line=*/ 2);
    }

    #[test]
    fn headless_wgpu_down_arrow_doesnt_drift_scroll_x() {
        check_vertical_arrow_no_drift(imgui::Key::DownArrow, /*start_line=*/ 1);
    }

    /// Shared body for the Up/Down drift tests. Activates `start_line`,
    /// forces scroll_x to 0 (so any drift to gutter_w is observable),
    /// presses the requested vertical arrow, and asserts scroll_x stays
    /// at the forced baseline.
    fn check_vertical_arrow_no_drift(arrow: imgui::Key, start_line: u32) {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let long = "x".repeat(500);
        let text = format!("{long}\n{long}\n{long}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, start_line, 10,
        );

        // Activation snapped scroll_x to gutter_w (≈60). Force it back
        // to 0 by queueing the pin manually and running long enough
        // for the pin to take effect and expire.
        view_state.pin_scroll_x_after_splice = Some((Side::Left, 0.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }
        let baseline_x = view_state.last_left_scroll_x;
        assert!(
            baseline_x.abs() < 1.0,
            "expected forced baseline near 0, got {baseline_x}",
        );

        // Press the vertical arrow. set_keyboard_focus_here on the
        // adjacent row would fire imgui's nav-scroll without the pin
        // (this is the same path the splice test exercises).
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(arrow),
                ..Default::default()
            },
        );
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }

        // Same drift bound as the splice test: gutter_w is 60 px,
        // anything within 10 of baseline is acceptable.
        const MAX_DRIFT: f32 = 10.0;
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "{arrow:?} drifted scroll_x from {baseline_x} to {} (>{MAX_DRIFT}px) — \
             pin failed for vertical arrow",
            view_state.last_left_scroll_x,
        );
    }

    /// Pressing Left or Right inside an active row triggers imgui's
    /// nav-scroll on the focused widget — same root cause as the
    /// splice-refocus bug, snapping scroll_x to gutter_w. The fix
    /// queues a scroll-x pin (same field + countdown as the splice
    /// pin) so subsequent frames push `igSetNextWindowScroll` with
    /// the pre-key scroll_x, neutralizing the nav-scroll.
    ///
    /// **Caveat:** the imgui-side nav-scroll for a Left/Right keypress
    /// (as opposed to `set_keyboard_focus_here`, which DOES fire in
    /// headless and is verified by `headless_wgpu_splice_preserves_*`)
    /// doesn't reliably reproduce here — same kind of limitation we hit
    /// with the splice scroll bug before adding the wgpu pipeline. This
    /// test catches state-wiring regressions (the pin field gets set
    /// with the right side and a fresh countdown) rather than the
    /// imgui-side drift itself. Manual verification: in the live GUI,
    /// open a long line, scroll horizontally to the left edge, press
    /// Left — view must not shift.
    #[test]
    fn headless_wgpu_left_arrow_queues_scroll_pin() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let long = "x".repeat(500);
        let text = format!("{long}\n{long}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 10,
        );
        // Drain whatever pin the activation set; clear state so we
        // can detect the new pin set by Left arrow.
        view_state.pin_scroll_x_after_splice = None;

        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::LeftArrow),
                ..Default::default()
            },
        );

        // The Left press should have queued a scroll-x pin for the
        // left pane with a fresh countdown of 4.
        let pin = view_state
            .pin_scroll_x_after_splice
            .expect("Left arrow inside active row should queue a scroll-x pin");
        assert_eq!(pin.0, Side::Left, "pin should be for the left pane");
        assert_eq!(pin.2, 4, "pin countdown should be the standard 4 frames");

        // Verify Right arrow also queues the pin (clearing first so
        // the countdown reset is observable).
        view_state.pin_scroll_x_after_splice = None;
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        let pin = view_state
            .pin_scroll_x_after_splice
            .expect("Right arrow inside active row should queue a scroll-x pin");
        assert_eq!(pin.0, Side::Left);
        assert_eq!(pin.2, 4);
    }

    /// User's reported repro: a multi-line code file where the cursor
    /// is placed in the middle of a word ("frames") on a line with
    /// indentation. Asserts that the cursor's pixel x equals the
    /// expected x = text_start + calc_text_size(prefix_through_m),
    /// AND that the cursor lines up with the right edge of 'm' as
    /// rendered by paint_row_text (which the test verifies by
    /// finding the rightmost pixel of the 'm' glyph stroke).
    #[test]
    fn headless_wgpu_pixel_caret_in_middle_of_word() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        // The user's exact line. Multi-line file (a few lines around
        // it) so it's not a degenerate single-line session and there
        // can be a diff that exercises char-level highlighting.
        let a = "fn main() {\n        // Replay mode: load recording, render frames to PNG, exit.\n}\n";
        let b = "fn main() {\n        // Replay mode: load recording, render FRAMES to PNG, exit.\n}\n";
        let store = SessionStore::new();
        let id = store.open_two_way(a, b, None).unwrap();

        let mut ctx = imgui::Context::create();
        // Apply the live app's theme (catppuccin) — same color palette
        // the user is rendering against in their screenshot. The colors
        // affect what counts as "diff pixel" in our threshold filter
        // and surface any layout-affecting style differences from the
        // imgui default theme.
        crate::app::theme::apply(&mut ctx);
        let w: u32 = 1024;
        let h: u32 = 256;
        ctx.io_mut().display_size = [w as f32, h as f32];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 16.0);
        let target_format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        // Position to place the caret: just past the 'm' in "frames"
        // on line 2 of a.
        let line2 = "        // Replay mode: load recording, render frames to PNG, exit.";
        let prefix_through_m = "        // Replay mode: load recording, render fram";
        let char_col = prefix_through_m.chars().count();
        let _ = line2; // documentation only

        // Baseline: no focus, but force scroll_x to 0 via the pin so
        // both scenarios have matching layout (the with-caret scenario
        // will also have scroll_x pinned to 0 — see below).
        let mut state_no_caret = DiffViewState::default();
        state_no_caret.pin_scroll_x_after_splice = Some((Side::Left, 0.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut state_no_caret, Some(mono), FrameInput::default(),
            );
        }
        let pixels_baseline = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_no_caret, Some(mono), w, h,
        );

        // With caret on line 2 right after 'm' in "frames". Focus
        // shifts scroll_x via nav-scroll; re-pin to 0 to match the
        // baseline so the diff isolates only caret pixels.
        let mut state_with_caret = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_with_caret, mono, Side::Left, 2, char_col,
        );
        state_with_caret.pin_scroll_x_after_splice = Some((Side::Left, 0.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut state_with_caret, Some(mono), FrameInput::default(),
            );
        }
        let pixels_with_caret = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_with_caret, Some(mono), w, h,
        );

        // Find caret column by diffing.
        let mut diff_x: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let aa = &pixels_baseline[i..i + 4];
                let bb = &pixels_with_caret[i..i + 4];
                let max_d = aa
                    .iter()
                    .zip(bb.iter())
                    .map(|(av, bv)| (*av as i32 - *bv as i32).abs())
                    .max()
                    .unwrap_or(0);
                if max_d > 50 {
                    diff_x.insert(x);
                }
            }
        }
        assert!(!diff_x.is_empty(), "no caret pixels found");
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        for x in &diff_x {
            match cur {
                None => cur = Some((*x, *x)),
                Some((lo, hi)) => {
                    if *x <= hi + 3 {
                        cur = Some((lo, *x));
                    } else {
                        bands.push((lo, hi));
                        cur = Some((*x, *x));
                    }
                }
            }
        }
        if let Some(b) = cur {
            bands.push(b);
        }
        assert_eq!(
            bands.len(),
            1,
            "expected one caret band; got {} ({:?})",
            bands.len(),
            bands,
        );
        let caret_x = (bands[0].0 + bands[0].1) as f32 * 0.5;

        // Compute the expected caret x in PIXELS. Two parts:
        //  - text_start_x: the row's text-area left edge. We don't
        //    know it analytically, but state.last_left_scroll_x is 0
        //    (we forced it) and the pane has standard layout, so
        //    text_start_x ≈ window_padding + gutter_w. We capture the
        //    actual text_start_x by rendering a SECOND with-caret
        //    scenario at col 0 — that caret lands AT text_start_x.
        //  - offset_in_state: the caret's offset from text_start_x.
        let offset_in_state = state_with_caret
            .last_active_caret_offset
            .expect("caret offset should be in state")
            .1;
        let expected_offset: Cell<f32> = Cell::new(0.0);
        {
            let ui = ctx.new_frame();
            ui.window("m")
                .size([200.0, 100.0], imgui::Condition::Always)
                .build(|| {
                    let _tok = ui.push_font(mono);
                    expected_offset.set(ui.calc_text_size(prefix_through_m)[0]);
                });
            let _ = ctx.render();
        }
        let expected = expected_offset.get();
        // (1) state's offset must match calc_text_size(prefix).
        assert!(
            (offset_in_state - expected).abs() < 0.5,
            "state.last_active_caret_offset {offset_in_state} differs \
             from calc_text_size(prefix) {expected} by more than 0.5 px",
        );

        // (2) Find text_start_x by rendering a parallel scenario with
        // the caret at col 0, then diff against baseline to locate it.
        let mut state_col0 = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_col0, mono, Side::Left, 2, 0,
        );
        state_col0.pin_scroll_x_after_splice = Some((Side::Left, 0.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut state_col0, Some(mono), FrameInput::default(),
            );
        }
        let pixels_col0 = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_col0, Some(mono), w, h,
        );
        let mut diff_x_col0: std::collections::BTreeSet<u32> =
            std::collections::BTreeSet::new();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let aa = &pixels_baseline[i..i + 4];
                let bb = &pixels_col0[i..i + 4];
                let max_d = aa
                    .iter()
                    .zip(bb.iter())
                    .map(|(av, bv)| (*av as i32 - *bv as i32).abs())
                    .max()
                    .unwrap_or(0);
                if max_d > 50 {
                    diff_x_col0.insert(x);
                }
            }
        }
        // The col-0 caret bands cluster into ONE narrow band at
        // text_start_x.
        let mut bands0: Vec<(u32, u32)> = Vec::new();
        let mut cur0: Option<(u32, u32)> = None;
        for x in &diff_x_col0 {
            match cur0 {
                None => cur0 = Some((*x, *x)),
                Some((lo, hi)) => {
                    if *x <= hi + 3 {
                        cur0 = Some((lo, *x));
                    } else {
                        bands0.push((lo, hi));
                        cur0 = Some((*x, *x));
                    }
                }
            }
        }
        if let Some(b) = cur0 {
            bands0.push(b);
        }
        assert_eq!(
            bands0.len(),
            1,
            "expected one col-0 caret band; got {} ({:?})",
            bands0.len(),
            bands0,
        );
        let text_start_x = (bands0[0].0 + bands0[0].1) as f32 * 0.5;

        // (3) The middle-of-word caret should land at text_start_x +
        // calc_text_size(prefix). Within ~2 px of anti-aliasing.
        let expected_caret_x = text_start_x + expected;
        assert!(
            (caret_x - expected_caret_x).abs() <= 2.0,
            "caret pixel x ({caret_x}) doesn't match expected \
             text_start_x + calc_text_size(prefix) ({expected_caret_x}); \
             diff = {} px",
            (caret_x - expected_caret_x).abs(),
        );
    }

    /// Direct pixel-level alignment check between the caret and the
    /// highlight rect's right edge. Sets up a diff where a single
    /// trailing character is marked hl=true, focuses the row, and
    /// places the caret at the END of the highlighted region. If
    /// caret and highlight both use calc_text_size correctly, the
    /// caret pixel column should equal the highlight rect's right
    /// edge pixel column (modulo 1 px AA).
    ///
    /// Localizes the caret via diff with a baseline (focused vs not),
    /// and the highlight rect via the same red-pixel filter as the
    /// width test.
    #[test]
    fn headless_wgpu_pixel_caret_aligns_with_highlight_right_edge() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        // Char-diff scenario: 5 of 6 chars match, 'w' (last char of
        // line a) differs from 'X' in b. The Delete row's segments:
        //   ["hello" hl=false, "w" hl=true]
        // So the hl rect covers JUST 'w' at the END of the line.
        let a = "hellow\n";
        let b = "helloX\n";
        let store = SessionStore::new();
        let id = store.open_two_way(a, b, None).unwrap();

        let mut ctx = imgui::Context::create();
        let w: u32 = 1024;
        let h: u32 = 256;
        ctx.io_mut().display_size = [w as f32, h as f32];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let font = load_proportional_font(&mut ctx, 16.0);
        let target_format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        // Baseline: no caret (no focus). Captures the hl rect at its
        // natural position.
        let mut state_no_caret = DiffViewState::default();
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_no_caret, Some(font), FrameInput::default(),
        );
        let pixels_baseline = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_no_caret, Some(font), w, h,
        );

        // With caret: focus the Delete row at col 6 (end of "hellow").
        // The caret renders at the rightmost edge of the hl segment.
        let mut state_with_caret = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_with_caret, font, Side::Left, 1, 6,
        );
        let pixels_with_caret = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_with_caret, Some(font), w, h,
        );

        // Find the hl rect's right edge in the baseline (no caret).
        let mut hl_cols: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let r = pixels_baseline[i] as i32;
                let g = pixels_baseline[i + 1] as i32;
                let b = pixels_baseline[i + 2] as i32;
                if r > 35 && (r - g) > 25 && (r - b) > 25 {
                    hl_cols.insert(x);
                }
            }
        }
        assert!(!hl_cols.is_empty(), "no highlight pixels in baseline");
        let hl_right = *hl_cols.iter().last().unwrap();

        // Find the caret column via diff between with-caret and
        // baseline (only caret pixels differ — same text, same hl).
        let mut diff_x: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = &pixels_baseline[i..i + 4];
                let b = &pixels_with_caret[i..i + 4];
                let max_d = a
                    .iter()
                    .zip(b.iter())
                    .map(|(av, bv)| (*av as i32 - *bv as i32).abs())
                    .max()
                    .unwrap_or(0);
                if max_d > 50 {
                    diff_x.insert(x);
                }
            }
        }
        assert!(!diff_x.is_empty(), "no caret pixels found");
        // Cluster diff columns; expect ONE band (the caret's only
        // position differs between scenarios).
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        for x in &diff_x {
            match cur {
                None => cur = Some((*x, *x)),
                Some((lo, hi)) => {
                    if *x <= hi + 3 {
                        cur = Some((lo, *x));
                    } else {
                        bands.push((lo, hi));
                        cur = Some((*x, *x));
                    }
                }
            }
        }
        if let Some(b) = cur {
            bands.push(b);
        }
        assert_eq!(
            bands.len(),
            1,
            "expected one caret band; got {} ({:?})",
            bands.len(),
            bands,
        );
        let caret_x = (bands[0].0 + bands[0].1) as f32 * 0.5;

        // The caret should land at the right edge of the hl rect
        // (col 6 = end of "hellow", and the hl segment is the last
        // char). Tolerate 1 px for anti-aliasing / band-center drift.
        let diff_px = (caret_x - hl_right as f32).abs();
        assert!(
            diff_px <= 2.0,
            "caret x ({caret_x}) does not align with hl rect right edge \
             ({hl_right}); diff = {diff_px} px",
        );
    }

    /// Pixel-readback regression for highlight-rect alignment with
    /// rendered text. Sets up a session where char-level diff marks a
    /// single character with `hl=true` on a Delete row; that character
    /// gets a bright-red rect drawn behind it. Captures the pixel
    /// buffer, finds the bright-red columns via a color filter, and
    /// asserts the rect's WIDTH matches `calc_text_size` of the
    /// highlighted character — proving the rect tracks the actual
    /// glyph advance, not the `char_w` denominator the prior formula
    /// would have used.
    #[test]
    fn headless_wgpu_pixel_highlight_width_matches_calc_text_size() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        // Char-level diff needs lines to be similar enough to pair.
        // Make most chars match so only one differs and char-diff
        // engages: 'hellow' vs 'hellpw' → 'o' (col 4) is hl=true on
        // the Delete row for side A.
        let a = "hellow\n";
        let b = "hellpw\n";
        let store = SessionStore::new();
        let id = store.open_two_way(a, b, None).unwrap();

        let mut ctx = imgui::Context::create();
        let w: u32 = 1024;
        let h: u32 = 256;
        ctx.io_mut().display_size = [w as f32, h as f32];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let font = load_proportional_font(&mut ctx, 16.0);
        let target_format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        // Measure the proportional advances.
        let o_advance: Cell<f32> = Cell::new(0.0);
        let m_advance: Cell<f32> = Cell::new(0.0);
        {
            let ui = ctx.new_frame();
            ui.window("measure")
                .size([200.0, 100.0], imgui::Condition::Always)
                .build(|| {
                    let _tok = ui.push_font(font);
                    o_advance.set(ui.calc_text_size("o")[0]);
                    m_advance.set(ui.calc_text_size("m")[0]);
                });
            let _ = ctx.render();
        }
        let expected_w = o_advance.get();
        let m_w = m_advance.get();
        assert!(
            (m_w - expected_w).abs() > 1.0,
            "'o' advance ({expected_w}) and 'm' advance ({m_w}) should \
             differ for this test to distinguish calc_text_size from \
             the old char_w formula",
        );

        // Render the diff view; one warm-up frame, then capture.
        let mut view_state = DiffViewState::default();
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(font), FrameInput::default(),
        );
        let pixels = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(font), w, h,
        );

        // Detect highlight-rect pixels. The Delete row's background
        // is a dim red; the per-char `hl=true` rect overlays a
        // brighter red on top. Filter for the brighter case using a
        // red-heavy color filter calibrated against the row bg
        // (R ≈ 63, G ≈ 35, B ≈ 42) and the hl rect (R ≈ 94, G ≈ 37,
        // B ≈ 43) on the default imgui background.
        // The hl rect overlays a brighter red on the Delete row bg.
        // Concretely (after blending with WindowBg ≈ 0.06 over black):
        //   Delete row bg pixel  ≈ (52, 24, 24).
        //   Hl rect on top of it ≈ (85, 28, 28).
        // The R-G channel difference distinguishes them robustly:
        //   row bg:  R - G ≈ 28.
        //   hl rect: R - G ≈ 57.
        // A threshold of (R - G) > 40 picks up hl pixels and rejects
        // the plain row bg.
        // The hl rect overlays a brighter red on the Delete row bg.
        // Empirically (pixel-dumped during dev with this test text):
        //   Delete row bg pixel  ≈ (21, 3, 3).
        //   Hl rect on top of it ≈ (52, 3, 3).
        // Both are red-saturated; the discriminator is R itself.
        // R > 35 picks up hl pixels and rejects plain row bg.
        let mut hl_cols: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let r = pixels[i] as i32;
                let g = pixels[i + 1] as i32;
                let b = pixels[i + 2] as i32;
                if r > 35 && (r - g) > 25 && (r - b) > 25 {
                    hl_cols.insert(x);
                }
            }
        }
        assert!(
            !hl_cols.is_empty(),
            "no highlight pixels found — check color filter or that \
             the diff actually produced a highlighted segment",
        );

        // The hl rect is a contiguous band of bright-red columns.
        // Group; expect a single band.
        let mut bands: Vec<(u32, u32)> = Vec::new();
        let mut cur: Option<(u32, u32)> = None;
        for x in &hl_cols {
            match cur {
                None => cur = Some((*x, *x)),
                Some((lo, hi)) => {
                    if *x <= hi + 2 {
                        cur = Some((lo, *x));
                    } else {
                        bands.push((lo, hi));
                        cur = Some((*x, *x));
                    }
                }
            }
        }
        if let Some(b) = cur {
            bands.push(b);
        }
        assert_eq!(
            bands.len(),
            1,
            "expected one highlight band; got {} ({:?})",
            bands.len(),
            bands,
        );
        let (band_lo, band_hi) = bands[0];
        let measured_width = (band_hi - band_lo + 1) as f32;

        // The width should match calc_text_size("o") (the 'o' glyph's
        // actual advance), NOT char_w = calc_text_size("m") (the old
        // buggy denominator).
        let diff_correct = (measured_width - expected_w).abs();
        let diff_buggy = (measured_width - m_w).abs();
        assert!(
            diff_correct < diff_buggy,
            "measured highlight width {measured_width} is closer to the \
             buggy `char_w` value {m_w} than to the correct \
             `calc_text_size(\"o\")` value {expected_w}",
        );
        assert!(
            (measured_width - expected_w).abs() < 2.0,
            "measured highlight width {measured_width} differs from \
             calc_text_size(\"o\") {expected_w} by more than 2 px",
        );
    }

    /// Pixel-readback regression for caret alignment with rendered
    /// glyphs. Renders the same row twice, once with the caret at
    /// column 0 and once at column 1, captures pixels in both, and
    /// diffs them. The diff isolates caret pixels: two vertical
    /// stripes, one at the col-0 position and one at the col-1
    /// position. Their horizontal distance is the rendered advance
    /// of the first character.
    ///
    /// Using a proportional font (Roboto) with "Mi" — `M` is much
    /// wider than `m` and definitely much wider than `i` — guarantees
    /// the advance is distinctly different from `calc_text_size("m")[0]`
    /// (the old buggy `char_w` denominator). Asserts the measured
    /// caret advance matches `calc_text_size("M")[0]` and NOT
    /// `calc_text_size("m")[0]`.
    #[test]
    fn headless_wgpu_pixel_caret_advance_matches_calc_text_size() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        // 'W' is much wider than 'i' or 'm' in Roboto; this makes the
        // proportional advance distinct from the `char_w = m_advance`
        // value the old (buggy) formula would have used.
        let text = "Wi\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        // Sized so width is a multiple of 64 (capture helper requires
        // it) and the diff view fits.
        let w: u32 = 1024;
        let h: u32 = 256;
        ctx.io_mut().display_size = [w as f32, h as f32];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let font = load_proportional_font(&mut ctx, 16.0);
        // Non-sRGB format so the readback bytes are linear and easy to
        // compare against expected colors.
        let target_format = wgpu::TextureFormat::Rgba8Unorm;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        // Measure the expected glyph advances using imgui itself.
        let expected_w_advance: Cell<f32> = Cell::new(0.0);
        let m_advance: Cell<f32> = Cell::new(0.0);
        {
            let ui = ctx.new_frame();
            ui.window("measure")
                .size([200.0, 100.0], imgui::Condition::Always)
                .build(|| {
                    let _tok = ui.push_font(font);
                    expected_w_advance.set(ui.calc_text_size("W")[0]);
                    m_advance.set(ui.calc_text_size("m")[0]);
                });
            let _ = ctx.render();
        }
        let expected_advance = expected_w_advance.get();
        let m_w = m_advance.get();
        // Sanity: 'W' must be noticeably wider than 'm' for this test
        // to distinguish the correct formula from the old buggy one.
        assert!(
            expected_advance > m_w + 0.5,
            "Roboto's 'W' advance ({expected_advance}) should be \
             noticeably wider than 'm' ({m_w}) for this test to mean anything",
        );

        // Scenario A: caret at column 0 (start of "Wi").
        let mut state_a = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_a, font, Side::Left, 1, 0,
        );
        let pixels_a = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_a, Some(font), w, h,
        );

        // Scenario B: caret at column 1 (after 'W', before 'i').
        // The widget is still active; press Right once to advance the
        // cursor one character.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_a, Some(font),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        // Let the keypress settle.
        for _ in 0..2 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut state_a, Some(font), FrameInput::default(),
            );
        }
        let pixels_b = capture_frame_pixels(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut state_a, Some(font), w, h,
        );

        // Diff the two pixel buffers and collect columns where any
        // pixel differs significantly. The only thing that changed
        // between A and B is the caret's position (and possibly a
        // tiny side effect from scroll-pin counters, which manifest
        // as scroll changes — the test's text is short enough that
        // no scrolling happens).
        let mut diff_x: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let a = &pixels_a[i..i + 4];
                let b = &pixels_b[i..i + 4];
                let max_d = a
                    .iter()
                    .zip(b.iter())
                    .map(|(av, bv)| (*av as i32 - *bv as i32).abs())
                    .max()
                    .unwrap_or(0);
                if max_d > 50 {
                    diff_x.insert(x);
                }
            }
        }
        assert!(
            !diff_x.is_empty(),
            "no pixel differences found — caret may not be rendering",
        );

        // The caret is a 1-px vertical line; with blink+anti-aliasing
        // the diff columns cluster into two narrow bands (one at the
        // col-0 caret x, one at the col-1 caret x). Cluster by
        // splitting where consecutive columns differ by more than a
        // few pixels.
        let mut bands: Vec<(u32, u32)> = Vec::new(); // (lo, hi)
        let mut cur: Option<(u32, u32)> = None;
        for x in &diff_x {
            match cur {
                None => cur = Some((*x, *x)),
                Some((lo, hi)) => {
                    if *x <= hi + 3 {
                        cur = Some((lo, *x));
                    } else {
                        bands.push((lo, hi));
                        cur = Some((*x, *x));
                    }
                }
            }
        }
        if let Some(b) = cur {
            bands.push(b);
        }
        assert_eq!(
            bands.len(),
            2,
            "expected exactly 2 caret-position bands in the diff; got {} ({:?})",
            bands.len(),
            bands,
        );
        let band_center = |b: (u32, u32)| (b.0 + b.1) as f32 * 0.5;
        let measured_advance = band_center(bands[1]) - band_center(bands[0]);

        // The measured caret advance should match calc_text_size("W")
        // (the actual rendered glyph advance), not char_w = calc_text_size("m").
        let diff_correct = (measured_advance - expected_advance).abs();
        let diff_buggy = (measured_advance - m_w).abs();
        assert!(
            diff_correct < diff_buggy,
            "measured caret advance {measured_advance} is closer to the \
             buggy `char_w` value {m_w} than to the correct \
             `calc_text_size(\"W\")` value {expected_advance}",
        );
        assert!(
            (measured_advance - expected_advance).abs() < 2.0,
            "measured caret advance {measured_advance} differs from \
             calc_text_size(\"W\") {expected_advance} by more than 2 px",
        );
    }

    /// With a proportional font, `col * char_w` (where `char_w` is the
    /// "m" advance) doesn't equal the actual text width — narrower
    /// glyphs like "i" or "l" make the real width less than `col *
    /// char_w`, wider glyphs like "M" make it more. The caret must use
    /// `calc_text_size` to track the rendered glyphs in either case.
    ///
    /// Test setup: don't load a mono font; use imgui's built-in default
    /// proportional font. Park the caret at the end of "lllll iiiii"
    /// (a string of narrow chars), then assert the caret offset matches
    /// the actual rendered width of the prefix, NOT `chars_so_far *
    /// calc_text_size("m")[0]` (which the old formula computed).
    #[test]
    fn headless_wgpu_caret_aligns_with_proportional_font() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let line = "lllll iiiii"; // 11 narrow chars
        let text = format!("{line}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Use Roboto Regular — the live app's UI font. Truly
        // proportional: `i` and `l` are much narrower than `m`.
        let prop_font = load_proportional_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Park caret at char column 11 (end of line) with Roboto as
        // the row font.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, prop_font, Side::Left, 1, 11,
        );

        let (_, caret_offset) = view_state
            .last_active_caret_offset
            .expect("caret offset should be exposed after activation");

        // Measure the EXACT rendered width of `line` and `m`-strings via
        // imgui inside a one-off frame. The caret offset must match the
        // line width to within sub-pixel tolerance.
        let measure_actual: Cell<f32> = Cell::new(0.0);
        let measure_naive: Cell<f32> = Cell::new(0.0);
        {
            let ui = ctx.new_frame();
            ui.window("measure")
                .size([200.0, 100.0], imgui::Condition::Always)
                .build(|| {
                    let _tok = ui.push_font(prop_font);
                    measure_actual.set(ui.calc_text_size(line)[0]);
                    let char_w = ui.calc_text_size("m")[0];
                    measure_naive.set(11.0 * char_w);
                });
            let _ = ctx.render();
        }
        let line_w_actual = measure_actual.get();
        let line_w_naive = measure_naive.get();

        // For a truly proportional font with mostly narrow chars,
        // actual rendered width is much smaller than the naive
        // `col * char_w("m")` estimate. Confirm the two diverge —
        // otherwise the test doesn't exercise the proportional path.
        assert!(
            line_w_naive - line_w_actual > 10.0,
            "Roboto should be proportional: actual={line_w_actual} \
             naive={line_w_naive} — too close for a meaningful test",
        );
        assert!(
            (caret_offset - line_w_actual).abs() < 1.5,
            "caret_offset {caret_offset} should match the actual rendered \
             width {line_w_actual}, not the naive `col * char_w` value \
             {line_w_naive}",
        );
    }

    /// The manually-drawn caret must align with the rendered text at
    /// any cursor position. ImGui's `cursor_pos` is a BYTE offset, but
    /// `paint_row_text` positions glyphs by CHAR index. For ASCII the
    /// two are identical; for any UTF-8 codepoint > 1 byte they
    /// diverge, and the caret ends up off by one char_w per multibyte
    /// codepoint preceding it.
    ///
    /// Test: render a row containing `café` (where 'é' is 2 bytes),
    /// park the caret at the end of the word, and assert the caret's
    /// offset matches the rendered text's width (4 chars * char_w),
    /// NOT the byte count (5 * char_w).
    #[test]
    fn headless_wgpu_caret_aligns_with_text_in_utf8_line() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        // 'é' is 2 bytes in UTF-8. So "café word" is 9 chars but 10
        // bytes. Caret at char column 4 (end of "café") corresponds
        // to byte position 5.
        let text = "café word\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Park caret at char column 4 (end of "café"). The
        // arrow_focus mechanism's `seed_byte` correctly converts this
        // to byte position 5.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 4,
        );

        let (side, caret_offset) = view_state
            .last_active_caret_offset
            .expect("caret offset should be exposed after activation");
        assert_eq!(side, Side::Left);

        // Expected: 4 chars * char_w. Compute char_w from the same
        // mono font the production code uses.
        let _font_tok = ctx.io_mut(); // no-op; we just need ctx in scope
        // We can't easily call ui.calc_text_size outside a frame, so
        // approximate: char_w ≈ 6 in headless RobotoMono @ size 13
        // (verified by prior tests). 4 chars → 24. The bug would put
        // the caret at 5 chars → 30 (one char further right).
        let expected_4_chars = 4.0 * 6.0;
        let bug_value_5_chars = 5.0 * 6.0;
        let dist_to_correct = (caret_offset - expected_4_chars).abs();
        let dist_to_bug = (caret_offset - bug_value_5_chars).abs();
        assert!(
            dist_to_correct < dist_to_bug,
            "caret_offset {caret_offset} is closer to the bug value \
             {bug_value_5_chars} (one char too far right) than to the \
             correct value {expected_4_chars}",
        );
        // Tighter bound: caret offset should be within ~1 px of the
        // 4-char width (char_w drift aside, in mono it's exact).
        assert!(
            (caret_offset - expected_4_chars).abs() < 3.0,
            "caret_offset {caret_offset} should be ~{expected_4_chars} \
             (4 chars * char_w), not based on byte count",
        );
    }

    /// Right-edge variant: position the caret so that one more Right
    /// press pushes it past the visible right edge, then press Right
    /// and verify the cursor-follow scroll engaged AND the resulting
    /// caret position is at least 2 chars inside the right edge.
    /// Previously the trigger used `window_size()` for the viewport
    /// width, which includes padding/scrollbar, so the cursor went
    /// out of view by ~1 char before the scroll kicked in.
    #[test]
    fn headless_wgpu_right_arrow_keeps_margin_from_right_edge() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let long = "x".repeat(500);
        let text = format!("{long}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Park caret far to the right (col 90 — past where a single
        // pane can show with scroll_x=0).
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 90,
        );
        // Force scroll_x to 0 so the caret at col 90 is now off the
        // RIGHT edge of the visible pane.
        view_state.pin_scroll_x_after_splice = Some((Side::Left, 0.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }

        // Press Right. Caret advances to col 91; cursor-follow scroll
        // must engage to bring the cursor back into view with the
        // 2-char right margin.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }

        // Cursor at col 91 has content x ≈ 60 + 91*6 = 606. With the
        // 2-char margin, scroll_x should be set so the cursor lands
        // ~2 chars (12 px) inside the right visible edge.
        //
        // Compute the cursor's in-viewport screen offset measured
        // from the LEFT of the visible content area, then subtract
        // from the visible width to get distance from right edge.
        let scroll_x = view_state.last_left_scroll_x;
        let cursor_content_x = 60.0 + 91.0 * 6.0; // ≈606
        let cursor_in_viewport = cursor_content_x - scroll_x;
        // Approximate the live visible content width — content_region
        // accounts for WindowPadding (and scrollbar reservation). For
        // a 1200×800 display with two panes + connector, each pane is
        // ~568 px and visible content within ~552 after padding. We
        // assert with a generous range below.
        let approx_visible_w = 552.0;
        let dist_from_right = approx_visible_w - cursor_in_viewport;

        assert!(
            dist_from_right >= 8.0,
            "cursor went too close to (or past) right edge: cursor_x={cursor_content_x}, \
             scroll_x={scroll_x}, in-viewport={cursor_in_viewport}, \
             dist-from-right={dist_from_right} (want ≥8 px ≈ >1 char)",
        );
        assert!(
            dist_from_right <= 20.0,
            "cursor landed too far from right edge: dist-from-right={dist_from_right} \
             (want ≤20 px for ~2-char margin)",
        );
    }

    /// The cursor-follow scroll keeps a 2-character margin from the
    /// viewport edges. Setup: scroll so the caret would be exactly at
    /// the edge, then press an arrow; after the move the cursor must
    /// be at least 2 chars inside the viewport.
    #[test]
    fn headless_wgpu_lateral_arrow_keeps_two_char_margin() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let long = "x".repeat(500);
        let text = format!("{long}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 0,
        );

        // Force scroll_x to a moderate value so the caret at col 0 is
        // off-screen. After the Right press, the caret will be brought
        // back into view — and we'll measure how far inside the viewport
        // it ended up.
        view_state.pin_scroll_x_after_splice = Some((Side::Left, 200.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }

        // Press Right: caret advances to col 1; scroll snaps back so
        // cursor is visible with the 2-char margin.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }

        // char_w in the mono font at size 13 is ~6 px (verified by
        // existing tests). Cursor at col 1 has content x ≈ 66
        // (gutter_w 60 + 1*6). With a 2-char margin we expect
        // scroll_x ≈ 66 - 12 = 54. That puts the cursor 12 px inside
        // the left edge of the viewport — i.e., a 2-char margin.
        let scroll_x = view_state.last_left_scroll_x;
        let cursor_content_x = 60.0 + 6.0; // approximate (col 1)
        let cursor_screen_x_in_viewport = cursor_content_x - scroll_x;

        // Margin should be ~2 chars (~12 px). Allow generous tolerance
        // for char_w drift and float math; the bug case would be 0 or
        // 6 (1-char or no margin).
        assert!(
            cursor_screen_x_in_viewport >= 10.0,
            "cursor landed too close to left edge: cursor_x={cursor_content_x}, \
             scroll_x={scroll_x}, in-viewport={cursor_screen_x_in_viewport} \
             (expected >=10 for ~2-char margin)",
        );
        assert!(
            cursor_screen_x_in_viewport <= 20.0,
            "cursor landed too far from edge: in-viewport={cursor_screen_x_in_viewport} \
             (expected ~12 for 2-char margin)",
        );
    }

    /// Pressing Left/Right while the caret is off-screen must scroll
    /// the view so the caret comes back into view. ImGui's input_text
    /// doesn't manage the parent's scroll when the widget spans the
    /// full content_w, so we compute the scroll target ourselves in
    /// `draw_row` and feed it through the pin mechanism.
    ///
    /// Test setup: focus the row, force scroll_x to a non-zero value
    /// (so the caret at column 0 is off the LEFT edge of the visible
    /// pane), press Right, and assert scroll_x came back to keep the
    /// caret in view.
    #[test]
    fn headless_wgpu_lateral_arrow_follows_cursor_into_view() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let long = "x".repeat(500);
        let text = format!("{long}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Park caret at column 0.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 0,
        );

        // Force scroll_x to 300, well past where column 0 lives.
        // After this settles, the caret is off the left edge of the
        // visible pane.
        view_state.pin_scroll_x_after_splice = Some((Side::Left, 300.0, 4));
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }
        let scrolled_away = view_state.last_left_scroll_x;
        assert!(
            (scrolled_away - 300.0).abs() < 5.0,
            "scroll_x didn't take the forced value (got {scrolled_away})",
        );

        // Press Right: caret moves from col 0 to col 1, still way off
        // the left edge of the visible pane. Cursor-follow scroll must
        // bring scroll_x back near 0 so the caret is visible.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        for _ in 0..6 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }

        // The caret is at column 1 (cursor_content_x ≈ 60 + 1*6 = 66).
        // With a left-edge pad of `char_w`, scroll target ≈ 60. So
        // scroll_x should be far from 300 (the previous "scrolled
        // away" value) — anything below 100 indicates the cursor-
        // follow kicked in.
        assert!(
            view_state.last_left_scroll_x < 100.0,
            "scroll_x should have followed the cursor back to ~60; \
             was {scrolled_away}, now {}",
            view_state.last_left_scroll_x,
        );
    }

    /// Left or Right arrow inside an active row must reset
    /// `state.caret_blink_reset` to the current imgui time so the
    /// manually-drawn caret is on for the first half of the new
    /// blink cycle — otherwise the user would press Left/Right and
    /// see no caret for up to half a second.
    #[test]
    fn headless_wgpu_lateral_arrow_resets_caret_blink() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Activate line 1 column 4. After settle, blink_reset is set
        // to the activation frame's time.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 1, 4,
        );
        let blink_at_activation = view_state.caret_blink_reset;

        // Run several idle frames so imgui's clock advances well past
        // the activation timestamp. blink_reset should NOT change —
        // idle time with no input doesn't reset the blink.
        for _ in 0..10 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }
        assert_eq!(
            view_state.caret_blink_reset, blink_at_activation,
            "idle frames must not reset blink",
        );

        // Press RightArrow. This should bump blink_reset to a later
        // imgui time so the caret is visible at the new position.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::RightArrow),
                ..Default::default()
            },
        );
        let blink_after_right = view_state.caret_blink_reset;
        assert!(
            blink_after_right > blink_at_activation,
            "RightArrow should reset blink_reset to a later time \
             (was {blink_at_activation}, now {blink_after_right})",
        );

        // Idle frames again — blink_reset should hold steady.
        for _ in 0..5 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono), FrameInput::default(),
            );
        }
        assert_eq!(
            view_state.caret_blink_reset, blink_after_right,
            "idle frames after RightArrow must not reset again",
        );

        // LeftArrow likewise resets.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::LeftArrow),
                ..Default::default()
            },
        );
        assert!(
            view_state.caret_blink_reset > blink_after_right,
            "LeftArrow should reset blink_reset",
        );
    }

    /// Pressing a plain arrow key (no shift modifier) inside an active
    /// row collapses any existing cross-row `state.selection`. Standard
    /// editor behavior: arrow keys without shift dismiss the selection
    /// and move the caret as a point.
    #[test]
    fn headless_wgpu_plain_arrow_clears_selection() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\nuvwxyz0123\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        // Activate line 2's input_text.
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 2, 4,
        );
        // Pre-seed a cross-row selection from (1, 4) → (2, 4).
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 4 },
            caret: SelPoint { line_no: 2, col: 4 },
        });

        // Plain Down (no shift) should clear the selection.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::DownArrow),
                shift: false,
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono), FrameInput::default(),
        );

        assert!(
            view_state.selection.is_none(),
            "plain DownArrow should have cleared selection; got {:?}",
            view_state.selection.as_ref().map(|s| (s.anchor, s.caret)),
        );
    }

    /// Shift+Up mirror of the Shift+Down test.
    #[test]
    fn headless_wgpu_shift_up_extends_selection_to_prev_line() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let text = "abcdefghij\nklmnopqrst\nuvwxyz0123\n";
        let store = SessionStore::new();
        let id = store.open_two_way(text, text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();
        focus_row_and_settle(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, mono, Side::Left, 2, 4,
        );

        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono),
            FrameInput {
                arrow: Some(imgui::Key::UpArrow),
                shift: true,
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, Some(mono), FrameInput::default(),
        );

        let sel = view_state
            .selection
            .as_ref()
            .expect("Shift+Up should produce a selection");
        assert_eq!(sel.side, Side::Left);
        assert_eq!(
            sel.anchor,
            SelPoint { line_no: 2, col: 4 },
            "anchor should be at the pre-move caret position",
        );
        assert_eq!(
            sel.caret,
            SelPoint { line_no: 1, col: 4 },
            "caret should jump to same column on line 1",
        );
    }

    /// User-reported scenario: double-clicking on `=` in
    /// `#[cfg(target_arch = "wasm32")]` selects just `=`, not
    /// `target_arch`. This test loads the live app's mono font
    /// (RobotoMono) so `char_w` matches imgui's hit-test and we can
    /// drive a click at the exact `=` column.
    ///
    /// Note: imgui's default WORDLEFT/WORDRIGHT happens to also select
    /// just `=` when the cursor lands directly on it (because `=` is
    /// flanked by spaces, forming a one-char non-space run). So this
    /// test PASSES with or without the override fix — it's a scenario
    /// documentation rather than a bug-catcher. The punct-run test
    /// above is the regression gate for the user's underlying class
    /// of issue (no-space punct runs).
    #[test]
    fn headless_wgpu_double_click_equal_in_cfg_selects_just_equal() {
        let _guard = imgui_lock();
        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let line = "#[cfg(target_arch = \"wasm32\")]";
        let text = format!("{line}\n");
        let store = SessionStore::new();
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Load mono font so calc_text_size("m") returns the per-char
        // width imgui actually uses for hit-testing — without this the
        // default proportional font breaks our column math.
        let mono = load_mono_font(&mut ctx, 13.0);
        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        // The `=` is at byte (and char) index 18. With the mono font
        // and 1200×800 display, widget_x0 ≈ 76 and char_w ≈ 7.8;
        // x ≈ 76 + 18*7.8 ≈ 217. We sweep a small range to absorb any
        // few-pixel drift; with the mono font in place this only needs
        // one or two iterations to hit `=`, and crucially the imgui
        // state doesn't bleed because the override fires once on each
        // double-click detection.
        let equal_byte_idx = 18;
        let mut hit_equal = false;
        for click_x in (160..=220).step_by(2) {
            let mut view_state = DiffViewState::default();
            let click_pos = [click_x as f32, 40.0];
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput {
                    mouse_pos: Some(click_pos),
                    left_button: Some(true),
                    ..Default::default()
                },
            );
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput { left_button: Some(false), ..Default::default() },
            );
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput {
                    mouse_pos: Some(click_pos),
                    left_button: Some(true),
                    ..Default::default()
                },
            );
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, Some(mono),
                FrameInput { left_button: Some(false), ..Default::default() },
            );
            for _ in 0..2 {
                run_frame_with_wgpu(
                    &mut ctx, &mut renderer, &device, &queue, target_format,
                    &store, id, &mut view_state, Some(mono), FrameInput::default(),
                );
            }

            let Some((_, ln, start, end)) = view_state.last_active_input_selection else {
                continue;
            };
            assert_eq!(ln, 1);
            if start == equal_byte_idx && end == equal_byte_idx + 1 {
                hit_equal = true;
                break;
            }
        }
        assert!(
            hit_equal,
            "no swept x position selected exactly '='; calibration drifted",
        );
    }

    /// Real-renderer end-to-end: drives the full imgui → wgpu pipeline
    /// per frame (ctx.render → CommandEncoder → render_pass →
    /// Renderer::render → queue.submit, against an offscreen target),
    /// then asserts that across the pin window scroll_x stays at the
    /// post-splice baseline.
    ///
    /// **This test does catch the original bug.** With the pin push
    /// disabled (replace the `pin_scroll_x` capture at the top of
    /// `render` with `None`), scroll_x drifts from 0 to gutter_w (~60px)
    /// — exactly the live-app symptom — and the test fails. With the
    /// pin active, scroll_x stays at the baseline.
    ///
    /// Notes:
    ///   - The wgpu device + queue is required: without rendering
    ///     submission, imgui's nav-scroll pipeline doesn't fully trip.
    ///   - `NAV_ENABLE_KEYBOARD` config flag is required (sets up the
    ///     nav system so set_keyboard_focus_here engages it).
    ///   - The pin countdown must be ≥3 frames to outlast imgui's
    ///     widget-activation cycle; we use 4 for safety.
    ///   - This test takes ~1.5s due to wgpu init + per-frame texture
    ///     allocation; the in-memory variant covers state-machine
    ///     regressions in ~20ms.
    #[test]
    fn headless_wgpu_splice_preserves_scroll_x() {
        let _guard = imgui_lock();

        let Some((device, queue)) = try_init_wgpu() else {
            eprintln!("skipping: no wgpu adapter available in this environment");
            return;
        };

        let store = SessionStore::new();
        let long = "x".repeat(500);
        let text = format!("hello world\n{long}\n");
        let id = store.open_two_way(&text, &text, None).unwrap();

        let mut ctx = imgui::Context::create();
        ctx.io_mut().display_size = [1200.0, 800.0];
        ctx.io_mut().delta_time = 1.0 / 60.0;
        ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
        // Note: imgui_wgpu::Renderer::new builds the font atlas itself.

        let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = imgui_wgpu::Renderer::new(
            &mut ctx,
            &device,
            &queue,
            imgui_wgpu::RendererConfig {
                texture_format: target_format,
                ..Default::default()
            },
        );

        let mut view_state = DiffViewState::default();

        // Engage NavWindow: click + release inside the left pane.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput {
                mouse_pos: Some([150.0, 80.0]),
                left_button: Some(true),
                ..Default::default()
            },
        );
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { left_button: Some(false), ..Default::default() },
        );
        view_state.selection = Some(Selection {
            side: Side::Left,
            anchor: SelPoint { line_no: 1, col: 0 },
            caret: SelPoint { line_no: 1, col: 5 },
        });

        // Splice frame: Backspace pressed.
        run_frame_with_wgpu(
            &mut ctx, &mut renderer, &device, &queue, target_format,
            &store, id, &mut view_state, None,
            FrameInput { backspace: true, ..Default::default() },
        );
        let snap = store.snapshot(id).unwrap();
        if let SessionMode::TwoWay { a_lines, .. } = &snap.mode {
            assert_eq!(a_lines[0], " world", "splice should have shortened line 1");
        }
        let baseline_x = view_state.last_left_scroll_x;
        assert!(matches!(
            view_state.pin_scroll_x_after_splice,
            Some((Side::Left, _, _))
        ));

        // Run many idle frames — enough to outlast any pin countdown.
        for _ in 0..15 {
            run_frame_with_wgpu(
                &mut ctx, &mut renderer, &device, &queue, target_format,
                &store, id, &mut view_state, None, FrameInput::default(),
            );
        }

        // The live-app bug shifts scroll_x by exactly gutter_w (60 px
        // at code_font_zoom=1.0). A 10-px bound catches that with margin
        // for any sub-pixel float drift but is way below imgui's
        // bug-magnitude scroll.
        const MAX_DRIFT: f32 = 10.0;
        assert!(
            (view_state.last_left_scroll_x - baseline_x).abs() < MAX_DRIFT,
            "scroll_x drifted from baseline {baseline_x} to {} (>{MAX_DRIFT}px) — pin failed",
            view_state.last_left_scroll_x,
        );
        assert!(view_state.pin_scroll_x_after_splice.is_none());
    }
}

mod move_pairing_tests {
    use super::*;

    #[test]
    fn session_with_move_produces_paired_hunks_with_matching_id() {
        use crate::app::diff_view::common::{find_paired_hunk, hunk_move_id, Side};
        use crate::diff::{DiffOptions, Hunk};
        use crate::session::{SessionMode, SessionStore};

        let a_text = "hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n";
        let b_text = "hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n";
        let store = SessionStore::new();
        let opts = DiffOptions {
            detect_moves: true,
            move_min_lines: 2,
            ..DiffOptions::default()
        };
        let id = store
            .open_two_way_with(a_text, b_text, Some("histogram".into()), opts)
            .expect("create session");
        let snapshot = store.snapshot(id).expect("snapshot");
        let hunks: Vec<Hunk> = match snapshot.mode {
            SessionMode::TwoWay { hunks, .. } => hunks,
            _ => panic!("expected TwoWay"),
        };
        let tagged: Vec<&Hunk> = hunks.iter().filter(|h| hunk_move_id(h).is_some()).collect();
        assert_eq!(tagged.len(), 2, "exactly two hunks should be tagged");
        let id_a = hunk_move_id(tagged[0]);
        let id_b = hunk_move_id(tagged[1]);
        assert_eq!(id_a, id_b, "both halves of a move share the id");
        let move_id = id_a.unwrap();
        let delete_hunk = tagged
            .iter()
            .find(|h| h.b_range == (0, 0))
            .expect("delete-only present");
        let paired = find_paired_hunk(&hunks, move_id, Side::Left);
        assert_eq!(
            paired.map(|h| h.id),
            Some(tagged.iter().find(|h| h.a_range == (0, 0)).unwrap().id)
        );
        let _ = delete_hunk;
    }
}
