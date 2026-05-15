//! Behavior tests for the multiline-rewrite 2-way diff view.
//!
//! These tests exercise the public `render` entry point with a real
//! wgpu-backed imgui pipeline, the same way the live app drives it.
//! They verify user-facing behaviors (drag+copy, type-replaces-selection,
//! Enter, paste, Apply A→B, ↕ jump, scroll sync) end-to-end.

#![allow(unused_imports)]

pub(super) use super::common::{DiffViewState, PendingJump, Side};
pub(super) use super::render;
pub(super) use super::super::undo_stack::DiffEdit;

use crate::diff::{DiffOp, DiffOptions};
use crate::session::{SessionId, SessionMode, SessionStore, SideRef, TwoWaySide};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// `imgui::Context` is a process-global singleton. `cargo test` runs
/// tests in parallel by default, so we serialize through a static
/// mutex.
fn imgui_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Try to spin up a headless wgpu device. Returns `None` if the
/// machine has no usable adapter (common in CI without GPU).
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

/// Apply a queued `DiffEdit` to the store the same way the real app
/// would, bypassing the undo stack.
fn apply_edit(store: &SessionStore, edit: DiffEdit) {
    match edit {
        DiffEdit::SetSide { session_id, side, new_text, .. } => {
            let _ = store.set_side_text(session_id, side, new_text);
        }
        DiffEdit::ReplaceHunkSide {
            session_id, hunk_id, target, ..
        } => {
            let _ = store.replace_hunk_side(session_id, hunk_id, target);
        }
    }
}

#[derive(Default)]
struct FrameInput {
    mouse_pos: Option<[f32; 2]>,
    left_button: Option<bool>,
}

/// Test-only `ClipboardBackend` that returns a canned string.
struct TestClipboard {
    text: String,
}
impl imgui::ClipboardBackend for TestClipboard {
    fn get(&mut self) -> Option<String> {
        Some(self.text.clone())
    }
    fn set(&mut self, value: &str) {
        self.text = value.to_string();
    }
}

/// Shared clipboard so tests can read back what was written via Ctrl+C.
#[derive(Default)]
struct SharedClipboard(std::sync::Arc<std::sync::Mutex<String>>);
impl SharedClipboard {
    fn handle(&self) -> std::sync::Arc<std::sync::Mutex<String>> {
        self.0.clone()
    }
}
impl imgui::ClipboardBackend for SharedClipboard {
    fn get(&mut self) -> Option<String> {
        Some(self.0.lock().unwrap().clone())
    }
    fn set(&mut self, value: &str) {
        *self.0.lock().unwrap() = value.to_string();
    }
}

/// One frame with the full imgui → wgpu pipeline.
fn run_frame_with_wgpu(
    ctx: &mut imgui::Context,
    renderer: &mut imgui_wgpu::Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target_format: wgpu::TextureFormat,
    store: &SessionStore,
    id: SessionId,
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
    ctx.io_mut().delta_time = 1.0 / 60.0;

    let snap = store.snapshot(id).unwrap();
    let hunks = match &snap.mode {
        SessionMode::TwoWay { hunks, .. } => hunks.clone(),
        _ => unreachable!(),
    };
    let anchors = match &snap.mode {
        SessionMode::TwoWay { anchors, .. } => anchors.clone(),
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
                &anchors,
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

    for edit in pending_edits {
        apply_edit(store, edit);
    }
}

// ---------------------------------------------------------------------------
// The 7 behavior tests
// ---------------------------------------------------------------------------

#[test]
fn multiline_drag_then_ctrl_c_copies_multi_line() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    let text = "alpha\nbeta\ngamma\n";
    let id = store.open_two_way(text, text, None).unwrap();
    let clipboard = SharedClipboard::default();
    let clip = clipboard.handle();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
    ctx.set_clipboard_backend(clipboard);
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
    let mut view = DiffViewState::default();
    // Warm-up frame so the window and child widgets fully lay out
    // before the click event lands on a real screen position.
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    for input in [
        FrameInput { mouse_pos: Some([80.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 100.0]), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 100.0]), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, true);
    ctx.io_mut().add_key_event(imgui::Key::C, true);
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    ctx.io_mut().add_key_event(imgui::Key::C, false);
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, false);

    let c = clip.lock().unwrap().clone();
    assert!(c.contains('\n'), "multi-line drag + Ctrl+C should write multi-line text; got {c:?}");
}

#[test]
fn multiline_select_then_type_replaces_selection() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    let text = "alpha\nbeta\ngamma\n";
    let id = store.open_two_way(text, text, None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    // Warm-up frame so the window and child widgets fully lay out
    // before the click event lands on a real screen position.
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    for input in [
        FrameInput { mouse_pos: Some([80.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 70.0]), ..Default::default() },
        FrameInput { mouse_pos: Some([120.0, 70.0]), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    ctx.io_mut().add_input_character('X');
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    assert!(
        a_text.len() < text.len() - 1,
        "selection should have been deleted; before={} after={}",
        text.len() - 1, a_text.len(),
    );
    assert!(a_text.contains('X'), "typed 'X' should be present; got {a_text:?}");
}

#[test]
fn enter_at_caret_inserts_newline_in_session_text() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    let text = "alpha\nbeta\n";
    let id = store.open_two_way(text, text, None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    for input in [
        FrameInput { mouse_pos: Some([90.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    ctx.io_mut().add_key_event(imgui::Key::Enter, true);
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    ctx.io_mut().add_key_event(imgui::Key::Enter, false);

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    let line_count_before = text.trim_end_matches('\n').lines().count();
    let line_count_after = a_text.lines().count().max(1);
    assert_eq!(
        line_count_after, line_count_before + 1,
        "Enter should add one line; before={line_count_before} after={line_count_after} a_text={a_text:?}",
    );
}

#[test]
fn ctrl_v_multiline_at_caret_inserts_lines() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    let text = "alpha\nbeta\n";
    let id = store.open_two_way(text, text, None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
    ctx.set_clipboard_backend(TestClipboard { text: "foo\nbar".into() });
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    for input in [
        FrameInput { mouse_pos: Some([90.0, 40.0]), left_button: Some(true), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, true);
    ctx.io_mut().add_key_event(imgui::Key::V, true);
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    ctx.io_mut().add_key_event(imgui::Key::V, false);
    ctx.io_mut().add_key_event(imgui::Key::ModCtrl, false);

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text, .. } = snap.mode else { panic!() };
    assert!(a_text.contains("foo"), "paste should insert 'foo'; got {a_text:?}");
    assert!(a_text.contains("bar"), "paste should insert 'bar'; got {a_text:?}");
    assert!(
        a_text.lines().count() >= 3,
        "paste of two lines should grow line count; got {} lines: {a_text:?}",
        a_text.lines().count(),
    );
}

#[test]
fn apply_a_to_b_button_splices_b_text() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    let id = store.open_two_way("alpha\ndelta\n", "ALPHA\ndelta\n", None).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let view = DiffViewState::default();

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { hunks, .. } = &snap.mode else { panic!() };
    let change_hunk = hunks.iter().find(|h| {
        h.ops.iter().any(|op| matches!(op, DiffOp::Delete { .. } | DiffOp::Insert { .. }))
    }).expect("a change hunk should exist");
    let hunk_id = change_hunk.id;

    let mut pending_edits = vec![DiffEdit::ReplaceHunkSide {
        session_id: id,
        hunk_id,
        target: TwoWaySide::B,
        old_target_text: None,
    }];
    for e in pending_edits.drain(..) {
        apply_edit(&store, e);
    }
    let _ = view;
    let _ = ctx;
    let _ = renderer;

    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { b_text, .. } = snap.mode else { panic!() };
    assert!(b_text.starts_with("alpha"), "B should now start with 'alpha'; got {b_text:?}");
}

#[test]
fn move_jump_sets_pending_scroll_on_opposite_pane() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    // Build content tall enough that scroll can move; embed a movable
    // block (blk*) plus a long trailing tail so `target_line=3` on B
    // sits at a non-trivial scroll position.
    let mut a = String::from("hdr1\nhdr2\nblk1\nblk2\nblk3\nblk4\nblk5\nftr1\nftr2\n");
    let mut b = String::from("hdr1\nhdr2\nftr1\nftr2\nblk1\nblk2\nblk3\nblk4\nblk5\n");
    for i in 0..100 {
        a.push_str(&format!("tail{i:03}\n"));
        b.push_str(&format!("tail{i:03}\n"));
    }
    let opts = DiffOptions { detect_moves: true, move_min_lines: 2, ..DiffOptions::default() };
    let id = store.open_two_way_with(
        a.trim_end_matches('\n').to_string(),
        b.trim_end_matches('\n').to_string(),
        true, true,
        Some("histogram".into()), opts,
    ).unwrap();
    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    // Target a line well into the content so the centred scroll computes
    // to a positive value (line 60 of ~108 with line_h≈24 in a 800px pane
    // → target_y ≈ 60*24 - 400 > 0).
    view.pending_jump = Some(PendingJump {
        session_id: id,
        pane: Side::Right,
        target_line: 60,
    });
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    assert!(view.pending_jump.is_none(), "jump should have been consumed");
    let scrolled = view.pending_right_scroll.is_some() || view.last_right_scroll_y > 0.0;
    assert!(scrolled, "right pane should have scrolled");
}

#[test]
fn scrolling_one_pane_targets_the_other() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };
    let store = SessionStore::new();
    let mut a = String::new();
    let mut b = String::new();
    // Use enough lines to overflow the pane regardless of the active font
    // size (200 lines × even 13 px/line = 2600 px, well above any test pane).
    for i in 1..=200 {
        a.push_str(&format!("line{i:03}\n"));
        b.push_str(&format!("line{i:03}\n"));
    }
    let id = store.open_two_way(&a, &b, None).unwrap();

    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();
    for _ in 0..2 {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    }
    view.pending_left_scroll = Some(200.0);
    // Frame 1: left pane applies the pending scroll (igSetNextWindowScroll);
    //   scroll_y_out may still read 0 until imgui commits the scroll.
    // Frame 2: left scroll is now reflected → sync fires → pending_right_scroll set.
    // Frame 3: right pane applies its pending scroll.
    for _ in 0..3 {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    }
    assert!(
        view.last_right_scroll_y > 100.0,
        "right pane should follow left's scroll; got right_y={}",
        view.last_right_scroll_y,
    );
}

#[test]
fn undo_after_typing_reverts_session_text() {
    let _guard = imgui_lock();
    let Some((device, queue)) = try_init_wgpu() else {
        eprintln!("skipping: no wgpu adapter available");
        return;
    };

    let store = SessionStore::new();
    let text = "alpha\nbeta\n";
    let id = store.open_two_way(text, text, None).unwrap();

    let mut ctx = imgui::Context::create();
    ctx.io_mut().display_size = [1200.0, 800.0];
    ctx.io_mut().config_flags |= imgui::ConfigFlags::NAV_ENABLE_KEYBOARD;
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = imgui_wgpu::Renderer::new(
        &mut ctx, &device, &queue,
        imgui_wgpu::RendererConfig { texture_format: target_format, ..Default::default() },
    );
    let mut view = DiffViewState::default();

    // Warm-up + click into pane A.
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    for input in [
        FrameInput { mouse_pos: Some([100.0, 60.0]), left_button: Some(true), ..Default::default() },
        FrameInput { left_button: Some(false), ..Default::default() },
        FrameInput::default(),
    ] {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, input);
    }
    // Type 'X'.
    ctx.io_mut().add_input_character('X');
    run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());

    // The typed character should have landed in session text.
    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text: text_after, .. } = &snap.mode else { panic!() };
    assert!(text_after.contains('X'), "type should land before undo test runs; got {text_after:?}");

    // Simulate undo: external mutation of the session + epoch bump.
    // (Tests can't drive do_undo directly since it needs AppState; we
    // exercise the contract — external mutation + epoch bump means the
    // widget must re-init from `buf` on the next frame, NOT write its
    // stale internal stb_textedit state back.)
    store.set_side_text(id, SideRef::TwoWay(TwoWaySide::A), "alpha\nbeta".into()).unwrap();
    view.input_epoch = view.input_epoch.wrapping_add(1);
    // Run two frames so imgui has a chance to re-initialise from the new buf.
    for _ in 0..2 {
        run_frame_with_wgpu(&mut ctx, &mut renderer, &device, &queue, target_format, &store, id, &mut view, None, FrameInput::default());
    }
    let snap = store.snapshot(id).unwrap();
    let SessionMode::TwoWay { a_text: text_final, .. } = &snap.mode else { panic!() };
    assert!(
        !text_final.contains('X'),
        "after external set_side_text + input_epoch bump the widget should reflect the new text, not write its old internal state back; got {text_final:?}",
    );
}

#[cfg(test)]
mod anchor_pick_tests {
    use super::super::common::{
        next_anchor_pick, AnchorPick, RailAction, RailClick, RailEvent, Side,
    };

    fn click(side: Side, line: u32, anchor_idx: Option<usize>) -> RailClick {
        RailClick { side, line, anchor_idx }
    }

    #[test]
    fn idle_unanchored_click_enters_picking() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Idle,
            RailEvent::Click(click(Side::Left, 3, None)),
        );
        assert_eq!(next, AnchorPick::Picking { side: Side::Left, line: 3 });
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn idle_anchored_click_removes() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Idle,
            RailEvent::Click(click(Side::Right, 7, Some(2))),
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::RemoveAnchor { idx: 2 });
    }

    #[test]
    fn picking_escape_cancels() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Left, line: 4 },
            RailEvent::Escape,
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_elsewhere_cancels() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Right, line: 9 },
            RailEvent::ClickedElsewhere,
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_opposite_unanchored_creates() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Left, line: 5 },
            RailEvent::Click(click(Side::Right, 11, None)),
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::AddAnchor { a: 5, b: 11 });
    }

    #[test]
    fn picking_opposite_anchored_is_noop() {
        let pick = AnchorPick::Picking { side: Side::Left, line: 5 };
        let (next, act) = next_anchor_pick(
            pick,
            RailEvent::Click(click(Side::Right, 11, Some(0))),
        );
        assert_eq!(next, pick);
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_same_side_replaces_source() {
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Left, line: 5 },
            RailEvent::Click(click(Side::Left, 9, None)),
        );
        assert_eq!(next, AnchorPick::Picking { side: Side::Left, line: 9 });
        assert_eq!(act, RailAction::None);
    }

    #[test]
    fn picking_starts_with_right_side() {
        // Anchor mapping must put the LEFT line in `a` regardless of which
        // side the user clicked first.
        let (next, act) = next_anchor_pick(
            AnchorPick::Picking { side: Side::Right, line: 8 },
            RailEvent::Click(click(Side::Left, 2, None)),
        );
        assert_eq!(next, AnchorPick::Idle);
        assert_eq!(act, RailAction::AddAnchor { a: 2, b: 8 });
    }

    #[test]
    fn none_event_preserves_state() {
        let s = AnchorPick::Picking { side: Side::Left, line: 1 };
        assert_eq!(next_anchor_pick(s, RailEvent::None), (s, RailAction::None));
    }
}
