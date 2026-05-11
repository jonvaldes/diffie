//! Native GUI entry point.
//!
//! Sets up winit (0.30 `ApplicationHandler`) + wgpu + imgui-rs and drives the
//! frame loop. The actual diff UI is rendered inside `frame_ui()`; everything
//! else here is plumbing that shouldn't change as the UI grows.

use std::sync::Arc;
use std::time::Instant;

use imgui::{Context, FontSource};
use imgui_wgpu::{Renderer, RendererConfig};
use imgui_winit_support::{HiDpiMode, WinitPlatform};
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

const INITIAL_WIDTH: u32 = 1400;
const INITIAL_HEIGHT: u32 = 900;

pub fn run() {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App::default();
    event_loop.run_app(&mut app).expect("run event loop");
}

#[derive(Default)]
struct AppState {
    // Real state lands here as features come online (sessions, tabs, status).
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
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("diffie-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu::Trace::default(),
            },
        ))
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
        imgui.fonts().add_font(&[FontSource::DefaultFontData {
            config: Some(imgui::FontConfig {
                size_pixels: font_size,
                ..Default::default()
            }),
        }]);

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
        // Forward to imgui first. We reconstruct an Event::WindowEvent because
        // imgui-winit-support takes the full Event enum.
        let full_event: winit::event::Event<()> = winit::event::Event::WindowEvent {
            window_id: gpu.window.id(),
            event: event.clone(),
        };
        gpu.platform.handle_event(gpu.imgui.io_mut(), &gpu.window, &full_event);

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

fn frame_ui(ui: &imgui::Ui, _state: &mut AppState) {
    let display = ui.io().display_size;
    ui.window("Diffie")
        .position([0.0, 0.0], imgui::Condition::Always)
        .size(display, imgui::Condition::Always)
        .flags(
            imgui::WindowFlags::NO_DECORATION
                | imgui::WindowFlags::NO_MOVE
                | imgui::WindowFlags::NO_RESIZE
                | imgui::WindowFlags::NO_BRING_TO_FRONT_ON_FOCUS,
        )
        .build(|| {
            ui.text("Diffie — native GUI bootstrap");
            ui.text_disabled("imgui + wgpu + winit running. Diff UI lands next.");
        });
}
