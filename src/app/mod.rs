//! Native GUI entry point.
//!
//! Sets up winit (0.30 `ApplicationHandler`) + wgpu + imgui-rs and drives the
//! frame loop. The actual diff UI is rendered inside `frame_ui()`; everything
//! else here is plumbing that shouldn't change as the UI grows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use imgui::{Context, FontId, FontSource};
use imgui_wgpu::{Renderer, RendererConfig};
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use crate::diff::Anchor;
use crate::io as fileio;
use crate::session::{SessionId, SessionMode, SessionStore};

mod char_diff;
mod diff_view;
mod merge_view;
mod result_pane;

const INITIAL_WIDTH: u32 = 1400;
const INITIAL_HEIGHT: u32 = 900;

/// One "wheel unit" is ~one text line in imgui's internal scroll math.
/// `MouseScrollDelta::PixelDelta` from touchpads arrives in raw pixels; we
/// divide by this to get a comparable per-line value.
const PIXEL_DELTA_PER_LINE: f32 = 16.0;

pub fn run() {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run event loop");
}

#[derive(Clone, Debug)]
enum TabMode {
    TwoWay,
    ThreeWay,
}

#[derive(Clone, Debug)]
struct Tab {
    session_id: SessionId,
    label: String,
    mode: TabMode,
}

struct AppState {
    sessions: SessionStore,
    tabs: Vec<Tab>,
    active: Option<SessionId>,
    status: String,
    diff_views: HashMap<SessionId, diff_view::DiffViewState>,
    merge_views: HashMap<SessionId, merge_view::MergeViewState>,
    result_panes: HashMap<SessionId, result_pane::ResultState>,
    /// FontId of Roboto Mono, registered alongside the UI font in `resumed`.
    /// Pushed around the diff/merge code rows so columns align character by
    /// character.
    mono_font: Option<FontId>,
    /// Set by the File > Quit menu / Ctrl+Q shortcut. The event-loop handler
    /// checks this after each frame and calls `event_loop.exit()`.
    quit_requested: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            sessions: SessionStore::new(),
            tabs: Vec::new(),
            active: None,
            status: "Open files to begin.".to_string(),
            diff_views: HashMap::new(),
            merge_views: HashMap::new(),
            result_panes: HashMap::new(),
            mono_font: None,
            quit_requested: false,
        }
    }
}

/// Boot-time resources. Populated lazily in `resumed` since `winit` 0.30 only
/// gives us an `ActiveEventLoop` after the platform is ready.
struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    imgui: Context,
    platform: WinitPlatform,
    renderer: Renderer,
    last_frame: Instant,
}

#[derive(Default)]
struct App {
    gpu: Option<Gpu>,
    state: AppState,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("Diffie")
            .with_inner_size(winit::dpi::LogicalSize::new(
                INITIAL_WIDTH as f64,
                INITIAL_HEIGHT as f64,
            ));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        // --- wgpu -----------------------------------------------------------
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))
            .expect("request adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("diffie-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
            trace: wgpu::Trace::default(),
        }))
        .expect("request device");

        let size = window.inner_size();
        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        // --- imgui ----------------------------------------------------------
        let mut imgui = Context::create();
        imgui.set_ini_filename(None);

        let mut platform = WinitPlatform::new(&mut imgui);
        platform.attach_window(imgui.io_mut(), &window, HiDpiMode::Default);

        let hidpi_factor = window.scale_factor();
        let font_size = (13.0 * hidpi_factor) as f32;
        imgui.io_mut().font_global_scale = (1.0 / hidpi_factor) as f32;
        imgui.fonts().add_font(&[FontSource::TtfData {
            data: aetna_fonts_roboto::ROBOTO_REGULAR,
            size_pixels: font_size,
            config: Some(imgui::FontConfig {
                size_pixels: font_size,
                ..Default::default()
            }),
        }]);
        // Code-view font: 1.5x the UI size so dense diffs stay easier to scan
        // than the UI font, but without dominating the layout.
        let code_font_size = font_size * 1.5;
        let mono_font = imgui.fonts().add_font(&[FontSource::TtfData {
            data: include_bytes!("../../assets/RobotoMono-Regular.ttf"),
            size_pixels: code_font_size,
            config: Some(imgui::FontConfig {
                size_pixels: code_font_size,
                ..Default::default()
            }),
        }]);
        self.state.mono_font = Some(mono_font);

        let renderer = Renderer::new(
            &mut imgui,
            &device,
            &queue,
            RendererConfig {
                texture_format: surface_format,
                ..Default::default()
            },
        );

        self.gpu = Some(Gpu {
            window,
            surface,
            device,
            queue,
            surface_config,
            imgui,
            platform,
            renderer,
            last_frame: Instant::now(),
        });
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        // Handle mouse-wheel ourselves so PixelDelta (touchpad / hi-res
        // scroll) doesn't get fed to imgui as raw-pixel wheel units, which
        // makes scroll fly at multiple screens per gesture. LineDelta passes
        // through 1:1 (matches the OS's default per-notch step).
        if let WindowEvent::MouseWheel { delta, .. } = &event {
            let (h, v) = match delta {
                winit::event::MouseScrollDelta::LineDelta(x, y) => (*x, *y),
                winit::event::MouseScrollDelta::PixelDelta(p) => (
                    (p.x as f32) / PIXEL_DELTA_PER_LINE,
                    (p.y as f32) / PIXEL_DELTA_PER_LINE,
                ),
            };
            let io = gpu.imgui.io_mut();
            io.mouse_wheel_h += h;
            io.mouse_wheel += v;
        } else {
            let full_event: winit::event::Event<()> = winit::event::Event::WindowEvent {
                window_id: gpu.window.id(),
                event: event.clone(),
            };
            gpu.platform.handle_event(gpu.imgui.io_mut(), &gpu.window, &full_event);
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(new_size) => {
                gpu.surface_config.width = new_size.width.max(1);
                gpu.surface_config.height = new_size.height.max(1);
                gpu.surface.configure(&gpu.device, &gpu.surface_config);
                gpu.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                render(gpu, &mut self.state);
                if self.state.quit_requested {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.window.request_redraw();
        }
    }
}

fn render(gpu: &mut Gpu, state: &mut AppState) {
    let now = Instant::now();
    gpu.imgui.io_mut().update_delta_time(now - gpu.last_frame);
    gpu.last_frame = now;

    let frame = match gpu.surface.get_current_texture() {
        Ok(f) => f,
        Err(_) => {
            gpu.surface.configure(&gpu.device, &gpu.surface_config);
            return;
        }
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    gpu.platform
        .prepare_frame(gpu.imgui.io_mut(), &gpu.window)
        .expect("prepare imgui frame");
    let ui = gpu.imgui.new_frame();
    frame_ui(ui, state);
    gpu.platform.prepare_render(ui, &gpu.window);

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("diffie-frame"),
        });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("diffie-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.06,
                        g: 0.07,
                        b: 0.09,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        gpu.renderer
            .render(gpu.imgui.render(), &gpu.queue, &gpu.device, &mut pass)
            .expect("imgui render");
    }
    gpu.queue.submit(Some(encoder.finish()));
    frame.present();
}

// --- UI -------------------------------------------------------------------

fn frame_ui(ui: &imgui::Ui, state: &mut AppState) {
    keyboard_shortcuts(ui, state);
    menu_bar(ui, state);

    // Position the root window inside the viewport's "work area" so it sits
    // below the main menu bar instead of overlapping it. Reading directly
    // from the sys ImGuiViewport avoids depending on imgui-rs's wrapper
    // re-export path.
    let (work_pos, work_size) = unsafe {
        let vp = imgui::sys::igGetMainViewport();
        ([(*vp).WorkPos.x, (*vp).WorkPos.y], [(*vp).WorkSize.x, (*vp).WorkSize.y])
    };

    ui.window("Diffie")
        .position(work_pos, imgui::Condition::Always)
        .size(work_size, imgui::Condition::Always)
        .flags(
            imgui::WindowFlags::NO_DECORATION
                | imgui::WindowFlags::NO_MOVE
                | imgui::WindowFlags::NO_RESIZE
                | imgui::WindowFlags::NO_BRING_TO_FRONT_ON_FOCUS,
        )
        .build(|| {
            toolbar(ui, state);
            ui.separator();
            tab_bar(ui, state);
            ui.separator();
            current_session_summary(ui, state);
        });
}

fn menu_bar(ui: &imgui::Ui, state: &mut AppState) {
    ui.main_menu_bar(|| {
        ui.menu("File", || {
            if ui
                .menu_item_config("Open 2-way…")
                .shortcut("Ctrl+O")
                .build()
            {
                open_two_way(state);
            }
            if ui
                .menu_item_config("Open 3-way…")
                .shortcut("Ctrl+Shift+O")
                .build()
            {
                open_three_way(state);
            }
            ui.separator();
            let has_session = state.active.is_some();
            if ui
                .menu_item_config("Save Result As…")
                .shortcut("Ctrl+S")
                .enabled(has_session)
                .build()
            {
                save_as(state);
            }
            if ui
                .menu_item_config("Close Tab")
                .shortcut("Ctrl+W")
                .enabled(has_session)
                .build()
            {
                close_active_tab(state);
            }
            ui.separator();
            if ui
                .menu_item_config("Quit")
                .shortcut("Ctrl+Q")
                .build()
            {
                state.quit_requested = true;
            }
        });
        ui.menu("Edit", || {
            // Phase 2+ will wire these up to selection/clipboard logic.
            let _ = ui.menu_item_config("Cut").shortcut("Ctrl+X").enabled(false).build();
            let _ = ui.menu_item_config("Copy").shortcut("Ctrl+C").enabled(false).build();
            let _ = ui.menu_item_config("Paste").shortcut("Ctrl+V").enabled(false).build();
            ui.separator();
            let _ = ui.menu_item_config("Select All").shortcut("Ctrl+A").enabled(false).build();
        });
        ui.menu("View", || {
            // Font zoom items land in the View-menu phase (atlas rebuild).
            let _ = ui.menu_item_config("Increase Code Font").shortcut("Ctrl++").enabled(false).build();
            let _ = ui.menu_item_config("Decrease Code Font").shortcut("Ctrl+-").enabled(false).build();
            let _ = ui.menu_item_config("Reset Code Font").shortcut("Ctrl+0").enabled(false).build();
            ui.separator();
            let multi = state.tabs.len() > 1;
            if ui
                .menu_item_config("Next Tab")
                .shortcut("Ctrl+Tab")
                .enabled(multi)
                .build()
            {
                cycle_tab(state, 1);
            }
            if ui
                .menu_item_config("Previous Tab")
                .shortcut("Ctrl+Shift+Tab")
                .enabled(multi)
                .build()
            {
                cycle_tab(state, -1);
            }
        });
    });
}

fn keyboard_shortcuts(ui: &imgui::Ui, state: &mut AppState) {
    use imgui::Key;
    let io = ui.io();
    let ctrl = io.key_ctrl;
    let shift = io.key_shift;
    if !ctrl {
        return;
    }
    if ui.is_key_pressed(Key::O) {
        if shift {
            open_three_way(state);
        } else {
            open_two_way(state);
        }
    }
    if !shift && ui.is_key_pressed(Key::S) {
        save_as(state);
    }
    if !shift && ui.is_key_pressed(Key::W) {
        close_active_tab(state);
    }
    if !shift && ui.is_key_pressed(Key::Q) {
        state.quit_requested = true;
    }
    if ui.is_key_pressed(Key::Tab) {
        cycle_tab(state, if shift { -1 } else { 1 });
    }
}

fn close_active_tab(state: &mut AppState) {
    let Some(id) = state.active else {
        return;
    };
    let idx = state.tabs.iter().position(|t| t.session_id == id);
    state.tabs.retain(|t| t.session_id != id);
    state.diff_views.remove(&id);
    state.merge_views.remove(&id);
    state.result_panes.remove(&id);
    state.active = idx
        .and_then(|i| state.tabs.get(i.min(state.tabs.len().saturating_sub(1))))
        .map(|t| t.session_id);
    state.status = format!("closed tab (session {id})");
}

fn cycle_tab(state: &mut AppState, delta: i32) {
    if state.tabs.is_empty() {
        return;
    }
    let cur = state
        .active
        .and_then(|id| state.tabs.iter().position(|t| t.session_id == id))
        .unwrap_or(0) as i32;
    let len = state.tabs.len() as i32;
    let next = ((cur + delta) % len + len) % len;
    state.active = Some(state.tabs[next as usize].session_id);
}

fn tab_bar(ui: &imgui::Ui, state: &mut AppState) {
    if state.tabs.is_empty() {
        return;
    }
    let mut new_active: Option<SessionId> = None;
    let mut close: Option<SessionId> = None;
    for tab in &state.tabs {
        let active = state.active == Some(tab.session_id);
        let _col = if active {
            Some(ui.push_style_color(imgui::StyleColor::Button, [0.30, 0.50, 0.80, 1.0]))
        } else {
            None
        };
        let badge = match tab.mode {
            TabMode::TwoWay => "[2]",
            TabMode::ThreeWay => "[3]",
        };
        let label = format!("{badge} {}##sw_{}", tab.label, tab.session_id);
        if ui.button(label) {
            new_active = Some(tab.session_id);
        }
        drop(_col);
        ui.same_line_with_spacing(0.0, 2.0);
        if ui.small_button(format!("✕##close_{}", tab.session_id)) {
            close = Some(tab.session_id);
        }
        ui.same_line();
    }
    // End the same-line run so the next widget wraps onto a new row.
    ui.new_line();
    if let Some(id) = new_active {
        state.active = Some(id);
    }
    if let Some(id) = close {
        let idx = state.tabs.iter().position(|t| t.session_id == id);
        state.tabs.retain(|t| t.session_id != id);
        state.diff_views.remove(&id);
        state.merge_views.remove(&id);
        state.result_panes.remove(&id);
        if state.active == Some(id) {
            state.active = idx
                .and_then(|i| state.tabs.get(i.min(state.tabs.len().saturating_sub(1))))
                .map(|t| t.session_id);
        }
        state.status = format!("closed tab (session {id})");
    }
}

fn anchor_bar_two_way(
    ui: &imgui::Ui,
    store: &SessionStore,
    session_id: SessionId,
    anchors: &[Anchor],
    status: &mut String,
) {
    if anchors.is_empty() {
        ui.text_disabled("Anchors: none — click a row in each pane to add one.");
        return;
    }
    ui.text("Anchors: ");
    for (i, a) in anchors.iter().enumerate() {
        ui.same_line();
        ui.text(format!("A:{} ↔ B:{}", a.a, a.b));
        ui.same_line();
        if ui.small_button(format!("✕##rm_anc_{i}")) {
            match store.remove_anchor(session_id, i) {
                Ok(()) => *status = format!("anchor removed: A:{} ↔ B:{}", a.a, a.b),
                Err(e) => *status = format!("anchor remove error: {e}"),
            }
            return; // bail; next frame re-renders with the updated list
        }
    }
}

fn toolbar(ui: &imgui::Ui, state: &mut AppState) {
    if ui.button("Open 2-way…") {
        open_two_way(state);
    }
    ui.same_line();
    if ui.button("Open 3-way…") {
        open_three_way(state);
    }
    ui.same_line();
    let has_session = state.active.is_some();
    ui.disabled(!has_session, || {
        if ui.button("Save Result As…") {
            save_as(state);
        }
    });
    ui.same_line();
    // Status: red-tinted for entries that read like errors, default otherwise.
    let lower = state.status.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") {
        ui.text_colored([1.0, 0.45, 0.45, 1.0], &state.status);
    } else {
        ui.text_disabled(&state.status);
    }
}

fn save_as(state: &mut AppState) {
    let Some(id) = state.active else {
        return;
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title("Save merged result")
        .save_file()
    else {
        return;
    };
    let text = match state.sessions.compute_result(id) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("compute error: {e}");
            return;
        }
    };
    match fileio::write_text(&path, &text) {
        Ok(()) => state.status = format!("saved: {}", path.display()),
        Err(e) => state.status = format!("save error: {e}"),
    }
}

fn current_session_summary(ui: &imgui::Ui, state: &mut AppState) {
    let Some(id) = state.active else {
        ui.text_disabled("No session open. Open two files (2-way) or three files (3-way) to begin.");
        return;
    };
    let snap = match state.sessions.snapshot(id) {
        Ok(s) => s,
        Err(e) => {
            ui.text_colored([1.0, 0.4, 0.4, 1.0], format!("Session error: {e}"));
            return;
        }
    };
    let tab = state.tabs.iter().find(|t| t.session_id == id);
    if let Some(t) = tab {
        ui.text(format!("Tab: {} (id={})", t.label, t.session_id));
    }
    match &snap.mode {
        SessionMode::TwoWay { hunks, anchors, .. } => {
            anchor_bar_two_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
            let avail = ui.content_region_avail();
            let result_h = 200.0_f32.min(avail[1] * 0.4);
            let diff_h = (avail[1] - result_h - 8.0).max(50.0);
            {
                let store = &state.sessions;
                let status = &mut state.status;
                let mono = state.mono_font;
                let view_state = state.diff_views.entry(id).or_default();
                ui.child_window("diff_area")
                    .size([0.0, diff_h])
                    .build(|| {
                        diff_view::render(ui, store, id, hunks, anchors, status, view_state, mono);
                    });
            }
            {
                let mono = state.mono_font;
                let result = state.result_panes.entry(id).or_default();
                ui.child_window("result_area")
                    .size([0.0, 0.0])
                    .border(true)
                    .build(|| {
                        result_pane::render(ui, &state.sessions, id, result, mono);
                    });
            }
        }
        SessionMode::ThreeWay { hunks, anchors, .. } => {
            anchor_bar_three_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
            let avail = ui.content_region_avail();
            let result_h = 200.0_f32.min(avail[1] * 0.4);
            let diff_h = (avail[1] - result_h - 8.0).max(50.0);
            {
                let store = &state.sessions;
                let status = &mut state.status;
                let mono = state.mono_font;
                let view_state = state.merge_views.entry(id).or_default();
                ui.child_window("merge_area")
                    .size([0.0, diff_h])
                    .build(|| {
                        merge_view::render(ui, store, id, hunks, anchors, status, view_state, mono);
                    });
            }
            {
                let mono = state.mono_font;
                let result = state.result_panes.entry(id).or_default();
                ui.child_window("result_area")
                    .size([0.0, 0.0])
                    .border(true)
                    .build(|| {
                        result_pane::render(ui, &state.sessions, id, result, mono);
                    });
            }
        }
    }
}

fn anchor_bar_three_way(
    ui: &imgui::Ui,
    store: &SessionStore,
    session_id: SessionId,
    anchors: &[crate::merge::MergeAnchor],
    status: &mut String,
) {
    if anchors.is_empty() {
        ui.text_disabled("Anchors: none.");
        return;
    }
    ui.text("Anchors: ");
    for (i, a) in anchors.iter().enumerate() {
        ui.same_line();
        ui.text(format!("base:{} ↔ L:{} ↔ R:{}", a.base, a.local, a.remote));
        ui.same_line();
        if ui.small_button(format!("✕##rm_mancr_{i}")) {
            match store.remove_anchor(session_id, i) {
                Ok(()) => *status = format!("anchor removed"),
                Err(e) => *status = format!("anchor remove error: {e}"),
            }
            return;
        }
    }
}

// --- File-dialog actions ---------------------------------------------------

fn basename(p: &PathBuf) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

fn pick_file(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_file()
}

fn open_two_way(state: &mut AppState) {
    let Some(a) = pick_file("Open file A (2-way)") else {
        return;
    };
    let Some(b) = pick_file("Open file B (2-way)") else {
        return;
    };
    let a_text = match fileio::read_text(&a) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (A): {e}");
            return;
        }
    };
    let b_text = match fileio::read_text(&b) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (B): {e}");
            return;
        }
    };
    match state.sessions.open_two_way(&a_text, &b_text, None) {
        Ok(id) => {
            let label = format!("{} ↔ {}", basename(&a), basename(&b));
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::TwoWay,
            });
            state.active = Some(id);
            state.status = format!("Opened 2-way: {label}");
        }
        Err(e) => state.status = format!("Open 2-way failed: {e}"),
    }
}

fn open_three_way(state: &mut AppState) {
    let Some(base) = pick_file("Open BASE (3-way)") else {
        return;
    };
    let Some(local) = pick_file("Open LOCAL (3-way)") else {
        return;
    };
    let Some(remote) = pick_file("Open REMOTE (3-way)") else {
        return;
    };
    let base_text = match fileio::read_text(&base) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (BASE): {e}");
            return;
        }
    };
    let local_text = match fileio::read_text(&local) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (LOCAL): {e}");
            return;
        }
    };
    let remote_text = match fileio::read_text(&remote) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (REMOTE): {e}");
            return;
        }
    };
    match state
        .sessions
        .open_three_way(&base_text, &local_text, &remote_text, None)
    {
        Ok(id) => {
            let label = format!("{} (3-way)", basename(&base));
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::ThreeWay,
            });
            state.active = Some(id);
            state.status = format!("Opened 3-way: {label}");
        }
        Err(e) => state.status = format!("Open 3-way failed: {e}"),
    }
}
