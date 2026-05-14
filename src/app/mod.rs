//! Native GUI entry point.
//!
//! Sets up winit (0.30 `ApplicationHandler`) + wgpu + imgui-rs and drives the
//! frame loop. The actual diff UI is rendered inside `frame_ui()`; everything
//! else here is plumbing that shouldn't change as the UI grows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use imgui::{Context, FontGlyphRanges, FontId, FontSource};
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
mod engine_bar;
mod merge_view;
mod preferences;
mod recents;
mod result_pane;
mod syntax;
mod theme;
mod undo_stack;

const INITIAL_WIDTH: u32 = 1400;
const INITIAL_HEIGHT: u32 = 900;

const CODE_FONT_BASE_SCALE: f32 = 1.5;
const CODE_FONT_ZOOM_MIN: f32 = 0.5;
const CODE_FONT_ZOOM_MAX: f32 = 4.0;
const CODE_FONT_ZOOM_STEP: f32 = 0.1;

/// Module-level zoom value for the code-view font, kept as packed f32 bits
/// in an `AtomicU32`. The view modules read this via `code_font_zoom()` to
/// derive `row_h` / `gutter_w` without threading the zoom through every
/// signature.
static CODE_FONT_ZOOM_BITS: AtomicU32 = AtomicU32::new(0x3f800000); // 1.0f32 bits

pub fn code_font_zoom() -> f32 {
    f32::from_bits(CODE_FONT_ZOOM_BITS.load(Ordering::Relaxed))
}

fn set_code_font_zoom(v: f32) {
    CODE_FONT_ZOOM_BITS.store(v.to_bits(), Ordering::Relaxed);
}

/// imgui `ClipboardBackend` adapter on top of `arboard::Clipboard`. We lazily
/// construct the arboard handle the first time it's used so a failure to
/// open the platform clipboard (e.g. on a headless machine) doesn't crash
/// startup — we just degrade to no-op clipboard.
#[derive(Default)]
struct ArboardClipboard {
    inner: Option<arboard::Clipboard>,
}

impl ArboardClipboard {
    fn ensure(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.inner.is_none() {
            self.inner = arboard::Clipboard::new().ok();
        }
        self.inner.as_mut()
    }
}

impl imgui::ClipboardBackend for ArboardClipboard {
    fn get(&mut self) -> Option<String> {
        self.ensure().and_then(|c| c.get_text().ok())
    }
    fn set(&mut self, value: &str) {
        if let Some(c) = self.ensure() {
            let _ = c.set_text(value.to_string());
        }
    }
}

/// One "wheel unit" is ~one text line in imgui's internal scroll math.
/// `MouseScrollDelta::PixelDelta` from touchpads arrives in raw pixels; we
/// divide by this to get a comparable per-line value.
const PIXEL_DELTA_PER_LINE: f32 = 16.0;

pub fn run() {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run event loop");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabMode {
    TwoWay,
    ThreeWay,
}

#[derive(Clone, Debug)]
struct Tab {
    session_id: SessionId,
    label: String,
    mode: TabMode,
    /// File paths in role order:
    /// - 2-way: \[A, B]
    /// - 3-way: \[base, local, remote]
    /// Used so Save File A / Save File B / Save Result As can write back
    /// without re-prompting.
    paths: Vec<PathBuf>,
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
    /// Which pane (in which session) holds the input focus. Currently only
    /// updated by view code for diagnostics; the Edit menu no longer routes
    /// clipboard operations through this field since `input_text_multiline`
    /// handles Cut/Copy/Paste/Select-All natively inside the focused widget.
    focused: Option<(SessionId, FocusedPane)>,
    /// Set when the user changes code-font zoom; the next render call clears
    /// the imgui font atlas, re-adds Roboto + Roboto Mono at the new size,
    /// reloads the GPU font texture, and stores the new mono `FontId`.
    font_rebuild_pending: bool,
    /// Recently-opened comparisons (move-to-front, persisted to JSON in
    /// the platform's config dir). Populated from disk in `Default`.
    recents: Vec<recents::RecentEntry>,
    /// Per-session undo/redo history. Every diff-view mutation
    /// (line edits, Apply A↔B hunk replacements) is pushed onto the
    /// matching tab's stack; Edit > Undo / Redo (Ctrl+Z / Ctrl+Shift+Z)
    /// drive these.
    undo_stacks: HashMap<SessionId, undo_stack::Stack>,
    /// Tree-sitter parse cache. Per (SessionId, side) the parser holds onto
    /// the last source hash + per-line highlight spans so unchanged buffers
    /// don't re-parse every frame.
    syntax: syntax::HighlightCache,
    /// Default engine + DiffOptions applied to new tabs. Persisted to
    /// `settings.json` via the Preferences dialog.
    preferences: preferences::AppPreferences,
    /// True while the Preferences modal is open. The modal is rendered
    /// each frame inside `render` and closes when the user clicks OK
    /// or presses Escape.
    preferences_open: bool,
    /// Working copy of preferences while the dialog is open.
    preferences_draft: preferences::AppPreferences,
}

/// Identifier shared between diff/merge views and the result pane so view
/// code can record "the focused pane" without a generic-side enum. The Edit
/// menu no longer dispatches by this — imgui's `input_text_multiline`
/// handles clipboard ops inside the focused widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusedPane {
    TwoWayA,
    TwoWayB,
    ThreeWayBase,
    ThreeWayLocal,
    ThreeWayRemote,
    Result,
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
            focused: None,
            font_rebuild_pending: false,
            recents: recents::load(),
            undo_stacks: HashMap::new(),
            syntax: syntax::HighlightCache::default(),
            preferences: preferences::load(),
            preferences_open: false,
            preferences_draft: preferences::AppPreferences::default(),
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
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
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
            experimental_features: wgpu::ExperimentalFeatures::default(),
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
        theme::apply(&mut imgui);
        syntax::prime_tables();
        // Cross-platform clipboard via arboard so set/get_clipboard_text on
        // the Ui actually round-trips to the OS clipboard (winit-support
        // doesn't wire this up for us).
        imgui.set_clipboard_backend(ArboardClipboard::default());

        let mut platform = WinitPlatform::new(&mut imgui);
        platform.attach_window(imgui.io_mut(), &window, HiDpiMode::Default);

        let hidpi_factor = window.scale_factor();
        let font_size = (13.0 * hidpi_factor) as f32;
        imgui.io_mut().font_global_scale = (1.0 / hidpi_factor) as f32;
        let mono_font = load_fonts(&mut imgui, font_size);
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
        wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            gpu.surface.configure(&gpu.device, &gpu.surface_config);
            return;
        }
        _ => return,
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());

    if state.font_rebuild_pending {
        state.font_rebuild_pending = false;
        let hidpi_factor = gpu.window.scale_factor();
        let ui_font_size = (13.0 * hidpi_factor) as f32;
        let new_mono = load_fonts(&mut gpu.imgui, ui_font_size);
        state.mono_font = Some(new_mono);
        gpu.renderer
            .reload_font_texture(&mut gpu.imgui, &gpu.device, &gpu.queue);
    }

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
                depth_slice: None,
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
            multiview_mask: None,
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
    preferences_modal(ui, state);

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
            // Recents submenu — last-12 list, persisted to JSON.
            ui.menu("Recents", || {
                if state.recents.is_empty() {
                    ui.text_disabled("(no recent comparisons)");
                    return;
                }
                let mut to_open: Option<recents::RecentEntry> = None;
                let mut clear = false;
                for entry in &state.recents {
                    if ui.menu_item(entry.label()) {
                        to_open = Some(entry.clone());
                    }
                }
                ui.separator();
                if ui.menu_item("Clear Recents") {
                    clear = true;
                }
                if let Some(entry) = to_open {
                    open_recent(state, &entry);
                }
                if clear {
                    state.recents.clear();
                    recents::save(&state.recents);
                }
            });
            ui.separator();
            let has_session = state.active.is_some();
            let is_two_way = active_mode(state) == Some(TabMode::TwoWay);
            let is_three_way = active_mode(state) == Some(TabMode::ThreeWay);
            if ui
                .menu_item_config("Save File A")
                .shortcut("Ctrl+S")
                .enabled(is_two_way)
                .build()
            {
                save_two_way_side(state, crate::session::TwoWaySide::A);
            }
            if ui
                .menu_item_config("Save File B")
                .shortcut("Ctrl+Shift+S")
                .enabled(is_two_way)
                .build()
            {
                save_two_way_side(state, crate::session::TwoWaySide::B);
            }
            if ui
                .menu_item_config("Save Result As…")
                .shortcut("Ctrl+S")
                .enabled(is_three_way)
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
            if ui.menu_item("Preferences...") {
                state.preferences_draft = state.preferences.clone();
                state.preferences_open = true;
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
            let (can_undo, can_redo) = state
                .active
                .and_then(|id| state.undo_stacks.get(&id))
                .map(|r| (r.can_undo(), r.can_redo()))
                .unwrap_or((false, false));
            if ui
                .menu_item_config("Undo")
                .shortcut("Ctrl+Z")
                .enabled(can_undo)
                .build()
            {
                do_undo(state);
            }
            if ui
                .menu_item_config("Redo")
                .shortcut("Ctrl+Shift+Z")
                .enabled(can_redo)
                .build()
            {
                do_redo(state);
            }
            // Cut / Copy / Paste / Select All intentionally omitted:
            // imgui's `input_text_multiline` handles those natively inside
            // the focused pane.
        });
        ui.menu("View", || {
            if ui
                .menu_item_config("Increase Code Font")
                .shortcut("Ctrl+=")
                .build()
            {
                bump_zoom(state, CODE_FONT_ZOOM_STEP);
            }
            if ui
                .menu_item_config("Decrease Code Font")
                .shortcut("Ctrl+-")
                .build()
            {
                bump_zoom(state, -CODE_FONT_ZOOM_STEP);
            }
            if ui
                .menu_item_config("Reset Code Font")
                .shortcut("Ctrl+0")
                .enabled((code_font_zoom() - 1.0).abs() > 1e-3)
                .build()
            {
                set_code_font_zoom(1.0);
                state.font_rebuild_pending = true;
            }
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

        // Right-aligned status text in the menu bar — replaces the old
        // toolbar status. Open / Save / etc. all moved to File menu items.
        let lower = state.status.to_ascii_lowercase();
        let text_w = ui.calc_text_size(&state.status)[0];
        let avail_w = ui.content_region_avail()[0];
        let pad = (avail_w - text_w - 12.0).max(8.0);
        ui.dummy([pad, 0.0]);
        ui.same_line();
        if lower.contains("error") || lower.contains("failed") {
            ui.text_colored([1.0, 0.45, 0.45, 1.0], &state.status);
        } else {
            ui.text_disabled(&state.status);
        }
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
    if ui.is_key_pressed(Key::S) {
        match active_mode(state) {
            Some(TabMode::TwoWay) => {
                if shift {
                    save_two_way_side(state, crate::session::TwoWaySide::B);
                } else {
                    save_two_way_side(state, crate::session::TwoWaySide::A);
                }
            }
            Some(TabMode::ThreeWay) if !shift => save_as(state),
            _ => {}
        }
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
    // Ctrl+C / Ctrl+X / Ctrl+V / Ctrl+A are handled natively by imgui's
    // `input_text_multiline` inside the focused pane. Ctrl+Z is the
    // exception — we route it through the app-level undo stack, but only
    // when the focus isn't inside the 3-way Result pane (where imgui's
    // built-in text undo wins).
    let result_focused = matches!(state.focused, Some((_, FocusedPane::Result)));
    if !shift && (ui.is_key_pressed(Key::Equal) || ui.is_key_pressed(Key::KeypadAdd)) {
        bump_zoom(state, CODE_FONT_ZOOM_STEP);
    }
    if !shift && (ui.is_key_pressed(Key::Minus) || ui.is_key_pressed(Key::KeypadSubtract)) {
        bump_zoom(state, -CODE_FONT_ZOOM_STEP);
    }
    if !shift && (ui.is_key_pressed(Key::Alpha0) || ui.is_key_pressed(Key::Keypad0)) {
        set_code_font_zoom(1.0);
        state.font_rebuild_pending = true;
    }
    if ui.is_key_pressed(Key::Z) && !result_focused {
        if shift {
            do_redo(state);
        } else {
            do_undo(state);
        }
    }
}

/// Clear the font atlas and re-add Roboto Regular (UI) + Roboto Mono (code)
/// at the current `code_font_zoom`. Returns the new mono `FontId`.
fn load_fonts(imgui: &mut Context, ui_font_size: f32) -> FontId {
    let fonts = imgui.fonts();
    fonts.clear();
    fonts.add_font(&[FontSource::TtfData {
        data: aetna_fonts_roboto::ROBOTO_REGULAR,
        size_pixels: ui_font_size,
        config: Some(imgui::FontConfig {
            size_pixels: ui_font_size,
            glyph_ranges: FontGlyphRanges::from_slice(EXTRA_GLYPH_RANGES),
            ..Default::default()
        }),
    }]);
    let code_size = ui_font_size * CODE_FONT_BASE_SCALE * code_font_zoom();
    fonts.add_font(&[FontSource::TtfData {
        data: include_bytes!("../../assets/RobotoMono-Regular.ttf"),
        size_pixels: code_size,
        config: Some(imgui::FontConfig {
            size_pixels: code_size,
            glyph_ranges: FontGlyphRanges::from_slice(EXTRA_GLYPH_RANGES),
            ..Default::default()
        }),
    }])
}

/// Codepoint ranges loaded into the font atlas. Default imgui covers only
/// Basic Latin, which leaves UI strings full of `→ ↔ — … ✕ ⇒ ≥ Δ` rendering
/// as missing-glyph boxes. Each pair is an inclusive [start, end]; the slice
/// is zero-terminated as imgui requires.
///
/// Note: Roboto Regular does not cover every codepoint in these ranges (e.g.
/// U+2715 ✕ is missing even though Dingbats is requested). Imgui shows `?`
/// for unmapped codepoints, so prefer characters Roboto actually ships
/// (e.g. × U+00D7) for UI labels.
#[rustfmt::skip]
static EXTRA_GLYPH_RANGES: &[u32] = &[
    0x0020, 0x00FF, // Basic Latin + Latin-1 Supplement
    0x0370, 0x03FF, // Greek (Δ)
    0x2010, 0x205E, // General Punctuation (— – … etc.)
    0x2190, 0x21FF, // Arrows (→ ↔ ⇒)
    0x2200, 0x22FF, // Mathematical Operators (≥)
    0x2700, 0x27BF, // Dingbats (✕)
    0,
];

fn do_undo(state: &mut AppState) {
    let Some(id) = state.active else {
        return;
    };
    let store = &mut state.sessions;
    let Some(record) = state.undo_stacks.get_mut(&id) else {
        return;
    };
    if record.can_undo() {
        record.undo(store);
        // The diff/merge views sync their buffers from session text at the
        // top of every render, so no per-view epoch bump is required.
        state.status = "undone".to_string();
    } else {
        state.status = "nothing to undo".to_string();
    }
}

fn do_redo(state: &mut AppState) {
    let Some(id) = state.active else {
        return;
    };
    let store = &mut state.sessions;
    let Some(record) = state.undo_stacks.get_mut(&id) else {
        return;
    };
    if record.can_redo() {
        record.redo(store);
        state.status = "redone".to_string();
    } else {
        state.status = "nothing to redo".to_string();
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
    state.undo_stacks.remove(&id);
    state.active = idx
        .and_then(|i| state.tabs.get(i.min(state.tabs.len().saturating_sub(1))))
        .map(|t| t.session_id);
    state.status = format!("closed tab (session {id})");
}

fn bump_zoom(state: &mut AppState, delta: f32) {
    let new = (code_font_zoom() + delta).clamp(CODE_FONT_ZOOM_MIN, CODE_FONT_ZOOM_MAX);
    set_code_font_zoom(new);
    state.font_rebuild_pending = true;
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
            Some(ui.push_style_color(imgui::StyleColor::Button, theme::BLUE))
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
        if ui.small_button(format!("×##close_{}", tab.session_id)) {
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
        state.undo_stacks.remove(&id);
        if state.active == Some(id) {
            state.active = idx
                .and_then(|i| state.tabs.get(i.min(state.tabs.len().saturating_sub(1))))
                .map(|t| t.session_id);
        }
        state.status = format!("closed tab (session {id})");
    }
}

/// Render the Preferences modal when `state.preferences_open` is true.
/// On OK we save to disk; on Cancel/Escape we discard the draft.
fn preferences_modal(ui: &imgui::Ui, state: &mut AppState) {
    if !state.preferences_open {
        return;
    }
    ui.open_popup("Preferences");

    let mut still_open = true;
    if let Some(_token) = ui
        .modal_popup_config("Preferences")
        .opened(&mut still_open)
        .always_auto_resize(true)
        .begin_popup()
    {
        let engines = crate::diff::available_engines();
        let engine_names: Vec<String> = engines.iter().map(|(n, _)| n.clone()).collect();
        let mut engine_idx = engines
            .iter()
            .position(|(n, _)| *n == state.preferences_draft.default_engine)
            .unwrap_or(0);
        ui.text("Default engine for new tabs:");
        ui.set_next_item_width(200.0);
        if ui.combo_simple_string("##pref_engine", &mut engine_idx, &engine_names) {
            if let Some(n) = engine_names.get(engine_idx) {
                state.preferences_draft.default_engine = n.clone();
            }
        }

        ui.separator();
        ui.text("Default diff options for new tabs:");

        use crate::diff::{SubLineGranularity, Whitespace};
        const WS: &[(&str, Whitespace)] = &[
            ("Significant", Whitespace::None),
            ("Ignore all", Whitespace::IgnoreAll),
            ("Ignore leading", Whitespace::IgnoreLeading),
            ("Ignore trailing+EOL", Whitespace::IgnoreTrailingEol),
        ];
        const GR: &[(&str, SubLineGranularity)] = &[
            ("None", SubLineGranularity::None),
            ("Word", SubLineGranularity::Word),
            ("Char", SubLineGranularity::Char),
            ("Grapheme", SubLineGranularity::Grapheme),
        ];

        let mut ws_idx = WS
            .iter()
            .position(|(_, v)| *v == state.preferences_draft.default_options.whitespace)
            .unwrap_or(0);
        ui.text("Whitespace:");
        ui.same_line();
        ui.set_next_item_width(180.0);
        let ws_labels: Vec<&str> = WS.iter().map(|(l, _)| *l).collect();
        if ui.combo_simple_string("##pref_ws", &mut ws_idx, &ws_labels) {
            state.preferences_draft.default_options.whitespace = WS[ws_idx].1;
        }

        let mut g_idx = GR
            .iter()
            .position(|(_, v)| *v == state.preferences_draft.default_options.sub_line)
            .unwrap_or(0);
        ui.text("Sub-line granularity:");
        ui.same_line();
        ui.set_next_item_width(140.0);
        let g_labels: Vec<&str> = GR.iter().map(|(l, _)| *l).collect();
        if ui.combo_simple_string("##pref_sub", &mut g_idx, &g_labels) {
            state.preferences_draft.default_options.sub_line = GR[g_idx].1;
        }

        ui.checkbox(
            "Detect moves (when supported by engine)",
            &mut state.preferences_draft.default_options.detect_moves,
        );

        ui.separator();
        if ui.button("OK") {
            state.preferences = state.preferences_draft.clone();
            if let Err(e) = preferences::save(&state.preferences) {
                state.status = format!("preferences save error: {e}");
            } else {
                state.status = "preferences saved".into();
            }
            state.preferences_open = false;
            ui.close_current_popup();
        }
        ui.same_line();
        if ui.button("Cancel") {
            state.preferences_open = false;
            ui.close_current_popup();
        }
    }
    if !still_open {
        state.preferences_open = false;
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

fn active_mode(state: &AppState) -> Option<TabMode> {
    let id = state.active?;
    state.tabs.iter().find(|t| t.session_id == id).map(|t| t.mode)
}

fn save_two_way_side(state: &mut AppState, side: crate::session::TwoWaySide) {
    let Some(id) = state.active else {
        return;
    };
    let Some(tab) = state.tabs.iter().find(|t| t.session_id == id) else {
        return;
    };
    let idx = match side {
        crate::session::TwoWaySide::A => 0,
        crate::session::TwoWaySide::B => 1,
    };
    let Some(path) = tab.paths.get(idx).cloned() else {
        state.status = "no file path stored for this side".into();
        return;
    };
    let snap = match state.sessions.snapshot(id) {
        Ok(s) => s,
        Err(e) => {
            state.status = format!("snapshot error: {e}");
            return;
        }
    };
    let crate::session::SessionMode::TwoWay { a_text, b_text, a_trailing_newline, b_trailing_newline, .. } = &snap.mode else {
        state.status = "active session is not 2-way".into();
        return;
    };
    let (text, trailing) = match side {
        crate::session::TwoWaySide::A => (a_text, *a_trailing_newline),
        crate::session::TwoWaySide::B => (b_text, *b_trailing_newline),
    };
    match fileio::write_text(&path, text, trailing) {
        Ok(()) => {
            state.status = format!(
                "saved {}: {}",
                match side {
                    crate::session::TwoWaySide::A => "A",
                    crate::session::TwoWaySide::B => "B",
                },
                path.display()
            );
        }
        Err(e) => state.status = format!("save error: {e}"),
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
    match fileio::write_text(&path, &text, false) {
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
    engine_bar::render(
        ui,
        &state.sessions,
        id,
        &snap.engine,
        snap.options,
        &mut state.status,
    );
    ui.separator();
    match &snap.mode {
        SessionMode::TwoWay { hunks, anchors, a_text, b_text, .. } => {
            anchor_bar_two_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
            // 2-way edits the source files directly — there is no separate
            // "result" so the diff fills the remaining vertical space.
            let store = &state.sessions;
            let status = &mut state.status;
            let mono = state.mono_font;
            // Resolve per-side language from the tab's stored file paths,
            // then compute (or reuse) per-line highlight spans via the
            // tree-sitter cache.
            let (a_lang, b_lang) = match tab {
                Some(t) => (
                    t.paths.first().and_then(|p| syntax::lang_for_path(p)),
                    t.paths.get(1).and_then(|p| syntax::lang_for_path(p)),
                ),
                None => (None, None),
            };
            let a_key = id << 1;
            let b_key = (id << 1) | 1;
            let a_lines_vec: Vec<String> = crate::session::lines_of(a_text)
                .into_iter().map(|s| s.to_string()).collect();
            let b_lines_vec: Vec<String> = crate::session::lines_of(b_text)
                .into_iter().map(|s| s.to_string()).collect();
            let a_highlights = state
                .syntax
                .highlights(a_key, a_lang, &a_lines_vec)
                .to_vec();
            let b_highlights = state
                .syntax
                .highlights(b_key, b_lang, &b_lines_vec)
                .to_vec();
            let view_state = state.diff_views.entry(id).or_default();
            let mut focus_request: Option<FocusedPane> = None;
            let mut pending_edits: Vec<undo_stack::DiffEdit> = Vec::new();
            diff_view::render(
                ui,
                store,
                id,
                hunks,
                anchors,
                status,
                view_state,
                mono,
                &mut focus_request,
                &mut pending_edits,
                &a_highlights,
                &b_highlights,
            );
            if let Some(p) = focus_request {
                state.focused = Some((id, p));
            }
            // Apply queued mutations via the per-session undo stack so each
            // operation is reversible via Edit > Undo / Redo.
            if !pending_edits.is_empty() {
                let record = state.undo_stacks.entry(id).or_default();
                // The new multiline pane syncs its buffer from session
                // text at the top of every `diff_view::render`, so we no
                // longer need the per-view input-epoch trick.
                for edit in pending_edits {
                    record.edit(&mut state.sessions, edit);
                }
                state.status = "edited (Ctrl+Z to undo)".to_string();
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
                let mut focus_request: Option<FocusedPane> = None;
                let mut pending_edits: Vec<undo_stack::DiffEdit> = Vec::new();
                ui.child_window("merge_area")
                    .size([0.0, diff_h])
                    .build(|| {
                        merge_view::render(
                            ui,
                            store,
                            id,
                            hunks,
                            anchors,
                            status,
                            view_state,
                            mono,
                            &mut focus_request,
                            &mut pending_edits,
                        );
                    });
                if let Some(p) = focus_request {
                    state.focused = Some((id, p));
                }
                if !pending_edits.is_empty() {
                    let record = state.undo_stacks.entry(id).or_default();
                    for edit in pending_edits {
                        record.edit(&mut state.sessions, edit);
                    }
                    state.status = "edited (Ctrl+Z to undo)".to_string();
                }
            }
            {
                let mono = state.mono_font;
                let result = state.result_panes.entry(id).or_default();
                let mut focus_request: Option<FocusedPane> = None;
                ui.child_window("result_area")
                    .size([0.0, 0.0])
                    .border(true)
                    .build(|| {
                        result_pane::render(
                            ui,
                            &state.sessions,
                            id,
                            result,
                            mono,
                            &mut focus_request,
                        );
                    });
                if let Some(p) = focus_request {
                    state.focused = Some((id, p));
                }
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
    open_two_way_paths(state, a, b);
}

fn open_two_way_paths(state: &mut AppState, a: PathBuf, b: PathBuf) {
    let a_read = match fileio::read_text(&a) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (A): {e}");
            return;
        }
    };
    let b_read = match fileio::read_text(&b) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (B): {e}");
            return;
        }
    };
    let engine = Some(state.preferences.default_engine.clone());
    let opts = state.preferences.default_options;
    match state.sessions.open_two_way_with(
        a_read.text,
        b_read.text,
        a_read.trailing_newline,
        b_read.trailing_newline,
        engine,
        opts,
    ) {
        Ok(id) => {
            let label = format!("{} ↔ {}", basename(&a), basename(&b));
            let recent = recents::RecentEntry::TwoWay {
                a: a.clone(),
                b: b.clone(),
            };
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::TwoWay,
                paths: vec![a, b],
            });
            state.active = Some(id);
            state.status = format!("Opened 2-way: {label}");
            recents::add(&mut state.recents, recent);
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
    open_three_way_paths(state, base, local, remote);
}

fn open_three_way_paths(
    state: &mut AppState,
    base: PathBuf,
    local: PathBuf,
    remote: PathBuf,
) {
    let base_read = match fileio::read_text(&base) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (BASE): {e}");
            return;
        }
    };
    let local_read = match fileio::read_text(&local) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (LOCAL): {e}");
            return;
        }
    };
    let remote_read = match fileio::read_text(&remote) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("Read error (REMOTE): {e}");
            return;
        }
    };
    let engine = Some(state.preferences.default_engine.clone());
    let opts = state.preferences.default_options;
    match state
        .sessions
        .open_three_way_with(
            base_read.text,
            local_read.text,
            remote_read.text,
            base_read.trailing_newline,
            local_read.trailing_newline,
            remote_read.trailing_newline,
            engine,
            opts,
        )
    {
        Ok(id) => {
            let label = format!("{} (3-way)", basename(&base));
            let recent = recents::RecentEntry::ThreeWay {
                base: base.clone(),
                local: local.clone(),
                remote: remote.clone(),
            };
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::ThreeWay,
                paths: vec![base, local, remote],
            });
            state.active = Some(id);
            state.status = format!("Opened 3-way: {label}");
            recents::add(&mut state.recents, recent);
        }
        Err(e) => state.status = format!("Open 3-way failed: {e}"),
    }
}

fn open_recent(state: &mut AppState, entry: &recents::RecentEntry) {
    match entry.clone() {
        recents::RecentEntry::TwoWay { a, b } => open_two_way_paths(state, a, b),
        recents::RecentEntry::ThreeWay { base, local, remote } => {
            open_three_way_paths(state, base, local, remote)
        }
    }
}
