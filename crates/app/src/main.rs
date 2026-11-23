//! mi-terminal: ventana winit + wgpu (Metal) + glyphon sobre term-core.
use glyphon::*;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use term_core::alacritty_terminal::grid::{Dimensions, Scroll};
use term_core::alacritty_terminal::index::{Column, Line, Point, Side};
use term_core::alacritty_terminal::selection::{Selection, SelectionType};
use term_core::alacritty_terminal::term::cell::Flags;
use term_core::alacritty_terminal::term::TermMode;
use term_core::alacritty_terminal::vte::ansi::{Color as AColor, CursorShape};
use term_core::{dim, key_bytes, to_color, Key as K, Rgb, Session, BG};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

const FONT_SIZE: f32 = 13.0;
const PAD: f32 = 8.0;
const CHIP: &str = "Jump to bottom (click) ↓";
const SHADER: &str = "
struct VO { @builtin(position) p: vec4<f32>, @location(0) c: vec4<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32, @location(0) r: vec4<f32>, @location(1) c: vec4<f32>) -> VO {
  let q = vec2<f32>(f32(i & 1u), f32(i >> 1u));
  return VO(vec4<f32>(r.xy + q * r.zw, 0.0, 1.0), c);
}
@fragment fn fs(v: VO) -> @location(0) vec4<f32> { return v.c; }";

enum Ev { Out, Exit }

/// Píxel -> (columna, fila) de la grilla, acotado a sus límites.
fn px_to_cell(x: f32, y: f32, cell: (f32, f32), pad: f32, cols: usize, rows: usize) -> (usize, usize) {
    let c = (((x - pad) / cell.0).max(0.0) as usize).min(cols - 1);
    let r = (((y - pad) / cell.1).max(0.0) as usize).min(rows - 1);
    (c, r)
}

struct Gfx {
    window: Arc<Window>, surface: wgpu::Surface<'static>, cfg: wgpu::SurfaceConfiguration,
    device: wgpu::Device, queue: wgpu::Queue, fs: FontSystem, sc: SwashCache, vp: Viewport,
    atlas: TextAtlas, tr: TextRenderer, tr2: TextRenderer, pipe: wgpu::RenderPipeline,
    ibuf: wgpu::Buffer, icap: usize, bufs: Vec<Buffer>, chip: Buffer,
    scale: f32, cell: (f32, f32), cols: usize, rows: usize, chip_rect: Option<[f32; 4]>,
}

impl Gfx {
    fn metrics(&self) -> Metrics { Metrics::new(FONT_SIZE * self.scale, (FONT_SIZE * self.scale * 1.2).round()) }
    fn new_buf(&mut self) -> Buffer {
        let m = self.metrics();
        let mut b = Buffer::new(&mut self.fs, m);
        b.set_size(Some(self.cfg.width as f32), Some(self.cell.1));
        b.set_wrap(Wrap::None);
        b
    }
    /// Recalcula tamaño de celda y grilla; devuelve (cols, rows).
    fn relayout(&mut self) -> (usize, usize) {
        self.scale = self.window.scale_factor() as f32;
        let m = self.metrics();
        let mut probe = Buffer::new(&mut self.fs, m);
        probe.set_size(Some(1000.0), Some(m.line_height));
        probe.set_text("M", &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
        probe.shape_until_scroll(&mut self.fs, false);
        let w = probe.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(m.font_size * 0.6);
        self.cell = (w, m.line_height);
        let pad = PAD * self.scale;
        self.cols = (((self.cfg.width as f32 - 2.0 * pad) / w) as usize).max(2);
        self.rows = (((self.cfg.height as f32 - 2.0 * pad) / m.line_height) as usize).max(2);
        let n = self.rows;
        self.bufs.clear();
        for _ in 0..n { let b = self.new_buf(); self.bufs.push(b); }
        (self.cols, self.rows)
    }
}

type Inst = [f32; 8];
fn rect(out: &mut Vec<Inst>, sz: (f32, f32), x: f32, y: f32, w: f32, h: f32, c: Rgb) {
    let (nx, ny) = (x / sz.0 * 2.0 - 1.0, 1.0 - y / sz.1 * 2.0);
    out.push([nx, ny, w / sz.0 * 2.0, -h / sz.1 * 2.0, c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]);
}

fn draw(g: &mut Gfx, s: &Session) {
    let sz = (g.cfg.width as f32, g.cfg.height as f32);
    let (cw, ch) = g.cell;
    let pad = PAD * g.scale;
    let mut grid: Vec<Vec<(usize, usize, char, Rgb, Rgb, bool)>> = vec![vec![]; g.rows];
    let off;
    {
        let term = s.term.lock().unwrap();
        let content = term.renderable_content();
        off = content.display_offset;
        let cursor = (content.cursor.shape != CursorShape::Hidden && off == 0).then_some(content.cursor.point);
        let sel = content.selection;
        for it in content.display_iter {
            let line = it.point.line.0 + off as i32;
            let c = &it.cell;
            if line < 0 || line as usize >= g.rows || c.flags.contains(Flags::WIDE_CHAR_SPACER) { continue; }
            let (mut fg, mut bg) = (to_color(c.fg, true), to_color(c.bg, false));
            if let (true, AColor::Named(n)) = (c.flags.contains(Flags::BOLD), c.fg) {
                if (n as usize) < 8 { fg = to_color(AColor::Indexed(n as u8 + 8), true); }
            }
            if c.flags.contains(Flags::DIM) { fg = dim(fg); }
            let selected = sel.is_some_and(|s| s.contains(it.point));
            if c.flags.contains(Flags::INVERSE) ^ selected ^ (cursor == Some(it.point)) {
                std::mem::swap(&mut fg, &mut bg);
                if fg == bg { fg = BG; }
            }
            let ch = if c.flags.contains(Flags::HIDDEN) { ' ' } else { c.c };
            let w = if c.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            grid[line as usize].push((it.point.column.0, w, ch, fg, bg, c.flags.contains(Flags::BOLD)));
        }
    }

    // Pasada 1: fondos fusionados por tramo. Texto: tramos por (color, negrita).
    let mut rects: Vec<Inst> = vec![];
    for (r, row) in grid.iter().enumerate() {
        let y = pad + r as f32 * ch;
        let mut i = 0;
        while i < row.len() {
            let bg = row[i].4;
            let mut j = i;
            while j < row.len() && row[j].4 == bg { j += 1; }
            if bg != BG {
                let (a, z) = (row[i].0, row[j - 1].0 + row[j - 1].1);
                rect(&mut rects, sz, pad + a as f32 * cw, y, (z - a) as f32 * cw, ch, bg);
            }
            i = j;
        }
        let mut spans: Vec<(String, Rgb, bool)> = vec![];
        for c in row {
            match spans.last_mut() {
                Some(l) if l.1 == c.3 && l.2 == c.5 => l.0.push(c.2),
                _ => spans.push((c.2.to_string(), c.3, c.5)),
            }
        }
        let base = Attrs::new().family(Family::Name("Menlo"));
        let it = spans.iter().map(|(t, f, b)| {
            (t.as_str(), base.clone().color(Color::rgb(f[0], f[1], f[2])).weight(if *b { Weight::BOLD } else { Weight::NORMAL }))
        });
        g.bufs[r].set_rich_text(it, &base, Shaping::Advanced, None);
        g.bufs[r].shape_until_scroll(&mut g.fs, false);
    }
    let n_bg = rects.len();

    // Chip "Jump to bottom" (pasada 2 de rects + segundo TextRenderer, encima del texto).
    g.chip_rect = None;
    if off > 0 {
        let w = (CHIP.chars().count() + 2) as f32 * cw;
        let (h, x, y) = (ch * 1.5, sz.0 / 2.0 - w / 2.0, sz.1 - pad - ch * 3.0);
        rect(&mut rects, sz, x, y, w, h, [0x2a, 0x2a, 0x32]);
        g.chip_rect = Some([x, y, w, h]);
        let m = g.metrics();
        let mut b = Buffer::new(&mut g.fs, m);
        b.set_size(Some(w), Some(ch));
        b.set_text(CHIP, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
        b.shape_until_scroll(&mut g.fs, false);
        g.chip = b;
    }
    if rects.len() > g.icap {
        g.icap = rects.len().next_power_of_two();
        g.ibuf = g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (g.icap * 32) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    }
    if !rects.is_empty() { g.queue.write_buffer(&g.ibuf, 0, rects_bytes(&rects)); }

    g.vp.update(&g.queue, Resolution { width: g.cfg.width, height: g.cfg.height });
    let bounds = TextBounds { left: 0, top: 0, right: g.cfg.width as i32, bottom: g.cfg.height as i32 };
    let areas = g.bufs.iter().enumerate().map(|(r, b)| TextArea {
        buffer: b, left: pad, top: pad + r as f32 * ch, scale: 1.0, bounds,
        default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] });
    g.tr.prepare(&g.device, &g.queue, &mut g.fs, &mut g.atlas, &g.vp, areas, &mut g.sc).unwrap();
    if let Some([x, y, w, _]) = g.chip_rect {
        let _ = w;
        g.tr2.prepare(&g.device, &g.queue, &mut g.fs, &mut g.atlas, &g.vp, [TextArea {
            buffer: &g.chip, left: x + cw, top: y + ch * 0.25, scale: 1.0, bounds,
            default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] }], &mut g.sc).unwrap();
    }

    let frame = match g.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) => f,
        _ => { g.surface.configure(&g.device, &g.cfg); g.window.request_redraw(); return; }
    };
    let view = frame.texture.create_view(&Default::default());
    let mut enc = g.device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None, color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view, depth_slice: None, resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: BG[0] as f64 / 255.0, g: BG[1] as f64 / 255.0, b: BG[2] as f64 / 255.0, a: 1.0 }), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None, timestamp_writes: None, occlusion_query_set: None, multiview_mask: None });
        pass.set_pipeline(&g.pipe);
        pass.set_vertex_buffer(0, g.ibuf.slice(..));
        pass.draw(0..4, 0..n_bg as u32);
        g.tr.render(&g.atlas, &g.vp, &mut pass).unwrap();
        if g.chip_rect.is_some() {
            pass.set_pipeline(&g.pipe);
            pass.set_vertex_buffer(0, g.ibuf.slice(..));
            pass.draw(0..4, n_bg as u32..rects.len() as u32);
            g.tr2.render(&g.atlas, &g.vp, &mut pass).unwrap();
        }
    }
    g.queue.submit(Some(enc.finish()));
    g.queue.present(frame);
    g.atlas.trim();
}

fn rects_bytes(r: &[Inst]) -> &[u8] {
    // Inst es [f32; 8] sin relleno: reinterpretar como bytes es seguro.
    unsafe { std::slice::from_raw_parts(r.as_ptr() as *const u8, std::mem::size_of_val(r)) }
}

fn init_gfx(el: &ActiveEventLoop) -> Gfx {
    let window = Arc::new(el.create_window(Window::default_attributes()
        .with_inner_size(LogicalSize::new(960.0, 600.0)).with_title("mi-terminal")).unwrap());
    let sz = window.inner_size();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle())));
    let surface = instance.create_surface(window.clone()).unwrap();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance, compatible_surface: Some(&surface), ..Default::default() })).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
    // Formato sin sRGB: los colores del tema se usan tal cual en rects y texto (ColorMode::Web).
    let format = wgpu::TextureFormat::Bgra8Unorm;
    let cfg = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT, format, width: sz.width.max(1), height: sz.height.max(1),
        present_mode: wgpu::PresentMode::Fifo, alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![], desired_maximum_frame_latency: 2, color_space: wgpu::SurfaceColorSpace::Auto };
    surface.configure(&device, &cfg);

    let mut db = fontdb::Database::new();
    for p in ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Apple Symbols.ttf", "/System/Library/Fonts/Supplemental/STIXTwoMath.otf"] { let _ = db.load_font_file(p); }
    let fs = FontSystem::new_with_locale_and_db("en-US".into(), db);
    let cache = Cache::new(&device);
    let vp = Viewport::new(&device, &cache);
    let mut atlas = TextAtlas::with_color_mode(&device, &queue, &cache, format, ColorMode::Web);
    let tr = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
    let tr2 = TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[], immediate_size: 0 });
    let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: None, layout: Some(&layout),
        vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout { array_stride: 32, step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4] })] },
        primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
        depth_stencil: None, multisample: Default::default(),
        fragment: Some(wgpu::FragmentState { module: &shader, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(format.into())] }),
        multiview_mask: None, cache: None });
    let ibuf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 32, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let mut g = Gfx {
        window, surface, cfg, device, queue, fs, sc: SwashCache::new(), vp, atlas, tr, tr2, pipe, ibuf, icap: 1,
        bufs: vec![], chip: Buffer::new_empty(Metrics::new(1.0, 1.0)),
        scale: 1.0, cell: (8.0, 16.0), cols: 80, rows: 24, chip_rect: None };
    g.relayout();
    g
}

struct App {
    g: Option<Gfx>, session: Session, mods: ModifiersState,
    mouse: (f32, f32), anchor: Option<Point>, scroll_acc: f32,
}

impl App {
    fn sync_size(&mut self) {
        let Some(g) = self.g.as_mut() else { return };
        let (c, r) = g.relayout();
        let cur = { let t = self.session.term.lock().unwrap(); (t.columns(), t.screen_lines()) };
        if (c, r) != cur {
            self.session.resize(c, r);
        }
    }
    fn point(&self, off: usize) -> Point {
        let g = self.g.as_ref().unwrap();
        let (c, r) = px_to_cell(self.mouse.0, self.mouse.1, g.cell, PAD * g.scale, g.cols, g.rows);
        Point::new(Line(r as i32 - off as i32), Column(c))
    }
}

impl winit::application::ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.g.is_none() { self.g = Some(init_gfx(el)); self.sync_size(); self.g.as_ref().unwrap().window.request_redraw(); }
    }
    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        match ev {
            Ev::Out => if let Some(g) = &self.g { g.window.request_redraw() },
            Ev::Exit => el.exit(),
        }
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        if self.g.is_none() { return; }
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(z) => {
                let g = self.g.as_mut().unwrap();
                g.cfg.width = z.width.max(1);
                g.cfg.height = z.height.max(1);
                g.surface.configure(&g.device, &g.cfg);
                self.sync_size();
                self.g.as_ref().unwrap().window.request_redraw();
            }
            WindowEvent::RedrawRequested => draw(self.g.as_mut().unwrap(), &self.session),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let (cmd, ctrl) = (self.mods.super_key(), self.mods.control_key());
                if cmd {
                    match event.logical_key.as_ref() {
                        Key::Character("c") => {
                            if let Some(s) = self.session.term.lock().unwrap().selection_to_string() { pbcopy(&s); }
                        }
                        Key::Character("v") => {
                            let t = pbpaste();
                            if self.session.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE) {
                                self.session.send(format!("\x1b[200~{t}\x1b[201~").as_bytes());
                            } else {
                                self.session.send(t.as_bytes());
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                let key = match &event.logical_key {
                    Key::Named(NamedKey::Enter) => Some(K::Enter),
                    Key::Named(NamedKey::Backspace) => Some(K::Backspace),
                    Key::Named(NamedKey::Tab) => Some(K::Tab),
                    Key::Named(NamedKey::Escape) => Some(K::Escape),
                    Key::Named(NamedKey::ArrowUp) => Some(K::ArrowUp),
                    Key::Named(NamedKey::ArrowDown) => Some(K::ArrowDown),
                    Key::Named(NamedKey::ArrowLeft) => Some(K::ArrowLeft),
                    Key::Named(NamedKey::ArrowRight) => Some(K::ArrowRight),
                    Key::Named(NamedKey::Home) => Some(K::Home),
                    Key::Named(NamedKey::End) => Some(K::End),
                    Key::Named(NamedKey::Delete) => Some(K::Delete),
                    Key::Named(NamedKey::PageUp) => Some(K::PageUp),
                    Key::Named(NamedKey::PageDown) => Some(K::PageDown),
                    Key::Character(s) if ctrl => s.chars().next().map(K::Char),
                    _ => None,
                };
                let app_cursor = self.session.term.lock().unwrap().mode().contains(TermMode::APP_CURSOR);
                if let Some(b) = key.and_then(|k| key_bytes(k, ctrl, app_cursor)) {
                    self.session.send(&b);
                } else if !ctrl {
                    if let Some(t) = &event.text { self.session.send(t.as_bytes()); }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let ch = self.g.as_ref().unwrap().cell.1;
                self.scroll_acc += match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / ch,
                };
                let lines = self.scroll_acc as i32;
                self.scroll_acc -= lines as f32;
                if lines != 0 {
                    self.session.term.lock().unwrap().scroll_display(Scroll::Delta(lines));
                    self.g.as_ref().unwrap().window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse = (position.x as f32, position.y as f32);
                if let Some(a) = self.anchor {
                    let mut t = self.session.term.lock().unwrap();
                    let p = self.point(t.grid().display_offset());
                    match t.selection.as_mut() {
                        Some(s) => s.update(p, Side::Right),
                        None => t.selection = Some(Selection::new(SelectionType::Simple, a, Side::Left)),
                    }
                    drop(t);
                    self.g.as_ref().unwrap().window.request_redraw();
                }
            }
            WindowEvent::MouseInput { button: MouseButton::Left, state, .. } => {
                let g = self.g.as_ref().unwrap();
                if state == ElementState::Released { self.anchor = None; return; }
                let (mx, my) = self.mouse;
                if g.chip_rect.is_some_and(|[x, y, w, h]| mx >= x && mx <= x + w && my >= y && my <= y + h) {
                    self.session.term.lock().unwrap().scroll_display(Scroll::Bottom);
                } else {
                    let mut t = self.session.term.lock().unwrap();
                    t.selection = None;
                    self.anchor = Some(self.point(t.grid().display_offset()));
                }
                g.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn pbcopy(s: &str) {
    if let Ok(mut c) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut i) = c.stdin.take() { let _ = i.write_all(s.as_bytes()); }
        let _ = c.wait();
    }
}

fn pbpaste() -> String {
    Command::new("pbpaste").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn main() {
    let el = EventLoop::<Ev>::with_user_event().build().unwrap();
    el.set_control_flow(ControlFlow::Wait);
    let (p1, p2) = (el.create_proxy(), el.create_proxy());
    let session = Session::spawn(None, move || { let _ = p1.send_event(Ev::Out); }, move || { let _ = p2.send_event(Ev::Exit); });
    // Solo para pruebas: escribe este comando al PTY al inicio.
    if let Ok(c) = std::env::var("MI_TERMINAL_CMD") { session.send(format!("{c}\n").as_bytes()); }
    let mut app = App { g: None, session, mods: ModifiersState::empty(), mouse: (0.0, 0.0), anchor: None, scroll_acc: 0.0 };
    el.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pixel_a_celda() {
        assert_eq!(px_to_cell(8.0 + 25.0, 8.0 + 35.0, (10.0, 20.0), 8.0, 80, 24), (2, 1));
        assert_eq!(px_to_cell(-5.0, 9999.0, (10.0, 20.0), 8.0, 80, 24), (0, 23));
    }
}
