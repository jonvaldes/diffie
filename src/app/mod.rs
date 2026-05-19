//! Native GUI entry point.
//!
//! Sets up winit (0.30 `ApplicationHandler`) + wgpu + imgui-rs and drives the
//! frame loop. The actual diff UI is rendered inside `frame_ui()`; everything
//! else here is plumbing that shouldn't change as the UI grows.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use imgui::{Context, FontGlyphRanges, FontId, FontSource};
use imgui_wgpu::{Renderer, RendererConfig};
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

use crate::diff::Anchor;
use crate::io as fileio;
use crate::session::{SessionId, SessionMode, SessionStore};

mod diff_view;
mod engine_bar;
mod fonts;
mod merge_view;
mod preferences;
pub mod three_way_header;
mod recents;
mod result_pane;
mod syntax;
mod syntax_paint;
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

/// Files to open immediately on launch, supplied via CLI args.
#[derive(Debug, Clone)]
pub enum InitialOpen {
    TwoWay {
        a: PathBuf,
        b: PathBuf,
    },
    ThreeWay {
        base: PathBuf,
        local: PathBuf,
        remote: PathBuf,
        /// Bound save target for the merged result. If the file exists at
        /// launch its contents are loaded into `manual_result`; otherwise the
        /// path is just recorded so Ctrl+S writes there without prompting.
        result: PathBuf,
    },
}

pub fn run() {
    run_with(None);
}

pub fn run_with(initial: Option<InitialOpen>) {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::default();
    app.state.pending_initial = initial;
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
    /// 3-way only: bound save target for the merged result. Set when the
    /// user picks a path via "Save Result As…" or supplies one via the CLI.
    /// When `Some`, plain Save (Ctrl+S) writes here without prompting.
    result_path: Option<PathBuf>,
    /// Live string buffers backing the per-pane filename input boxes in
    /// the header strip. Parallel to `paths`; rewritten whenever `paths`
    /// changes (browse dialog, CLI open).
    path_inputs: Vec<String>,
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
    /// Set when the user changes the theme flavor; the next render call
    /// re-applies the palette to the imgui style table (which the
    /// modal scope itself can't reach, since it only has access to the
    /// frame `Ui`).
    theme_apply_pending: bool,
    /// Last value we pushed to `gpu.window.set_title`. We diff against the
    /// computed title each frame so we only call winit when something
    /// actually changed (active tab switched, file path bound, etc).
    last_window_title: String,
    /// False until the first frame has been presented. We create the window
    /// hidden (`with_visible(false)`) so Windows doesn't show the uninitialised
    /// half-white/half-black framebuffer before our first wgpu submit, and flip
    /// this to `true` right after `frame.present()` once there's content to see.
    window_shown: bool,
    /// CLI-supplied session to open on the first frame. Drained inside
    /// `frame_ui` once the GPU / imgui context is up.
    pending_initial: Option<InitialOpen>,
    /// True when something needs to keep redrawing as fast as possible —
    /// e.g. mid-ease scroll. Recomputed at the end of every frame from the
    /// per-session view states. Used by the event loop to switch between
    /// `Poll` (animating) and `Wait` (idle).
    animating: bool,
    /// Most-recent time at which we kicked a caret-blink redraw. While a
    /// text input is focused but nothing else is animating, the loop wakes
    /// at ~`CARET_BLINK_INTERVAL` to give the caret a frame to toggle.
    last_blink_request: Instant,
    /// Most-recent time an input event landed. We keep redrawing for a
    /// short grace period after this so animations triggered by the input
    /// (scroll easing, hover transitions) have time to start — without it,
    /// the loop can go idle on the same frame the input fired, before the
    /// view code has a chance to mark itself animating.
    last_input_at: Instant,
}

/// Imgui's default caret blink rate is one cycle per second; rendering at
/// half that interval is enough to keep on/off transitions visible.
const CARET_BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// How long to keep rendering after the last input event before allowing the
/// loop to drop into a true idle wait. Covers the gap between an input
/// arriving and the resulting animation showing up in `is_animating()`.
const INPUT_REDRAW_GRACE: Duration = Duration::from_millis(200);

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
            theme_apply_pending: false,
            last_window_title: String::new(),
            window_shown: false,
            pending_initial: None,
            animating: false,
            last_blink_request: Instant::now(),
            last_input_at: Instant::now(),
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
        // Wait for events by default. `about_to_wait` overrides this each
        // iteration based on whether anything is mid-animation or an input
        // is focused (caret blink).
        event_loop.set_control_flow(ControlFlow::Wait);
        if self.gpu.is_some() {
            return;
        }

        let icon = winit::window::Icon::from_rgba(
            include_bytes!("../../assets/diffie_icon_64.rgba").to_vec(),
            64,
            64,
        )
        .ok();
        let placement = &self.state.preferences.window;
        let (init_w, init_h) = (
            placement.width.unwrap_or(INITIAL_WIDTH),
            placement.height.unwrap_or(INITIAL_HEIGHT),
        );
        let mut attrs = Window::default_attributes()
            .with_title("Diffie")
            .with_window_icon(icon)
            .with_inner_size(winit::dpi::PhysicalSize::new(init_w, init_h))
            .with_maximized(placement.maximized)
            // Create hidden so the OS doesn't expose the uninitialised
            // framebuffer (a half white / half black flash on Windows)
            // before our first wgpu present. The window is shown in
            // `render()` once we've drawn a frame.
            .with_visible(false);
        if let (Some(x), Some(y)) = (placement.x, placement.y) {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }

        // On Wayland the compositor reads the window icon from a
        // matching `.desktop` file resolved by app_id; the raw RGBA
        // icon set above is ignored. Set app_id (and the X11 WM_CLASS,
        // which uses the same code path in winit) to "diffie" so the
        // packaged `assets/diffie.desktop` is the one consulted.
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android"), not(target_os = "ios")))]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs.with_name("diffie", "diffie")
        };
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        // --- wgpu -----------------------------------------------------------
        // On Windows, restrict to DX12 (skip the Vulkan probe that adds
        // ~hundreds of ms to startup) and prefer the integrated GPU. A code
        // diff tool doesn't need the discrete GPU, and waking the dGPU on
        // hybrid-graphics laptops adds 1-3s of visible delay before the
        // window paints. Other platforms keep PRIMARY (Vulkan on Linux,
        // Metal on macOS) and HighPerformance — both fast there.
        #[cfg(target_os = "windows")]
        let (backends, power_preference) =
            (wgpu::Backends::DX12, wgpu::PowerPreference::LowPower);
        #[cfg(not(target_os = "windows"))]
        let (backends, power_preference) =
            (wgpu::Backends::PRIMARY, wgpu::PowerPreference::HighPerformance);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference,
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
        // Apply the user's saved theme flavor before priming the style
        // table so the first frame already paints in the chosen palette.
        theme::set_flavor(self.state.preferences.theme);
        syntax_paint::set_show_whitespace(self.state.preferences.show_whitespace);
        syntax_paint::set_eol_glyph(self.state.preferences.code_font.eol_codepoint());
        theme::apply(&mut imgui);
        syntax::prime_tables();
        // Cross-platform clipboard via arboard so set/get_clipboard_text on
        // the Ui actually round-trips to the OS clipboard (winit-support
        // doesn't wire this up for us).
        imgui.set_clipboard_backend(ArboardClipboard::default());

        let mut platform = WinitPlatform::new(&mut imgui);
        platform.attach_window(imgui.io_mut(), &window, HiDpiMode::Default);

        let hidpi_factor = window.scale_factor();
        let font_size = (self.state.preferences.ui_font_size as f64 * hidpi_factor) as f32;
        imgui.io_mut().font_global_scale = (1.0 / hidpi_factor) as f32;
        let mono_font = load_fonts(&mut imgui, font_size, self.state.preferences.code_font);
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

        // Paint one frame *before* revealing the window so the OS never
        // exposes the uninitialised framebuffer (the half-white/half-black
        // flash on Windows). RedrawRequested doesn't fire while the window
        // is hidden, so without this explicit render the window-show step
        // inside `render` would never run and the window would stay invisible
        // forever.
        let gpu = self
            .gpu
            .as_mut()
            .expect("gpu just stored above");
        render(gpu, &mut self.state);
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
                save_window_placement(&gpu.window, &mut self.state.preferences);
            }
            WindowEvent::Moved(_) => {
                save_window_placement(&gpu.window, &mut self.state.preferences);
            }
            WindowEvent::RedrawRequested => {
                render(gpu, &mut self.state);
                if self.state.quit_requested {
                    event_loop.exit();
                }
            }
            // Any user input invalidates the current frame — schedule a
            // redraw and stamp the grace-period clock so `about_to_wait`
            // keeps rendering for a short window after the input lands.
            WindowEvent::CursorMoved { .. }
            | WindowEvent::CursorEntered { .. }
            | WindowEvent::CursorLeft { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::ModifiersChanged(_)
            | WindowEvent::Focused(_)
            | WindowEvent::ThemeChanged(_)
            | WindowEvent::ScaleFactorChanged { .. } => {
                self.state.last_input_at = Instant::now();
                gpu.window.request_redraw();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(gpu) = self.gpu.as_ref() else { return };
        let now = Instant::now();
        let in_input_grace = now < self.state.last_input_at + INPUT_REDRAW_GRACE;
        if self.state.animating || in_input_grace {
            // Mid-animation (e.g. easing scroll) — or recently received an
            // input and want to give any resulting animation time to start.
            // Render as fast as the platform will let us.
            gpu.window.request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
        } else if !self.state.tabs.is_empty() {
            // Any comparison is open — the text panes paint their own caret
            // and need periodic frames to blink. `state.focused` would be a
            // tighter signal, but `diff_view` doesn't update it (it ignores
            // its own `focus_request` callback), so we'd never wake up in a
            // 2-way diff. Triggering off "tab is open" is conservative —
            // one frame per `CARET_BLINK_INTERVAL` is cheap and harmless
            // even when no pane currently has a caret.
            let now = Instant::now();
            let next = self.state.last_blink_request + CARET_BLINK_INTERVAL;
            if now >= next {
                gpu.window.request_redraw();
                self.state.last_blink_request = now;
                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    now + CARET_BLINK_INTERVAL,
                ));
            } else {
                event_loop.set_control_flow(ControlFlow::WaitUntil(next));
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

/// Snapshot the window's current geometry into `prefs.window` and persist
/// to disk so the next launch restores it. Position+size are only captured
/// when the window is in its normal (non-maximized) state — otherwise the
/// restored maximized size would clobber the user's actual unmaximized
/// geometry.
fn save_window_placement(window: &Window, prefs: &mut preferences::AppPreferences) {
    let maximized = window.is_maximized();
    let mut changed = false;
    if prefs.window.maximized != maximized {
        prefs.window.maximized = maximized;
        changed = true;
    }
    if !maximized {
        let size = window.inner_size();
        if prefs.window.width != Some(size.width) || prefs.window.height != Some(size.height) {
            prefs.window.width = Some(size.width);
            prefs.window.height = Some(size.height);
            changed = true;
        }
        if let Ok(pos) = window.outer_position() {
            if prefs.window.x != Some(pos.x) || prefs.window.y != Some(pos.y) {
                prefs.window.x = Some(pos.x);
                prefs.window.y = Some(pos.y);
                changed = true;
            }
        }
    }
    if changed {
        let _ = preferences::save(prefs);
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
        let ui_font_size = (state.preferences.ui_font_size as f64 * hidpi_factor) as f32;
        let new_mono = load_fonts(&mut gpu.imgui, ui_font_size, state.preferences.code_font);
        state.mono_font = Some(new_mono);
        gpu.renderer
            .reload_font_texture(&mut gpu.imgui, &gpu.device, &gpu.queue);
    }

    if state.theme_apply_pending {
        state.theme_apply_pending = false;
        theme::apply(&mut gpu.imgui);
    }

    // Keep the OS-level window title in sync with the active comparison.
    // Diff against the last value to avoid hammering winit each frame.
    let title = compute_window_title(state);
    if title != state.last_window_title {
        gpu.window.set_title(&title);
        state.last_window_title = title;
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

    // First frame just landed — reveal the window now that there's real
    // content in the framebuffer.
    if !state.window_shown {
        gpu.window.set_visible(true);
        state.window_shown = true;
    }

    // Recompute "any animation in flight?" for the active session — drives
    // the event loop's wait/poll decision in `about_to_wait`.
    state.animating = state
        .active
        .map(|id| {
            state.diff_views.get(&id).is_some_and(|v| v.is_animating())
                || state
                    .merge_views
                    .get(&id)
                    .is_some_and(|v| v.is_animating())
        })
        .unwrap_or(false);
}

// --- UI -------------------------------------------------------------------

fn frame_ui(ui: &imgui::Ui, state: &mut AppState) {
    if let Some(initial) = state.pending_initial.take() {
        match initial {
            InitialOpen::TwoWay { a, b } => open_two_way_paths(state, a, b),
            InitialOpen::ThreeWay { base, local, remote, result } => {
                open_three_way_paths_with_result(state, base, local, remote, Some(result));
            }
        }
    }
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
            if state.tabs.len() > 1 {
                tab_bar(ui, state);
                ui.separator();
            }
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
                .menu_item_config("Save Result")
                .shortcut("Ctrl+S")
                .enabled(is_three_way)
                .build()
            {
                save_result(state);
            }
            if ui
                .menu_item_config("Save Result As…")
                .enabled(is_three_way)
                .build()
            {
                save_result_as(state);
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
            Some(TabMode::ThreeWay) if !shift => save_result(state),
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

/// Clear the font atlas and re-add the UI font + code font.
///
/// UI font: Roboto Regular + Nerd icons (PUA) merged in. Unchanged.
///
/// Code font: user's `CodeFont` choice as primary, with Noto Sans Mono merged
/// in on the same glyph ranges (fills any codepoint the primary's TTF lacks
/// — imgui drops missing-glyph entries during atlas build, so the merge
/// genuinely fills holes rather than colliding), and the Nerd-Font icon
/// block (PUA) merged in last so icon codepoints render inline in code.
///
/// Returns the mono `FontId`.
fn load_fonts(imgui: &mut Context, ui_font_size: f32, code_font: fonts::CodeFont) -> FontId {
    let atlas = imgui.fonts();
    atlas.clear();
    let nerd_font_data: &'static [u8] =
        include_bytes!("../../assets/RobotoMonoNerdFont-Regular.ttf");
    atlas.add_font(&[
        FontSource::TtfData {
            data: aetna_fonts_roboto::ROBOTO_REGULAR,
            size_pixels: ui_font_size,
            config: Some(imgui::FontConfig {
                size_pixels: ui_font_size,
                glyph_ranges: FontGlyphRanges::from_slice(EXTRA_GLYPH_RANGES),
                ..Default::default()
            }),
        },
        // Merge nerd-font icons into the UI font on the private-use range.
        // imgui-rs sets MergeMode on every source after the first, so the
        // icon glyphs supplement Roboto Regular without replacing it.
        FontSource::TtfData {
            data: nerd_font_data,
            size_pixels: ui_font_size,
            config: Some(imgui::FontConfig {
                size_pixels: ui_font_size,
                glyph_ranges: FontGlyphRanges::from_slice(NERD_ICON_GLYPH_RANGES),
                ..Default::default()
            }),
        },
    ]);
    let code_size = ui_font_size * CODE_FONT_BASE_SCALE * code_font_zoom();
    atlas.add_font(&[
        FontSource::TtfData {
            data: code_font.bytes(),
            size_pixels: code_size,
            config: Some(imgui::FontConfig {
                size_pixels: code_size,
                glyph_ranges: FontGlyphRanges::from_slice(MONO_GLYPH_RANGES),
                ..Default::default()
            }),
        },
        FontSource::TtfData {
            data: fonts::NOTO_SANS_MONO,
            size_pixels: code_size,
            config: Some(imgui::FontConfig {
                size_pixels: code_size,
                glyph_ranges: FontGlyphRanges::from_slice(MONO_GLYPH_RANGES),
                ..Default::default()
            }),
        },
        FontSource::TtfData {
            data: nerd_font_data,
            size_pixels: code_size,
            config: Some(imgui::FontConfig {
                size_pixels: code_size,
                glyph_ranges: FontGlyphRanges::from_slice(NERD_ICON_GLYPH_RANGES),
                ..Default::default()
            }),
        },
    ])
}

/// Codepoint ranges loaded into the font atlas. Default imgui covers only
/// Basic Latin, which leaves UI strings full of `→ ↔ — … ✕ ⇒ ≥ Δ` rendering
/// as missing-glyph boxes. Each pair is an inclusive [start, end]; the slice
/// is zero-terminated as imgui requires.
///
/// Note: Roboto Regular does not cover every codepoint in these ranges
/// (e.g. U+2192 →, U+2194 ↔, U+2715 ✕ are all missing even though their
/// blocks are requested). Imgui shows `?` for unmapped codepoints, so
/// prefer characters Roboto actually ships (e.g. × U+00D7, — U+2014, …
/// U+2026) for UI labels — or fall back to ASCII like `->`.
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

/// Private-use codepoints covered by Nerd Fonts (Powerline, Devicons,
/// Font Awesome, Octicons, Material Design subset, Codicons, etc). The
/// Material Design block above U+10000 is excluded — imgui's default
/// ImWchar is 16-bit, so high-plane glyphs are not loadable.
#[rustfmt::skip]
static NERD_ICON_GLYPH_RANGES: &[u32] = &[
    0xE000, 0xF8FF, // Private Use Area — Nerd-Font icon block
    0,
];

/// Codepoints rasterized into the monospace (code) font atlas. Combines
/// the regular UI glyph set with the Nerd-Font icon block so icons can
/// appear inline in diff/merge text.
#[rustfmt::skip]
static MONO_GLYPH_RANGES: &[u32] = &[
    0x0020, 0x00FF,
    0x0370, 0x03FF,
    0x2010, 0x205E,
    0x2190, 0x21FF,
    0x2200, 0x22FF,
    0x2300, 0x23FF, // Miscellaneous Technical (⏎ U+23CE, used as EOL marker)
    0x2700, 0x27BF,
    0xE000, 0xF8FF, // Nerd-Font private-use icons
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
        // imgui's input_text_multiline keeps stale stb_textedit state across
        // frames; bumping input_epoch changes the widget ID so it
        // re-initialises from the post-undo buffer.
        if let Some(v) = state.diff_views.get_mut(&id) {
            v.input_epoch = v.input_epoch.wrapping_add(1);
        }
        if let Some(v) = state.merge_views.get_mut(&id) {
            v.input_epoch = v.input_epoch.wrapping_add(1);
        }
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
        if let Some(v) = state.diff_views.get_mut(&id) {
            v.input_epoch = v.input_epoch.wrapping_add(1);
        }
        if let Some(v) = state.merge_views.get_mut(&id) {
            v.input_epoch = v.input_epoch.wrapping_add(1);
        }
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
    // Closing the last comparison quits the app — matches the tab-bar
    // convention that "no tabs left" implies "no work to do here". Applies
    // uniformly to Ctrl+W, the menu item, and the in-tab close glyph.
    if state.tabs.is_empty() {
        state.quit_requested = true;
        return;
    }
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
    // With at most one comparison open, the tab strip is pure visual noise —
    // hide it. The caller still draws its separator unconditionally; we
    // simply contribute nothing this frame.
    if state.tabs.len() <= 1 {
        return;
    }

    // Custom-drawn tab strip:
    //   - Each tab is a single rounded rectangle that contains the badge
    //     icon, the label, and an inline close glyph. Hit testing uses
    //     one invisible_button per tab; clicks inside the close glyph's
    //     sub-rect close the tab, anywhere else activates it.
    //   - The active tab shares its fill colour with the horizontal rule
    //     drawn beneath the strip and extends a few pixels past the
    //     rule, so the tab body visually merges into the rule (the
    //     classic browser-tab look). Inactive tabs sit on top of the
    //     rule with a flat bottom.
    let dl = ui.get_window_draw_list();
    let mut new_active: Option<SessionId> = None;
    let mut close: Option<SessionId> = None;

    const PAD_X: f32 = 10.0;
    const PAD_Y: f32 = 6.0;
    const GAP_LABEL_CLOSE: f32 = 12.0;
    const TAB_ROUNDING: f32 = 8.0;
    const TAB_SPACING: f32 = 4.0;
    const UNDERLINE_THICKNESS: f32 = 2.0;

    let close_glyph = "\u{f00d}";
    let close_size = ui.calc_text_size(close_glyph);
    let mouse = ui.io().mouse_pos;
    // Active tab is a grey one step brighter than inactive; hover preview
    // uses the same brightness so hovering an inactive tab previews how it
    // will look when selected.
    let active_color = theme::SURFACE1();
    let inactive_color = theme::SURFACE0();
    let hover_color = theme::SURFACE1();
    let label_color = theme::TEXT();
    let close_idle = theme::OVERLAY1();
    let close_hover = theme::TEXT();

    let strip_left = ui.cursor_screen_pos()[0];
    let strip_y = ui.cursor_screen_pos()[1];
    let mut row_max_h = 0.0_f32;
    let mut active_extent: Option<(f32, f32)> = None;
    let mut last_right_x = strip_left;

    for tab in &state.tabs {
        let active = state.active == Some(tab.session_id);
        let badge = match tab.mode {
            TabMode::TwoWay => "\u{f0ec}",
            TabMode::ThreeWay => "\u{f126}",
        };
        let label_text = format!("{badge}  {}", tab.label);
        let label_size = ui.calc_text_size(&label_text);
        let tab_h = label_size[1].max(close_size[1]) + PAD_Y * 2.0;
        let tab_w = PAD_X + label_size[0] + GAP_LABEL_CLOSE + close_size[0] + PAD_X;

        let p = ui.cursor_screen_pos();
        let p_min = p;
        let p_max = [p[0] + tab_w, p[1] + tab_h];

        ui.invisible_button(format!("##tab_{}", tab.session_id), [tab_w, tab_h]);
        let hovered = ui.is_item_hovered();
        let clicked = ui.is_item_clicked();

        let close_x = p_max[0] - PAD_X - close_size[0];
        let close_y = p_min[1] + (tab_h - close_size[1]) * 0.5;
        let on_close = hovered
            && mouse[0] >= close_x - 3.0
            && mouse[0] <= close_x + close_size[0] + 3.0
            && mouse[1] >= close_y - 3.0
            && mouse[1] <= close_y + close_size[1] + 3.0;

        if clicked {
            if on_close {
                close = Some(tab.session_id);
            } else {
                new_active = Some(tab.session_id);
            }
        }

        let bg = if active {
            active_color
        } else if hovered {
            hover_color
        } else {
            inactive_color
        };

        // Active tab: extend past the underline so the two share an edge
        // visually. Inactive tabs: stop at the underline so the rule
        // shows beneath them.
        let bottom_y = if active {
            p_max[1] + UNDERLINE_THICKNESS
        } else {
            p_max[1]
        };
        dl.add_rect(p_min, [p_max[0], bottom_y], bg)
            .filled(true)
            .rounding(TAB_ROUNDING)
            .round_bot_left(false)
            .round_bot_right(false)
            .build();

        let text_y = p_min[1] + PAD_Y;
        dl.add_text([p_min[0] + PAD_X, text_y], label_color, &label_text);

        let close_color = if on_close { close_hover } else { close_idle };
        dl.add_text([close_x, close_y], close_color, close_glyph);

        if active {
            active_extent = Some((p_min[0], p_max[0]));
        }
        row_max_h = row_max_h.max(tab_h);
        last_right_x = p_max[0];

        ui.same_line_with_spacing(0.0, TAB_SPACING);
    }
    ui.new_line();

    // Horizontal rule beneath the strip — matches the active-tab fill so
    // the active tab appears to "sit in" the rule.
    let underline_y = strip_y + row_max_h;
    let win_pos = ui.window_pos();
    let win_size = ui.window_size();
    let line_left = win_pos[0];
    let line_right = win_pos[0] + win_size[0];
    let line_color = active_color;
    if let Some((ax, bx)) = active_extent {
        if ax > line_left {
            dl.add_line([line_left, underline_y + UNDERLINE_THICKNESS * 0.5],
                        [ax,        underline_y + UNDERLINE_THICKNESS * 0.5],
                        line_color)
                .thickness(UNDERLINE_THICKNESS)
                .build();
        }
        if bx < line_right {
            dl.add_line([bx,         underline_y + UNDERLINE_THICKNESS * 0.5],
                        [line_right, underline_y + UNDERLINE_THICKNESS * 0.5],
                        line_color)
                .thickness(UNDERLINE_THICKNESS)
                .build();
        }
    } else {
        dl.add_line([line_left,  underline_y + UNDERLINE_THICKNESS * 0.5],
                    [line_right, underline_y + UNDERLINE_THICKNESS * 0.5],
                    line_color)
            .thickness(UNDERLINE_THICKNESS)
            .build();
    }
    let _ = last_right_x;
    // Push the cursor below the underline so subsequent content doesn't
    // overlap the rule.
    let cur = ui.cursor_screen_pos();
    ui.set_cursor_screen_pos([cur[0], underline_y + UNDERLINE_THICKNESS + 4.0]);
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
        ui.text("Code font:");
        ui.same_line();
        let mut font_idx = fonts::ALL
            .iter()
            .position(|f| *f == state.preferences_draft.code_font)
            .unwrap_or(0);
        let font_labels: Vec<&str> = fonts::ALL.iter().map(|f| f.label()).collect();
        ui.set_next_item_width(260.0);
        if ui.combo_simple_string("##pref_code_font", &mut font_idx, &font_labels) {
            state.preferences_draft.code_font = fonts::ALL[font_idx];
        }

        ui.separator();
        ui.text("UI font size:");
        ui.same_line();
        ui.set_next_item_width(260.0);
        let mut ui_font_size = state.preferences_draft.ui_font_size;
        if imgui::Drag::new("##pref_ui_font_size")
            .range(preferences::MIN_UI_FONT_SIZE, preferences::MAX_UI_FONT_SIZE)
            .speed(0.1)
            .display_format("%.1f px")
            .build(ui, &mut ui_font_size)
        {
            state.preferences_draft.ui_font_size = ui_font_size.clamp(
                preferences::MIN_UI_FONT_SIZE,
                preferences::MAX_UI_FONT_SIZE,
            );
        }

        ui.separator();
        ui.text("Theme:");
        ui.same_line();
        const FLAVORS: &[theme::Flavor] = &[theme::Flavor::Macchiato, theme::Flavor::Latte];
        let mut theme_idx = FLAVORS
            .iter()
            .position(|f| *f == state.preferences_draft.theme)
            .unwrap_or(0);
        let theme_labels: Vec<&str> = FLAVORS.iter().map(|f| f.label()).collect();
        ui.set_next_item_width(260.0);
        if ui.combo_simple_string("##pref_theme", &mut theme_idx, &theme_labels) {
            state.preferences_draft.theme = FLAVORS[theme_idx];
        }

        ui.separator();
        if ui.button("OK") {
            let theme_changed = state.preferences.theme != state.preferences_draft.theme;
            let font_changed = state.preferences.code_font != state.preferences_draft.code_font
                || (state.preferences.ui_font_size - state.preferences_draft.ui_font_size).abs()
                    > f32::EPSILON;
            state.preferences = state.preferences_draft.clone();
            if font_changed {
                state.font_rebuild_pending = true;
            }
            if theme_changed {
                // Live-switch the palette accessor (theme::current()).
                // The App drives the actual imgui style re-application
                // outside this borrow via `theme_apply_pending`.
                theme::set_flavor(state.preferences.theme);
                state.theme_apply_pending = true;
            }
            syntax_paint::set_show_whitespace(state.preferences.show_whitespace);
            syntax_paint::set_eol_glyph(state.preferences.code_font.eol_codepoint());
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
        ui.text(format!("A:{} \u{f0ec} B:{}", a.a, a.b));
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

/// Save the merged result. If the active tab has a `result_path` bound
/// (via CLI args or a prior "Save Result As…"), write directly to it.
/// Otherwise fall through to the prompt-and-bind variant.
fn save_result(state: &mut AppState) {
    let Some(id) = state.active else {
        return;
    };
    let bound = state
        .tabs
        .iter()
        .find(|t| t.session_id == id)
        .and_then(|t| t.result_path.clone());
    let Some(path) = bound else {
        save_result_as(state);
        return;
    };
    write_result_to(state, id, &path);
}

/// Prompt for a result path, write the current merged result to it, and
/// bind the chosen path to the active tab so future Save (Ctrl+S) calls
/// write there without prompting.
fn save_result_as(state: &mut AppState) {
    let Some(id) = state.active else {
        return;
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title("Save merged result")
        .save_file()
    else {
        return;
    };
    write_result_to(state, id, &path);
    if let Some(tab) = state.tabs.iter_mut().find(|t| t.session_id == id) {
        tab.result_path = Some(path);
    }
}

fn write_result_to(state: &mut AppState, id: SessionId, path: &std::path::Path) {
    let text = match state.sessions.compute_result(id) {
        Ok(t) => t,
        Err(e) => {
            state.status = format!("compute error: {e}");
            return;
        }
    };
    match fileio::write_text(path, &text, false) {
        Ok(()) => state.status = format!("saved: {}", path.display()),
        Err(e) => state.status = format!("save error: {e}"),
    }
}

/// One pending action emitted by `pane_header_bar`, deferred so it can run
/// after the active snapshot/borrow falls out of scope and free state can
/// be mutated.
#[derive(Clone)]
enum HeaderAction {
    /// Open a file picker for pane `usize`.
    Browse(usize),
    /// Save pane `usize` to its bound path.
    Save(usize),
    /// Load the file at the typed path into pane `usize` (Enter pressed
    /// in the filename input).
    LoadTyped(usize, String),
}

/// Header strip above the diff/merge view: per-pane filename input with
/// an inline browse button, plus a save button. Returns the user's last
/// action this frame, if any, for deferred handling.
fn pane_header_bar(ui: &imgui::Ui, state: &mut AppState, id: SessionId) -> Option<HeaderAction> {
    let tab_idx = state.tabs.iter().position(|t| t.session_id == id)?;
    let mode = state.tabs[tab_idx].mode;
    // Role marker per pane. For 3-way panes the marker is the same
    // shape+color the result pane uses to identify each source
    // (diamond/sapphire = Remote, square/yellow = Base, circle/green = Local).
    // 2-way panes have no marker.
    use crate::session::ThreeWaySide;
    let (segments, role_entries): (usize, &[(Option<ThreeWaySide>, &str)]) = match mode {
        TabMode::TwoWay => (2, &[(None, "A"), (None, "B")]),
        TabMode::ThreeWay => (
            3,
            &[
                (Some(ThreeWaySide::Remote), "REMOTE"),
                (Some(ThreeWaySide::Base), "BASE"),
                (Some(ThreeWaySide::Local), "LOCAL"),
            ],
        ),
    };
    if state.tabs[tab_idx].path_inputs.len() < segments {
        state.tabs[tab_idx]
            .path_inputs
            .resize(segments, String::new());
    }

    // nf-fa-ellipsis-h (\u{f141}) = browse "…", nf-fa-floppy_o (\u{f0c7}) = save.
    let browse_glyph = "\u{f141}";
    let save_glyph = "\u{f0c7}";

    let avail_w = ui.content_region_avail()[0];
    let seg_gap = 8.0;
    let seg_w = ((avail_w - seg_gap * (segments as f32 - 1.0)) / segments as f32).max(160.0);
    const ROLE_ICON_HALF: f32 = 5.5;
    const ROLE_ICON_GAP: f32 = 6.0;
    let icon_slot_w =
        |has_icon: bool| if has_icon { ROLE_ICON_HALF * 2.0 + ROLE_ICON_GAP } else { 0.0 };
    let role_w = role_entries
        .iter()
        .map(|(side, label)| {
            icon_slot_w(side.is_some()) + ui.calc_text_size(format!("[{label}]"))[0]
        })
        .fold(0.0_f32, f32::max);
    let save_btn_w = ui.calc_text_size(save_glyph)[0] + 14.0;
    let browse_btn_w = ui.calc_text_size(browse_glyph)[0] + 12.0;
    let inline_gap = 4.0;

    let start = ui.cursor_screen_pos();
    let mut action: Option<HeaderAction> = None;

    for i in 0..segments {
        let seg_left = start[0] + i as f32 * (seg_w + seg_gap);
        ui.set_cursor_screen_pos([seg_left, start[1]]);

        ui.align_text_to_frame_padding();
        let (side, label) = role_entries[i];
        // Draw the source-color shape (3-way only) just left of the label,
        // vertically centered on the row, then advance the cursor past it.
        let mut label_x = seg_left;
        if let Some(s) = side {
            let cy = start[1] + ui.frame_height() * 0.5;
            let cx = seg_left + ROLE_ICON_HALF;
            result_pane::paint_role_icon(ui, [cx, cy], s, ROLE_ICON_HALF);
            label_x = seg_left + ROLE_ICON_HALF * 2.0 + ROLE_ICON_GAP;
        }
        ui.set_cursor_screen_pos([label_x, start[1]]);
        ui.text_disabled(format!("[{label}]"));
        ui.same_line();
        ui.set_cursor_screen_pos([seg_left + role_w + 8.0, start[1]]);

        // The visual field stretches across `field_w`; the input widget
        // itself only owns the left portion. The browse button gets its
        // own hit area on the right. To make the two read as a single
        // field, we paint a frame-bg rectangle behind the button (same
        // color as the input's bg) before drawing the button, and we
        // null the button's own bg so only its glyph + hover/active
        // highlight show.
        let field_w = seg_w - role_w - 8.0 - save_btn_w - inline_gap;
        let input_w = (field_w - browse_btn_w - 2.0).max(40.0);
        let input_origin_x = ui.cursor_screen_pos()[0];
        let input_origin_y = ui.cursor_screen_pos()[1];

        let _w = ui.push_item_width(input_w);
        let buf = &mut state.tabs[tab_idx].path_inputs[i];
        let input_id = format!("##path_{i}_{}", id);
        let activated = ui
            .input_text(&input_id, buf)
            .enter_returns_true(true)
            .build();
        if activated {
            action = Some(HeaderAction::LoadTyped(i, buf.clone()));
        }
        drop(_w);
        let input_rect_min = ui.item_rect_min();
        let input_rect_max = ui.item_rect_max();
        let field_rect_max_x = input_origin_x + field_w;

        // Visually extend the input's background to cover the button
        // strip. Drawn on the *window* draw list (above the input's bg
        // but below the button) so the field reads as one continuous
        // rounded frame.
        let style = ui.clone_style();
        let frame_bg = style.colors[imgui::StyleColor::FrameBg as usize];
        let frame_bg_active = style.colors[imgui::StyleColor::FrameBgHovered as usize];
        let dl = ui.get_window_draw_list();
        let frame_rounding = style.frame_rounding;
        let extension_color: imgui::ImColor32 =
            if ui.is_item_active() || ui.is_item_focused() {
                frame_bg_active
            } else {
                frame_bg
            }
            .into();
        dl.add_rect(
            [input_rect_max[0], input_rect_min[1]],
            [field_rect_max_x, input_rect_max[1]],
            extension_color,
        )
        .filled(true)
        .rounding(frame_rounding)
        .round_top_left(false)
        .round_bot_left(false)
        .build();

        // Browse button placed in the extension strip with transparent
        // bg so it visually fuses with the field.
        let btn_h = input_rect_max[1] - input_rect_min[1];
        let btn_x = field_rect_max_x - browse_btn_w;
        ui.set_cursor_screen_pos([btn_x, input_origin_y]);
        let _button_bg = ui.push_style_color(imgui::StyleColor::Button, [0.0, 0.0, 0.0, 0.0]);
        if ui.button_with_size(
            format!("{browse_glyph}##browse_{i}_{}", id),
            [browse_btn_w, btn_h],
        ) {
            action = Some(HeaderAction::Browse(i));
        }
        drop(_button_bg);

        // Save button to the right of the field.
        ui.set_cursor_screen_pos([field_rect_max_x + inline_gap, input_origin_y]);
        if ui.button_with_size(
            format!("{save_glyph}##save_{i}_{}", id),
            [save_btn_w, btn_h],
        ) {
            action = Some(HeaderAction::Save(i));
        }
    }
    // Push cursor down past the row.
    ui.set_cursor_screen_pos([start[0], start[1] + ui.frame_height() + 2.0]);
    action
}

/// Map a 0-based pane index to the per-mode SideRef, then read the file
/// and rewrite the session's side text.
fn browse_replace_side_at(state: &mut AppState, id: SessionId, idx: usize) {
    let Some(tab_idx) = state.tabs.iter().position(|t| t.session_id == id) else {
        return;
    };
    let mode = state.tabs[tab_idx].mode;
    let side = match (mode, idx) {
        (TabMode::TwoWay, 0) => crate::session::SideRef::TwoWay(crate::session::TwoWaySide::A),
        (TabMode::TwoWay, 1) => crate::session::SideRef::TwoWay(crate::session::TwoWaySide::B),
        (TabMode::ThreeWay, 0) => crate::session::SideRef::ThreeWay(crate::session::ThreeWaySide::Remote),
        (TabMode::ThreeWay, 1) => crate::session::SideRef::ThreeWay(crate::session::ThreeWaySide::Base),
        (TabMode::ThreeWay, 2) => crate::session::SideRef::ThreeWay(crate::session::ThreeWaySide::Local),
        _ => return,
    };
    let Some(path) = pick_file("Open file") else {
        return;
    };
    let read = match fileio::read_text(&path) {
        Ok(r) => r,
        Err(e) => {
            state.status = format!("Read error: {e}");
            return;
        }
    };
    if let Err(e) = state.sessions.set_side_text(id, side, read.text) {
        state.status = format!("Set side error: {e}");
        return;
    }
    // Update the tab's stored path, mirror it into the header input
    // buffer, and refresh the tab label.
    if let Some(t) = state.tabs.get_mut(tab_idx) {
        if let Some(slot) = t.paths.get_mut(idx) {
            *slot = path.clone();
        }
        if t.path_inputs.len() < t.paths.len() {
            t.path_inputs.resize(t.paths.len(), String::new());
        }
        if let Some(buf) = t.path_inputs.get_mut(idx) {
            *buf = path.display().to_string();
        }
        t.label = match t.mode {
            TabMode::TwoWay => format!(
                "{} \u{f0ec} {}",
                pretty_basename(t.paths.first()),
                pretty_basename(t.paths.get(1))
            ),
            TabMode::ThreeWay => pretty_basename(t.paths.get(1)),
        };
    }
    // Bump the input epoch so imgui re-initialises the multiline text edit
    // state from the new buffer (mirrors what undo/redo does).
    match mode {
        TabMode::TwoWay => {
            if let Some(v) = state.diff_views.get_mut(&id) {
                v.input_epoch = v.input_epoch.wrapping_add(1);
            }
        }
        TabMode::ThreeWay => {
            if let Some(v) = state.merge_views.get_mut(&id) {
                v.input_epoch = v.input_epoch.wrapping_add(1);
            }
        }
    }
    state.status = format!("loaded: {}", path.display());
}

/// Save the side at pane index `idx` for the active tab to its stored path.
fn save_side_at_idx(state: &mut AppState, id: SessionId, idx: usize) {
    let Some(tab) = state.tabs.iter().find(|t| t.session_id == id) else {
        return;
    };
    match (tab.mode, idx) {
        (TabMode::TwoWay, 0) => save_two_way_side(state, crate::session::TwoWaySide::A),
        (TabMode::TwoWay, 1) => save_two_way_side(state, crate::session::TwoWaySide::B),
        (TabMode::ThreeWay, i) => save_three_way_side(state, id, i),
        _ => {}
    }
}

/// Load whatever path the user typed into pane `idx`'s filename input
/// (triggered by Enter). Mirrors `browse_replace_side_at` but starts from
/// a typed string rather than a file dialog.
fn load_typed_path_into_side(state: &mut AppState, id: SessionId, idx: usize, path: String) {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        state.status = "empty path".into();
        return;
    }
    let path = PathBuf::from(trimmed);
    let Some(tab_idx) = state.tabs.iter().position(|t| t.session_id == id) else {
        return;
    };
    let mode = state.tabs[tab_idx].mode;
    let side = match (mode, idx) {
        (TabMode::TwoWay, 0) => crate::session::SideRef::TwoWay(crate::session::TwoWaySide::A),
        (TabMode::TwoWay, 1) => crate::session::SideRef::TwoWay(crate::session::TwoWaySide::B),
        (TabMode::ThreeWay, 0) => crate::session::SideRef::ThreeWay(crate::session::ThreeWaySide::Remote),
        (TabMode::ThreeWay, 1) => crate::session::SideRef::ThreeWay(crate::session::ThreeWaySide::Base),
        (TabMode::ThreeWay, 2) => crate::session::SideRef::ThreeWay(crate::session::ThreeWaySide::Local),
        _ => return,
    };
    let read = match fileio::read_text(&path) {
        Ok(r) => r,
        Err(e) => {
            state.status = format!("Read error: {e}");
            return;
        }
    };
    if let Err(e) = state.sessions.set_side_text(id, side, read.text) {
        state.status = format!("Set side error: {e}");
        return;
    }
    if let Some(t) = state.tabs.get_mut(tab_idx) {
        if let Some(slot) = t.paths.get_mut(idx) {
            *slot = path.clone();
        }
        if let Some(buf) = t.path_inputs.get_mut(idx) {
            *buf = path.display().to_string();
        }
        t.label = match t.mode {
            TabMode::TwoWay => format!(
                "{} \u{f0ec} {}",
                pretty_basename(t.paths.first()),
                pretty_basename(t.paths.get(1))
            ),
            TabMode::ThreeWay => pretty_basename(t.paths.get(1)),
        };
    }
    match mode {
        TabMode::TwoWay => {
            if let Some(v) = state.diff_views.get_mut(&id) {
                v.input_epoch = v.input_epoch.wrapping_add(1);
            }
        }
        TabMode::ThreeWay => {
            if let Some(v) = state.merge_views.get_mut(&id) {
                v.input_epoch = v.input_epoch.wrapping_add(1);
            }
        }
    }
    state.status = format!("loaded: {}", path.display());
}

fn save_three_way_side(state: &mut AppState, id: SessionId, idx: usize) {
    let Some(tab) = state.tabs.iter().find(|t| t.session_id == id) else {
        return;
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
    let crate::session::SessionMode::ThreeWay {
        base_text,
        local_text,
        remote_text,
        base_trailing_newline,
        local_trailing_newline,
        remote_trailing_newline,
        ..
    } = &snap.mode
    else {
        state.status = "active session is not 3-way".into();
        return;
    };
    let (text, trailing, role) = match idx {
        0 => (remote_text, *remote_trailing_newline, "REMOTE"),
        1 => (base_text, *base_trailing_newline, "BASE"),
        2 => (local_text, *local_trailing_newline, "LOCAL"),
        _ => return,
    };
    match fileio::write_text(&path, text, trailing) {
        Ok(()) => {
            state.status = format!("saved {role}: {}", path.display());
        }
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
    // Snapshot the tab's paths up front so we can release the immutable
    // borrow before pane_header_bar takes state mutably.
    let tab_paths_snap: Option<Vec<PathBuf>> = state
        .tabs
        .iter()
        .find(|t| t.session_id == id)
        .map(|t| t.paths.clone());
    engine_bar::render(
        ui,
        &state.sessions,
        id,
        &snap.engine,
        snap.options,
        &mut state.preferences,
        &mut state.status,
    );
    ui.separator();
    // Per-pane header strip (filename + browse + save) is rendered per-mode,
    // immediately above the code views — so it sits directly atop the panes
    // it labels, below the engine bar / three-way header / anchor bar.
    // Actions are deferred (the dialog/save happens after the snapshot
    // borrow ends) so we can mutate state freely.
    let header_action: Option<HeaderAction>;
    match &snap.mode {
        SessionMode::TwoWay { hunks, anchors, a_text, b_text, .. } => {
            anchor_bar_two_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
            header_action = pane_header_bar(ui, state, id);
            ui.separator();
            // 2-way edits the source files directly — there is no separate
            // "result" so the diff fills the remaining vertical space.
            let store = &state.sessions;
            let status = &mut state.status;
            let mono = state.mono_font;
            // Resolve per-side language from the tab's stored file paths,
            // then compute (or reuse) per-line highlight spans via the
            // tree-sitter cache.
            let (a_lang, b_lang) = match &tab_paths_snap {
                Some(paths) => (
                    paths.first().and_then(|p| syntax::lang_for_path(p)),
                    paths.get(1).and_then(|p| syntax::lang_for_path(p)),
                ),
                None => (None, None),
            };
            let a_key = id << 2;
            let b_key = (id << 2) | 1;
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
                let mut needs_epoch_bump = false;
                for edit in pending_edits {
                    if !matches!(edit, undo_stack::DiffEdit::SetSide { .. }) {
                        // ReplaceHunkSide (Apply A->B / B->A) is an external
                        // mutation from the widget's POV — bump the epoch so
                        // imgui re-initialises stb_textedit from the new buf.
                        needs_epoch_bump = true;
                    }
                    record.edit(&mut state.sessions, edit);
                }
                if needs_epoch_bump {
                    if let Some(v) = state.diff_views.get_mut(&id) {
                        v.input_epoch = v.input_epoch.wrapping_add(1);
                    }
                }
                state.status = "edited (Ctrl+Z to undo)".to_string();
            }
        }
        SessionMode::ThreeWay { hunks, anchors, resolutions, base_text, local_text, remote_text, .. } => {
            let counts = three_way_header::count_hunks(hunks);
            three_way_header::render(ui, counts);
            ui.separator();
            anchor_bar_three_way(ui, &state.sessions, id, anchors, &mut state.status);
            ui.separator();
            header_action = pane_header_bar(ui, state, id);
            ui.separator();
            // Per-side syntax highlights for the three input panes, mirroring
            // the 2-way path. Each side may have a different language if the
            // user picked mixed extensions.
            // tab.paths is laid out as [REMOTE, BASE, LOCAL] to match the
            // header strip and pane order.
            let (base_lang, local_lang, remote_lang) = match &tab_paths_snap {
                Some(paths) => (
                    paths.get(1).and_then(|p| syntax::lang_for_path(p)),
                    paths.get(2).and_then(|p| syntax::lang_for_path(p)),
                    paths.first().and_then(|p| syntax::lang_for_path(p)),
                ),
                None => (None, None, None),
            };
            let base_key   = (id << 2) | 0;
            let local_key  = (id << 2) | 1;
            let remote_key = (id << 2) | 2;
            let base_lines:   Vec<String> = crate::session::lines_of(base_text)
                .into_iter().map(|s| s.to_string()).collect();
            let local_lines:  Vec<String> = crate::session::lines_of(local_text)
                .into_iter().map(|s| s.to_string()).collect();
            let remote_lines: Vec<String> = crate::session::lines_of(remote_text)
                .into_iter().map(|s| s.to_string()).collect();
            let base_h   = state.syntax.highlights(base_key,   base_lang,   &base_lines).to_vec();
            let local_h  = state.syntax.highlights(local_key,  local_lang,  &local_lines).to_vec();
            let remote_h = state.syntax.highlights(remote_key, remote_lang, &remote_lines).to_vec();
            // Result-pane highlights: use BASE's language as the canonical
            // file type for the merged output. The cache key uses bits 11
            // which the three input-pane keys (00/01/10) don't touch.
            let result_text = state.sessions.compute_result(id).unwrap_or_default();
            let result_lines: Vec<String> = result_text.lines().map(String::from).collect();
            let result_key = (id << 2) | 3;
            let result_highlights = state.syntax.highlights(result_key, base_lang, &result_lines).to_vec();
            let avail = ui.content_region_avail();
            const SPLITTER_H: f32 = 6.0;
            let default_result_h = avail[1] * 0.5;
            let stored_h = state
                .result_panes
                .get(&id)
                .and_then(|r| r.pane_height);
            let min_pane_h = 50.0_f32;
            let max_result_h = (avail[1] - SPLITTER_H - min_pane_h).max(min_pane_h);
            let result_h = stored_h
                .unwrap_or(default_result_h)
                .clamp(min_pane_h, max_result_h);
            let diff_h = (avail[1] - result_h - SPLITTER_H).max(min_pane_h);
            // Snapshot the 4 scroll targets BEFORE rendering so the post-render
            // sync pass can detect which pane (if any) was user-driven this
            // frame. Slots 0..3 are Base/Local/Remote/Result.
            let prev_scroll_targets: [f32; 4] = state
                .merge_views
                .entry(id)
                .or_default()
                .target;

            let upper_ranges;
            {
                let store = &state.sessions;
                let status = &mut state.status;
                let mono = state.mono_font;
                let view_state = state.merge_views.entry(id).or_default();
                let mut focus_request: Option<FocusedPane> = None;
                let mut pending_edits: Vec<undo_stack::DiffEdit> = Vec::new();
                let mut ranges_out: Option<merge_view::UpperPaneRanges> = None;
                ui.child_window("merge_area")
                    .size([0.0, diff_h])
                    .build(|| {
                        ranges_out = Some(merge_view::render(
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
                            &base_h,
                            &local_h,
                            &remote_h,
                        ));
                    });
                upper_ranges = ranges_out.unwrap_or(merge_view::UpperPaneRanges {
                    base: Vec::new(),
                    local: Vec::new(),
                    remote: Vec::new(),
                    view_h: 0.0,
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
            // Splitter between the merge area and the result pane. Drag to
            // resize the result pane; the new height persists in ResultState.
            {
                let splitter_pos = ui.cursor_screen_pos();
                let avail_w = ui.content_region_avail()[0];
                ui.invisible_button(
                    format!("##result_splitter_{id}"),
                    [avail_w, SPLITTER_H],
                );
                let hovered = ui.is_item_hovered();
                let active = ui.is_item_active();
                if hovered || active {
                    ui.set_mouse_cursor(Some(imgui::MouseCursor::ResizeNS));
                }
                if active {
                    let dy = ui.io().mouse_delta[1];
                    if dy != 0.0 {
                        let new_h = (result_h - dy).clamp(min_pane_h, max_result_h);
                        state
                            .result_panes
                            .entry(id)
                            .or_default()
                            .pane_height = Some(new_h);
                    }
                }
                let dl = ui.get_window_draw_list();
                let color = if active {
                    theme::with_alpha(theme::TEXT(), 0.50)
                } else if hovered {
                    theme::with_alpha(theme::TEXT(), 0.30)
                } else {
                    theme::with_alpha(theme::TEXT(), 0.12)
                };
                let pad = 1.0;
                dl.add_rect(
                    [splitter_pos[0], splitter_pos[1] + pad],
                    [splitter_pos[0] + avail_w, splitter_pos[1] + SPLITTER_H - pad],
                    color,
                )
                .filled(true)
                .build();
            }
            {
                let mono = state.mono_font;
                let result = state.result_panes.entry(id).or_default();
                let view_state = state.merge_views.entry(id).or_default();
                let mut focus_request: Option<FocusedPane> = None;
                let mut result_sync: Option<result_pane::ResultPaneSync> = None;
                ui.child_window("result_area")
                    .size([0.0, 0.0])
                    .border(true)
                    .build(|| {
                        result_sync = Some(result_pane::render(
                            ui,
                            &state.sessions,
                            id,
                            result,
                            view_state,
                            mono,
                            &mut focus_request,
                            hunks,
                            resolutions,
                            &result_highlights,
                        ));
                    });
                if let Some(p) = focus_request {
                    state.focused = Some((id, p));
                }

                // Unified 4-way scroll sync. Slot order matches MergeViewState:
                // [Base, Local, Remote, Result].
                let rs = result_sync.unwrap_or_default();
                merge_view::sync_scrolls(
                    view_state,
                    prev_scroll_targets,
                    [
                        upper_ranges.view_h,
                        upper_ranges.view_h,
                        upper_ranges.view_h,
                        rs.view_h,
                    ],
                    [
                        &upper_ranges.base,
                        &upper_ranges.local,
                        &upper_ranges.remote,
                        &rs.ranges,
                    ],
                );
            }
        }
    }

    // Snapshot/borrow is out of scope here — safe to mutate state via the
    // dialog/save helpers based on the header action gathered earlier.
    drop(snap);
    match header_action {
        Some(HeaderAction::Browse(idx)) => browse_replace_side_at(state, id, idx),
        Some(HeaderAction::Save(idx)) => save_side_at_idx(state, id, idx),
        Some(HeaderAction::LoadTyped(idx, path)) => load_typed_path_into_side(state, id, idx, path),
        None => {}
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

/// Build the OS-level window title from the active tab. 2-way appends
/// both file basenames; 3-way appends only the result filename (falling
/// back to the base file's name when no result path is bound yet).
fn compute_window_title(state: &AppState) -> String {
    let Some(id) = state.active else {
        return "Diffie".to_string();
    };
    let Some(tab) = state.tabs.iter().find(|t| t.session_id == id) else {
        return "Diffie".to_string();
    };
    match tab.mode {
        TabMode::TwoWay => {
            let a = pretty_basename(tab.paths.first());
            let b = pretty_basename(tab.paths.get(1));
            format!("Diffie \u{2014} {a} \u{2014} {b}")
        }
        TabMode::ThreeWay => {
            let name = tab
                .result_path
                .as_ref()
                .map(|p| basename(p))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| pretty_basename(tab.paths.get(1)));
            format!("Diffie \u{2014} {name}")
        }
    }
}

/// Tab-label-friendly version of `basename`: returns `(untitled)` for
/// `None` or empty paths so the rendered label never has a blank
/// segment around the connecting icon.
fn pretty_basename(p: Option<&PathBuf>) -> String {
    match p {
        Some(p) if !p.as_os_str().is_empty() => basename(p),
        _ => "(untitled)".to_string(),
    }
}

fn pick_file(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_file()
}

/// Open a fresh 2-way tab with two empty buffers and no bound paths.
/// The user fills the panes via the per-pane filename field (typed
/// path + Enter, or the `…` browse button) in the view's header strip.
fn open_two_way(state: &mut AppState) {
    let engine = Some(state.preferences.default_engine.clone());
    let opts = state.preferences.default_options;
    match state.sessions.open_two_way_with(
        String::new(),
        String::new(),
        false,
        false,
        engine,
        opts,
    ) {
        Ok(id) => {
            let label = "(untitled) \u{f0ec} (untitled)".to_string();
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::TwoWay,
                paths: vec![PathBuf::new(), PathBuf::new()],
                result_path: None,
                path_inputs: vec![String::new(), String::new()],
            });
            state.active = Some(id);
            state.status = "Opened empty 2-way".into();
        }
        Err(e) => state.status = format!("Open 2-way failed: {e}"),
    }
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
            // nf-fa-exchange between the two filenames; matches the 2-way
            // tab badge so the icon language stays consistent.
            let label = format!("{} \u{f0ec} {}", basename(&a), basename(&b));
            let recent = recents::RecentEntry::TwoWay {
                a: a.clone(),
                b: b.clone(),
            };
            let paths = vec![a, b];
            let path_inputs = paths.iter().map(|p| p.display().to_string()).collect();
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::TwoWay,
                paths,
                result_path: None,
                path_inputs,
            });
            state.active = Some(id);
            state.status = format!("Opened 2-way: {label}");
            recents::add(&mut state.recents, recent);
            // Snap the new view's first-frame scroll to the first non-equal
            // hunk so the user lands on actual content instead of two empty
            // file tops. Pre-creates the DiffViewState (normally lazy).
            if let Ok(snap) = state.sessions.snapshot(id) {
                if let SessionMode::TwoWay { hunks, .. } = &snap.mode {
                    if let Some((a_line, b_line)) = diff_view::first_change_lines(hunks) {
                        let view = state.diff_views.entry(id).or_default();
                        view.pending_initial_a_line = Some(a_line);
                        view.pending_initial_b_line = Some(b_line);
                    }
                }
            }
        }
        Err(e) => state.status = format!("Open 2-way failed: {e}"),
    }
}

fn open_three_way(state: &mut AppState) {
    let engine = Some(state.preferences.default_engine.clone());
    let opts = state.preferences.default_options;
    match state.sessions.open_three_way_with(
        String::new(),
        String::new(),
        String::new(),
        false,
        false,
        false,
        engine,
        opts,
    ) {
        Ok(id) => {
            let label = "(untitled)".to_string();
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::ThreeWay,
                paths: vec![PathBuf::new(), PathBuf::new(), PathBuf::new()],
                result_path: None,
                path_inputs: vec![String::new(), String::new(), String::new()],
            });
            state.active = Some(id);
            state.status = "Opened empty 3-way".into();
        }
        Err(e) => state.status = format!("Open 3-way failed: {e}"),
    }
}

fn open_three_way_paths(
    state: &mut AppState,
    base: PathBuf,
    local: PathBuf,
    remote: PathBuf,
) {
    open_three_way_paths_with_result(state, base, local, remote, None);
}

fn open_three_way_paths_with_result(
    state: &mut AppState,
    base: PathBuf,
    local: PathBuf,
    remote: PathBuf,
    result: Option<PathBuf>,
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
            // The tab badge already shows the code-fork icon; the label
            // here keeps just the base filename so we don't double up.
            let label = basename(&base);
            let recent = recents::RecentEntry::ThreeWay {
                base: base.clone(),
                local: local.clone(),
                remote: remote.clone(),
            };
            // If a result path was supplied and the file already exists,
            // pre-load it as the manual result so the user sees their
            // in-progress merge buffer.
            if let Some(rp) = result.as_ref() {
                if rp.exists() {
                    match fileio::read_text(rp) {
                        Ok(t) => {
                            let _ = state.sessions.update_manual_result(id, t.text);
                        }
                        Err(e) => {
                            state.status = format!("Read error (RESULT): {e}");
                        }
                    }
                }
            }
            let paths = vec![remote, base, local];
            let path_inputs = paths.iter().map(|p| p.display().to_string()).collect();
            state.tabs.push(Tab {
                session_id: id,
                label: label.clone(),
                mode: TabMode::ThreeWay,
                paths,
                result_path: result,
                path_inputs,
            });
            state.active = Some(id);
            state.status = format!("Opened 3-way: {label}");
            recents::add(&mut state.recents, recent);
            // Focus the first non-Stable hunk per pane so the user lands on
            // the first conflict / divergence rather than at the top.
            if let Ok(snap) = state.sessions.snapshot(id) {
                if let SessionMode::ThreeWay { hunks, .. } = &snap.mode {
                    if let Some((b, l, r)) = merge_view::first_change_lines(hunks) {
                        let view = state.merge_views.entry(id).or_default();
                        // Pane order in MergeViewState arrays mirrors the
                        // `Pane` enum: 0 = Base, 1 = Local, 2 = Remote.
                        view.pending_initial_line = [Some(b), Some(l), Some(r)];
                    }
                }
            }
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
