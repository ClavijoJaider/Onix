// Spike de memoria: winit + wgpu(Metal) + glyphon, grilla estática 80x24.
use glyphon::*;
use std::sync::Arc;
use winit::{dpi::LogicalSize, event::WindowEvent, event_loop::EventLoop, window::Window};

fn env_flag(k: &str) -> bool { std::env::var_os(k).is_some() }

struct St {
    window: Arc<Window>, surface: wgpu::Surface<'static>, cfg: wgpu::SurfaceConfiguration,
    device: wgpu::Device, queue: wgpu::Queue, fs: FontSystem, sc: SwashCache,
    vp: Viewport, atlas: TextAtlas, tr: TextRenderer, buf: Buffer,
}
struct A(Option<St>);

impl winit::application::ApplicationHandler for A {
    fn resumed(&mut self, el: &winit::event_loop::ActiveEventLoop) {
        if self.0.is_some() { return; }
        let window = Arc::new(el.create_window(Window::default_attributes()
            .with_inner_size(LogicalSize::new(960.0, 600.0)).with_title("spike")).unwrap());
        let sz = window.inner_size();
        if let Ok(st) = std::env::var("STAGE") { if st == "window" { self.0 = None; std::mem::forget(window); return; } }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle())));
        let surface = instance.create_surface(window.clone()).unwrap();
        let low = env_flag("LOWPOWER");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: if low { wgpu::PowerPreference::LowPower } else { wgpu::PowerPreference::HighPerformance },
            compatible_surface: Some(&surface), ..Default::default() })).unwrap();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: if env_flag("MINLIMITS") { wgpu::Limits::downlevel_defaults() } else { wgpu::Limits::default() },
            memory_hints: if low { wgpu::MemoryHints::MemoryUsage } else { wgpu::MemoryHints::Performance },
            ..Default::default() })).unwrap();
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let cfg = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format, width: sz.width, height: sz.height,
            present_mode: wgpu::PresentMode::Fifo, alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![], desired_maximum_frame_latency: if env_flag("LAT1") { 1 } else { 2 },
            color_space: wgpu::SurfaceColorSpace::Auto };
        surface.configure(&device, &cfg);
        if std::env::var("STAGE").as_deref() == Ok("wgpu") { std::mem::forget((window, surface, device, queue)); return; }

        let mut fs = if env_flag("ALLFONTS") { FontSystem::new() } else {
            let mut db = fontdb::Database::new();
            for p in ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Apple Symbols.ttf"] {
                let _ = db.load_font_file(p);
            }
            FontSystem::new_with_locale_and_db("en-US".into(), db)
        };
        if std::env::var("STAGE").as_deref() == Ok("fonts") { std::mem::forget((window, surface, device, queue, fs)); return; }
        let cache = Cache::new(&device);
        let vp = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let tr = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let mut buf = Buffer::new(&mut fs, Metrics::new(26.0, 34.0));
        buf.set_size(Some(sz.width as f32), Some(sz.height as f32));
        let mut text = String::new();
        for r in 0..24 {
            text.push_str(&format!("{r:02} Hello mi-terminal ⏵⏵ auto mode on ⎿ resultado ░▒▓█ ┌──┐ {}\n", "abcdefghij".repeat(3)));
        }
        buf.set_text(&text, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
        buf.shape_until_scroll(&mut fs, false);
        self.0 = Some(St { window, surface, cfg, device, queue, fs, sc: SwashCache::new(), vp, atlas, tr, buf });
        eprintln!("PID {}", std::process::id());
    }
    fn window_event(&mut self, el: &winit::event_loop::ActiveEventLoop, _: winit::window::WindowId, ev: WindowEvent) {
        let Some(s) = &mut self.0 else { return };
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(z) => { s.cfg.width = z.width.max(1); s.cfg.height = z.height.max(1); s.surface.configure(&s.device, &s.cfg); s.window.request_redraw(); }
            WindowEvent::RedrawRequested => {
                s.vp.update(&s.queue, Resolution { width: s.cfg.width, height: s.cfg.height });
                s.tr.prepare(&s.device, &s.queue, &mut s.fs, &mut s.atlas, &s.vp, [TextArea {
                    buffer: &s.buf, left: 8.0, top: 8.0, scale: 1.0,
                    bounds: TextBounds { left: 0, top: 0, right: s.cfg.width as i32, bottom: s.cfg.height as i32 },
                    default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] }], &mut s.sc).unwrap();
                let frame = match s.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f) => f,
                    _ => { s.surface.configure(&s.device, &s.cfg); s.window.request_redraw(); return; }
                };
                let view = frame.texture.create_view(&Default::default());
                let mut enc = s.device.create_command_encoder(&Default::default());
                {
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: None, color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view, depth_slice: None, resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.003, g: 0.003, b: 0.004, a: 1.0 }), store: wgpu::StoreOp::Store } })],
                        depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
                    s.tr.render(&s.atlas, &s.vp, &mut pass).unwrap();
                }
                s.queue.submit(Some(enc.finish()));
                s.queue.present(frame);
                s.atlas.trim();
            }
            _ => {}
        }
    }
}
fn main() { EventLoop::new().unwrap().run_app(&mut A(None)).unwrap(); }
