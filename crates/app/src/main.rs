//! mi-terminal: ventana winit + wgpu (Metal) + glyphon sobre term-core.
mod sidebar;

use glyphon::*;
use sidebar as sb;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use term_core::alacritty_terminal::grid::{Dimensions, Scroll};
use term_core::alacritty_terminal::index::{Column, Line, Point, Side};
use term_core::alacritty_terminal::selection::{Selection, SelectionType};
use term_core::alacritty_terminal::term::cell::Flags;
use term_core::alacritty_terminal::term::TermMode;
use term_core::alacritty_terminal::vte::ansi::{Color as AColor, CursorShape};
use term_core::{click_move_bytes, dim, key_bytes, FG, paste_bytes, to_color, Key as K, Mods, Rgb, Session, BG};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::macos::WindowAttributesExtMacOS;
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

const FONT_SIZE: f32 = 13.0;
const PAD: f32 = 8.0;
/// Franja reservada arriba para la barra de pestañas (parte 2), en px lógicos.
const TABBAR_H: f32 = 36.0;
const TABBAR_BG: Rgb = [0x08, 0x08, 0x0a];
const TAB_ACTIVE: Rgb = [0x16, 0x16, 0x1a];
const TAB_HOVER: Rgb = [0x10, 0x10, 0x13];
const SEP_W: f32 = 1.0;
const SEP_COLOR: Rgb = [0x26, 0x26, 0x2c];
const SIDE_BG: Rgb = [0x09, 0x09, 0x0b];
const BTN_BG: Rgb = [0x16, 0x16, 0x1a];
const TITLE_CH: usize = 24;
const CHIP: &str = "Jump to bottom (click) ↓";
const SHADER: &str = "
struct VO { @builtin(position) p: vec4<f32>, @location(0) c: vec4<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32, @location(0) r: vec4<f32>, @location(1) c: vec4<f32>) -> VO {
  let q = vec2<f32>(f32(i & 1u), f32(i >> 1u));
  return VO(vec4<f32>(r.xy + q * r.zw, 0.0, 1.0), c);
}
@fragment fn fs(v: VO) -> @location(0) vec4<f32> { return v.c; }";

enum Ev { Out(u64), Exit(u64), Tick }

/// Rectángulo en píxeles: (x, y, ancho, alto).
type R = (f32, f32, f32, f32);

/// Reparte `area` entre 1 o 2 paneles lado a lado con un separador de `sep` px; `split` es la fracción del primero.
fn pane_rects(area: R, n: usize, split: f32, sep: f32) -> Vec<R> {
    let (x, y, w, h) = area;
    if n < 2 { return vec![area]; }
    let w1 = ((w - sep) * split.clamp(0.1, 0.9)).round();
    vec![(x, y, w1, h), (x + w1 + sep, y, w - sep - w1, h)]
}

/// Tamaño (cols, filas) del PTY para un rect con relleno `pad` por lado.
fn pty_size(r: R, cell: (f32, f32), pad: f32) -> (usize, usize) {
    ((((r.2 - 2.0 * pad) / cell.0) as usize).max(2), (((r.3 - 2.0 * pad) / cell.1) as usize).max(2))
}

/// Píxel -> (columna, fila) de la grilla, acotado a sus límites.
fn px_to_cell(x: f32, y: f32, cell: (f32, f32), pad: f32, cols: usize, rows: usize) -> (usize, usize) {
    let c = (((x - pad) / cell.0).max(0.0) as usize).min(cols - 1);
    let r = (((y - pad) / cell.1).max(0.0) as usize).min(rows - 1);
    (c, r)
}

/// Píxel de ventana -> (columna, fila) del panel `rect` (resta su origen: pestañas y paneles previos), acotado.
fn mouse_cell(mouse: (f32, f32), rect: R, cell: (f32, f32), pad: f32, cols: usize, rows: usize) -> (usize, usize) {
    px_to_cell(mouse.0 - rect.0, mouse.1 - rect.1, cell, pad, cols, rows)
}

/// Evento de ratón para apps con modo de ratón (celda 0-based): SGR si `sgr`, si no X10 (límite 223).
fn mouse_report(press: bool, col: usize, row: usize, sgr: bool) -> Vec<u8> {
    if sgr { return format!("\x1b[<0;{};{}{}", col + 1, row + 1, if press { 'M' } else { 'm' }).into_bytes(); }
    vec![0x1b, b'[', b'M', if press { 32 } else { 35 }, (33 + col.min(222)) as u8, (33 + row.min(222)) as u8]
}

/// Qué hay bajo el ratón en la barra de pestañas.
#[derive(Debug, PartialEq, Clone, Copy)]
enum BarHit { Tab(usize), Close(usize), Plus }

const BAR_LEFT: f32 = 80.0; // semáforos
const BAR_RIGHT: f32 = 70.0; // marcadores
const TAB_MAX_W: f32 = 200.0;
const PLUS_W: f32 = 30.0;
const CLOSE_S: f32 = 16.0;

/// Inicio (px) de las pestañas: tras la barra lateral si se ve; si no, tras semáforos y botón de la barra.
fn bar_left(side: bool, sc: f32) -> f32 { (if side { sb::SIDE_W } else { BAR_LEFT + 32.0 }) * sc }

/// Rects (px) de las `n` pestañas y del botón `+`, desde `x0`, para una ventana de `width` px y escala `sc`.
fn bar_rects(n: usize, width: f32, sc: f32, x0: f32) -> (Vec<R>, R) {
    let h = TABBAR_H * sc;
    let tw = ((width - x0 - BAR_RIGHT * sc - PLUS_W * sc) / n.max(1) as f32).clamp(40.0 * sc, TAB_MAX_W * sc);
    ((0..n).map(|i| (x0 + i as f32 * tw, 0.0, tw, h)).collect(), (x0 + n as f32 * tw, 0.0, PLUS_W * sc, h))
}

/// Rect del botón cerrar dentro de una pestaña.
fn close_rect(t: R, sc: f32) -> R { (t.0 + t.2 - (CLOSE_S + 8.0) * sc, (t.3 - CLOSE_S * sc) / 2.0, CLOSE_S * sc, CLOSE_S * sc) }

fn inside(r: R, m: (f32, f32)) -> bool { m.0 >= r.0 && m.0 < r.0 + r.2 && m.1 >= r.1 && m.1 < r.1 + r.3 }

fn bar_hit(m: (f32, f32), n: usize, width: f32, sc: f32, x0: f32) -> Option<BarHit> {
    let (tabs, plus) = bar_rects(n, width, sc, x0);
    if inside(plus, m) { return Some(BarHit::Plus); }
    tabs.iter().position(|&t| inside(t, m)).map(|i| if inside(close_rect(tabs[i], sc), m) { BarHit::Close(i) } else { BarHit::Tab(i) })
}

fn debug_on() -> bool { std::env::var("MI_TERMINAL_DEBUG").is_ok_and(|v| v == "1") }

type Span = (String, Rgb, bool);

struct Gfx {
    window: Arc<Window>, surface: wgpu::Surface<'static>, cfg: wgpu::SurfaceConfiguration,
    device: wgpu::Device, queue: wgpu::Queue, fs: FontSystem, sc: SwashCache, vp: Viewport,
    atlas: TextAtlas, tr: TextRenderer, tr2: TextRenderer, pipe: wgpu::RenderPipeline,
    ibuf: wgpu::Buffer, icap: usize, chip: Buffer, icons: Vec<Buffer>, side: Vec<(String, Buffer)>, scale: f32, cell: (f32, f32),
}

struct Pane {
    id: u64, session: Session, anchor: Option<Point>, bufs: Vec<Buffer>, prev: Vec<Option<Vec<Span>>>,
    chip_rect: Option<[f32; 4]>, cols: usize, rows: usize, rect: R,
    born: Instant, last_out: Instant, unread: bool, agent: sb::Agent, had_agent: bool, project: String,
}

impl Pane {
    fn has(&self, x: f32, y: f32) -> bool {
        x >= self.rect.0 && x < self.rect.0 + self.rect.2 && y >= self.rect.1 && y < self.rect.1 + self.rect.3
    }
    /// Ajusta el panel a su rect: rehace buffers de fila y redimensiona el PTY solo si cambió algo.
    fn fit(&mut self, g: &mut Gfx, rect: R, force: bool) {
        self.rect = rect;
        let (c, r) = pty_size(rect, g.cell, PAD * g.scale);
        if force || (c, r) != (self.cols, self.rows) {
            (self.cols, self.rows) = (c, r);
            self.bufs = (0..r).map(|_| g.new_buf()).collect();
            self.prev = vec![None; r];
        }
        let cur = { let t = self.session.term.lock().unwrap(); (t.columns(), t.screen_lines()) };
        if (c, r) != cur { self.session.resize(c, r); }
    }
    /// Agente en primer plano y proyecto (cwd) vía libproc.
    fn refresh(&mut self) {
        let Some(pid) = self.session.fg_pid().or(self.session.child_pid.map(|p| p as i32)) else { return };
        let (name, path, cwd) = sb::proc_info(pid);
        self.agent = sb::agent_from(&name, &path);
        self.had_agent |= self.agent != sb::Agent::Shell;
        if !cwd.is_empty() { self.project = sb::project_of(&cwd, &std::env::var("HOME").unwrap_or_default()); }
    }
    fn point(&self, mouse: (f32, f32), cell: (f32, f32), pad: f32, off: usize) -> Point {
        let (c, r) = mouse_cell(mouse, self.rect, cell, pad, self.cols, self.rows);
        Point::new(Line(r as i32 - off as i32), Column(c))
    }
}

struct Tab { panes: Vec<Pane>, active: usize, split: f32, tbuf: Buffer, tprev: String }

impl Tab {
    fn title(&self) -> String {
        let t = self.panes[self.active].session.title.lock().unwrap().clone();
        if t.is_empty() { "zsh".into() } else { t }
    }
}

impl Gfx {
    fn metrics(&self) -> Metrics { Metrics::new(FONT_SIZE * self.scale, (FONT_SIZE * self.scale * 1.2).round()) }
    fn bar_metrics(&self) -> Metrics { Metrics::new(11.0 * self.scale, (14.0 * self.scale).round()) }
    fn new_buf(&mut self) -> Buffer {
        let m = self.metrics();
        let mut b = Buffer::new(&mut self.fs, m);
        b.set_size(Some(self.cfg.width as f32), Some(self.cell.1));
        b.set_wrap(Wrap::None);
        b
    }
    /// Recalcula el tamaño de celda y el chip; los paneles se reajustan con `Pane::fit`.
    fn relayout(&mut self) {
        self.scale = self.window.scale_factor() as f32;
        let m = self.metrics();
        let mut probe = Buffer::new(&mut self.fs, m);
        probe.set_size(Some(1000.0), Some(m.line_height));
        probe.set_text("M", &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
        probe.shape_until_scroll(&mut self.fs, false);
        let w = probe.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(m.font_size * 0.6);
        self.cell = (w, m.line_height);
        // Chip "Jump to bottom": se crea una vez por relayout, no por frame.
        let mut b = Buffer::new(&mut self.fs, m);
        b.set_size(Some((CHIP.chars().count() + 2) as f32 * w), Some(self.cell.1));
        b.set_text(CHIP, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fs, false);
        self.chip = b;
        // Glifos sueltos de la barra de pestañas: ícono, +, x, pulso y globo.
        let bm = self.bar_metrics();
        self.icons = ["✻", "+", "×", "∿", "◎", "◧"].iter().map(|t| {
            let mut b = Buffer::new(&mut self.fs, bm);
            b.set_size(Some(40.0 * self.scale), Some(bm.line_height));
            b.set_text(t, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
            b.shape_until_scroll(&mut self.fs, false);
            b
        }).collect();
    }
}

/// Una fila por panel de cada pestaña, con su estado actual.
fn entries(tabs: &[Tab], active_tab: usize) -> Vec<sb::Entry> {
    let now = Instant::now();
    tabs.iter().enumerate().flat_map(|(ti, t)| t.panes.iter().enumerate().map(move |(pi, p)| {
        let title = p.session.title.lock().unwrap().clone();
        sb::Entry {
            tab: ti, pane: pi, project: p.project.clone(), agent: p.agent, unread: p.unread, focused: ti == active_tab && pi == t.active,
            status: sb::status(p.agent, p.had_agent, now.duration_since(p.last_out), &title),
            title: sb::clean_title(if title.is_empty() { "zsh" } else { &title }, TITLE_CH),
            secs: now.duration_since(p.born).as_secs(),
        }
    })).collect()
}

type Inst = [f32; 8];
fn rect(out: &mut Vec<Inst>, sz: (f32, f32), x: f32, y: f32, w: f32, h: f32, c: Rgb) {
    let (nx, ny) = (x / sz.0 * 2.0 - 1.0, 1.0 - y / sz.1 * 2.0);
    out.push([nx, ny, w / sz.0 * 2.0, -h / sz.1 * 2.0, c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]);
}

fn draw(g: &mut Gfx, tabs: &mut [Tab], active_tab: usize, mouse: (f32, f32), side: &sb::Side) {
    let bm = g.bar_metrics();
    for t in tabs.iter_mut() {
        let title = t.title();
        if t.tprev != title {
            t.tbuf.set_metrics(bm);
            t.tbuf.set_size(Some(TAB_MAX_W * g.scale), Some(bm.line_height));
            t.tbuf.set_wrap(Wrap::None);
            t.tbuf.set_text(&title, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
            t.tbuf.shape_until_scroll(&mut g.fs, false);
            t.tprev = title;
        }
    }
    let tabs_len = tabs.len();
    let ents_all = entries(tabs, active_tab);
    let tab = &mut tabs[active_tab];
    let sz = (g.cfg.width as f32, g.cfg.height as f32);
    let (cw, ch) = g.cell;
    let pad = PAD * g.scale;
    let mut rects: Vec<Inst> = vec![];
    rect(&mut rects, sz, 0.0, 0.0, sz.0, TABBAR_H * g.scale, TABBAR_BG);
    let left = bar_left(side.visible, g.scale);
    let (trs, plus) = bar_rects(tabs_len, sz.0, g.scale, left);
    let hover = bar_hit(mouse, tabs_len, sz.0, g.scale, left);
    // Barra lateral: rects y textos (los buffers se reutilizan por índice, solo lo visible).
    let mut stxt: Vec<(String, f32, f32, Color, Option<(u16, f32)>)> = vec![];
    if side.visible {
        let sc = g.scale;
        let (all, wt, wk) = sb::counts(&ents_all);
        let ents = sb::filter(ents_all, side.filter, &side.query);
        let items = sb::group(&ents);
        let (dimc, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
        rect(&mut rects, sz, 0.0, 0.0, sb::SIDE_W * sc, sz.1, SIDE_BG);
        rect(&mut rects, sz, sb::SIDE_W * sc - 1.0, 0.0, 1.0, sz.1, SEP_COLOR);
        for (r, hit) in sb::layout(sz, sc, &items) {
            let (x, y, w, h) = r;
            let ty = y + (h - bm.line_height) / 2.0;
            // `gl` marca el próximo `put` como logo de marca (id, lado en px); el texto sale vacío.
            let gl = std::cell::Cell::new(None);
            let mut put = |t: String, dx: f32, dy: f32, c: Color| stxt.push((t, x + dx * sc, ty + dy * sc, c, gl.take()));
            let logo = |a: sb::Agent, s: f32| { let ic = a.icon(); gl.set(ic.map(|(id, _)| (id, s * sc))); if ic.is_some() { String::new() } else { a.glyph().into() } };
            match hit {
                sb::Hit::New => { rect(&mut rects, sz, x, y, w, h, BTN_BG); put("✎".into(), 10.0, 0.0, lit); put("New session".into(), 30.0, 0.0, lit); put("⇧⌘T".into(), (w - 44.0 * sc) / sc, 0.0, dimc); }
                sb::Hit::Folder => { rect(&mut rects, sz, x, y, w, h, BTN_BG); put("▭".into(), 8.0, 0.0, dimc); }
                sb::Hit::Search => {
                    let t = if side.searching { format!("{}▏", side.query) } else if side.query.is_empty() { "Search agents".into() } else { side.query.clone() };
                    put("⌕".into(), 10.0, 0.0, dimc); put(t, 30.0, 0.0, if side.query.is_empty() && !side.searching { dimc } else { lit });
                    put("/".into(), (w - 20.0 * sc) / sc, 0.0, dimc);
                }
                sb::Hit::Filter(f) => {
                    if side.filter == f { rect(&mut rects, sz, x + 2.0 * sc, y, w - 4.0 * sc, h, [0x24, 0x24, 0x2a]); }
                    let t = match f { sb::Filter::All => format!("All {all}"), sb::Filter::Waiting if wt > 0 => format!("Waiting {wt}"), sb::Filter::Waiting => "Waiting".into(), sb::Filter::Working => format!("Working {wk}") };
                    put(t, 12.0, 0.0, if side.filter == f { lit } else { dimc });
                }
                sb::Hit::Group(i) => if let sb::Item::Group(p) = &items[i] { put(p.clone(), 14.0, 0.0, dimc); },
                sb::Hit::Row(j) => {
                    let e = &ents[j];
                    if e.focused { rect(&mut rects, sz, x + 6.0 * sc, y, w - 12.0 * sc, h, [0x1a, 0x1a, 0x20]); }
                    let c = e.agent.color();
                    let st = match e.status { sb::Status::Done => Color::rgb(0x5a, 0xc0, 0x6a), _ => dimc };
                    let t = logo(e.agent, 18.0); put(t, 16.0, -9.0 + (bm.line_height / sc - 18.0) / 2.0, Color::rgb(c[0], c[1], c[2]));
                    if e.unread { put("●".into(), 8.0, -12.0, Color::rgb(0x5a, 0xc0, 0x6a)); }
                    put(e.title.clone(), 38.0, -9.0, lit);
                    put(sb::fmt_elapsed(e.secs), (w - 40.0 * sc) / sc, -9.0, dimc);
                    put(e.status.label().into(), 38.0, 7.0, st);
                }
                sb::Hit::Launch(i) => {
                    let a = sb::LAUNCH[i].0;
                    let c = if side.installed[i] { a.color() } else { [0x3a, 0x3a, 0x42] };
                    let t = logo(a, 18.0); put(t, 6.0, (bm.line_height / sc - 18.0) / 2.0, Color::rgb(c[0], c[1], c[2]));
                }
                sb::Hit::Archived => { put("▤".into(), 10.0, 0.0, dimc); put("Archived".into(), 30.0, 0.0, dimc); put(side.archived.len().to_string(), (w - 24.0 * sc) / sc, 0.0, dimc); }
            }
        }
        let bm = g.bar_metrics();
        for (i, (t, ..)) in stxt.iter().enumerate() {
            if i >= g.side.len() { let b = Buffer::new(&mut g.fs, bm); g.side.push((String::new(), b)); }
            let (k, b) = &mut g.side[i];
            if k != t {
                b.set_size(Some(sb::SIDE_W * sc), Some(bm.line_height));
                b.set_wrap(Wrap::None);
                b.set_text(t, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
                b.shape_until_scroll(&mut g.fs, false);
                *k = t.clone();
            }
        }
    }
    for (i, &t) in trs.iter().enumerate() {
        if i == active_tab { rect(&mut rects, sz, t.0, t.1, t.2, t.3, TAB_ACTIVE); }
        else if hover == Some(BarHit::Tab(i)) || hover == Some(BarHit::Close(i)) { rect(&mut rects, sz, t.0, t.1, t.2, t.3, TAB_HOVER); }
        if hover == Some(BarHit::Close(i)) { let c = close_rect(t, g.scale); rect(&mut rects, sz, c.0, c.1, c.2, c.3, [0x34, 0x34, 0x3c]); }
    }
    if hover == Some(BarHit::Plus) { rect(&mut rects, sz, plus.0, plus.1, plus.2, plus.3, TAB_HOVER); }
    if let [a, b] = &tab.panes[..] {
        let x = a.rect.0 + a.rect.2;
        rect(&mut rects, sz, x, a.rect.1, b.rect.0 - x, a.rect.3, SEP_COLOR);
    }
    let act = tab.active;
    let mut chips: Vec<[f32; 4]> = vec![];
    for (pi, p) in tab.panes.iter_mut().enumerate() {
    let (rx, ry, rw, rh) = p.rect;
    let (ox, oy) = (rx + pad, ry + pad);
    let mut grid: Vec<Vec<(usize, usize, char, Rgb, Rgb, bool)>> = vec![vec![]; p.rows];
    let off;
    let mut hollow = None;
    {
        let term = p.session.term.lock().unwrap();
        let content = term.renderable_content();
        off = content.display_offset;
        let cursor = (content.cursor.shape != CursorShape::Hidden && off == 0).then_some(content.cursor.point);
        // Solo el panel activo lleva cursor sólido; en los demás, hueco.
        let solid = pi == act;
        if !solid { hollow = cursor; }
        let sel = content.selection;
        for it in content.display_iter {
            let line = it.point.line.0 + off as i32;
            let c = &it.cell;
            if line < 0 || line as usize >= p.rows || c.flags.contains(Flags::WIDE_CHAR_SPACER) { continue; }
            let (mut fg, mut bg) = (to_color(c.fg, true), to_color(c.bg, false));
            if let (true, AColor::Named(n)) = (c.flags.contains(Flags::BOLD), c.fg) {
                if (n as usize) < 8 { fg = to_color(AColor::Indexed(n as u8 + 8), true); }
            }
            if c.flags.contains(Flags::DIM) { fg = dim(fg); }
            let selected = sel.is_some_and(|s| s.contains(it.point));
            if c.flags.contains(Flags::INVERSE) ^ selected ^ (solid && cursor == Some(it.point)) {
                std::mem::swap(&mut fg, &mut bg);
                if fg == bg { fg = BG; }
            }
            let ch = if c.flags.contains(Flags::HIDDEN) { ' ' } else { c.c };
            let w = if c.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            grid[line as usize].push((it.point.column.0, w, ch, fg, bg, c.flags.contains(Flags::BOLD)));
        }
    }

    // Pasada 1: fondos fusionados por tramo. Texto: tramos por (color, negrita).
    for (r, row) in grid.iter().enumerate() {
        let y = oy + r as f32 * ch;
        let mut i = 0;
        while i < row.len() {
            let bg = row[i].4;
            let mut j = i;
            while j < row.len() && row[j].4 == bg { j += 1; }
            if bg != BG {
                let (a, z) = (row[i].0, row[j - 1].0 + row[j - 1].1);
                rect(&mut rects, sz, ox + a as f32 * cw, y, (z - a) as f32 * cw, ch, bg);
            }
            i = j;
        }
        let mut spans: Vec<Span> = vec![];
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
        // Solo se reforma la fila si su contenido cambió respecto al frame anterior.
        if p.prev[r].as_ref() != Some(&spans) {
            p.bufs[r].set_rich_text(it, &base, Shaping::Advanced, None);
            p.bufs[r].shape_until_scroll(&mut g.fs, false);
            p.prev[r] = Some(spans);
        }
    }
    if let Some(c) = hollow.filter(|c| c.line.0 >= 0 && (c.line.0 as usize) < p.rows) {
        let (x, y, t) = (ox + c.column.0 as f32 * cw, oy + c.line.0 as f32 * ch, g.scale.max(1.0));
        for (a, b, w, h) in [(x, y, cw, t), (x, y + ch - t, cw, t), (x, y, t, ch), (x + cw - t, y, t, ch)] {
            rect(&mut rects, sz, a, b, w, h, FG);
        }
    }
    p.chip_rect = None;
    if off > 0 {
        let w = (CHIP.chars().count() + 2) as f32 * cw;
        p.chip_rect = Some([rx + rw / 2.0 - w / 2.0, ry + rh - pad - ch * 3.0, w, ch * 1.5]);
    }
    }
    let n_bg = rects.len();

    // Chips "Jump to bottom" (pasada 2 de rects + segundo TextRenderer, encima del texto).
    for p in &tab.panes {
        if let Some([x, y, w, h]) = p.chip_rect {
            rect(&mut rects, sz, x, y, w, h, [0x2a, 0x2a, 0x32]);
            chips.push([x, y, w, h]);
        }
    }
    if rects.len() > g.icap {
        g.icap = rects.len().next_power_of_two();
        g.ibuf = g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (g.icap * 32) as u64, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    }
    if !rects.is_empty() { g.queue.write_buffer(&g.ibuf, 0, rects_bytes(&rects)); }

    g.vp.update(&g.queue, Resolution { width: g.cfg.width, height: g.cfg.height });
    // Texto de la barra: ícono + título (+ x en activa/hover) por pestaña, +, y marcadores a la derecha.
    let (tabs, sc) = (&*tabs, g.scale);
    let tab = &tabs[active_tab];
    let full = TextBounds { left: 0, top: 0, right: g.cfg.width as i32, bottom: (TABBAR_H * sc) as i32 };
    let ty = (TABBAR_H * sc - bm.line_height) / 2.0;
    let (dim_c, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
    let mut bar: Vec<TextArea> = vec![];
    let lg = |id, s: f32, dx: f32, dy: f32, c: Color| [CustomGlyph { id, left: dx, top: dy, width: s, height: s, color: Some(c), snap_to_physical_pixel: true, metadata: 0 }];
    let tabg: Vec<[CustomGlyph; 1]> = (0..tabs.len()).map(|_| lg(0, 15.0 * sc, -18.0 * sc, (bm.line_height - 15.0 * sc) / 2.0, Color::rgb(0xd9, 0x77, 0x57))).collect();
    let sideg: Vec<[CustomGlyph; 1]> = stxt.iter().map(|e| match e.4 { Some((id, s)) => lg(id, s, 0.0, 0.0, e.3), None => lg(u16::MAX, 0.0, 0.0, 0.0, e.3) }).collect();
    for (i, (&t, tb)) in trs.iter().zip(tabs).enumerate() {
        let col = if i == active_tab { lit } else { dim_c };
        let b = TextBounds { left: t.0 as i32, top: 0, right: (t.0 + t.2 - (CLOSE_S + 10.0) * sc) as i32, bottom: full.bottom };
        // Logo de Claude de la pestaña, pegado al área del título (a su izquierda).
        bar.push(TextArea { buffer: &tb.tbuf, left: t.0 + 28.0 * sc, top: ty, scale: 1.0, bounds: b, default_color: col, custom_glyphs: &tabg[i] });
        if i == active_tab || matches!(hover, Some(BarHit::Tab(j) | BarHit::Close(j)) if j == i) {
            let c = close_rect(t, sc);
            bar.push(TextArea { buffer: &g.icons[2], left: c.0 + 4.0 * sc, top: ty, scale: 1.0, bounds: full, default_color: lit, custom_glyphs: &[] });
        }
    }
    bar.push(TextArea { buffer: &g.icons[1], left: plus.0 + 10.0 * sc, top: ty, scale: 1.0, bounds: full, default_color: dim_c, custom_glyphs: &[] });
    bar.push(TextArea { buffer: &g.icons[5], left: (BAR_LEFT + 6.0) * sc, top: ty, scale: 1.0, bounds: full, default_color: dim_c, custom_glyphs: &[] });
    let sb_bounds = TextBounds { left: 0, top: 0, right: (sb::SIDE_W * sc) as i32, bottom: g.cfg.height as i32 };
    for (((_, x, y, c, ic), (_, b)), gl) in stxt.iter().zip(&g.side).zip(&sideg) {
        bar.push(TextArea { buffer: b, left: *x, top: *y, scale: 1.0, bounds: sb_bounds, default_color: *c, custom_glyphs: if ic.is_some() { gl } else { &[] } });
    }
    for (k, x) in [(3, 56.0), (4, 28.0)] {
        bar.push(TextArea { buffer: &g.icons[k], left: sz.0 - x * sc, top: ty, scale: 1.0, bounds: full, default_color: dim_c, custom_glyphs: &[] });
    }
    let areas = bar.into_iter().chain(tab.panes.iter().flat_map(|p| {
        let bounds = TextBounds { left: p.rect.0 as i32, top: p.rect.1 as i32, right: (p.rect.0 + p.rect.2) as i32, bottom: (p.rect.1 + p.rect.3) as i32 };
        p.bufs.iter().enumerate().map(move |(r, b)| TextArea {
            buffer: b, left: p.rect.0 + pad, top: p.rect.1 + pad + r as f32 * ch, scale: 1.0, bounds,
            default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] })
    }));
    g.tr.prepare_with_custom(&g.device, &g.queue, &mut g.fs, &mut g.atlas, &g.vp, areas, &mut g.sc,
        |r| sb::raster_icon(r.id, r.width, r.height).map(|data| RasterizedCustomGlyph { data, content_type: ContentType::Mask })).unwrap();
    if !chips.is_empty() {
        let bounds = TextBounds { left: 0, top: 0, right: g.cfg.width as i32, bottom: g.cfg.height as i32 };
        let areas = chips.iter().map(|&[x, y, _, _]| TextArea {
            buffer: &g.chip, left: x + cw, top: y + ch * 0.25, scale: 1.0, bounds,
            default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] });
        g.tr2.prepare(&g.device, &g.queue, &mut g.fs, &mut g.atlas, &g.vp, areas, &mut g.sc).unwrap();
    }

    let frame = match g.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(f) => f,
        wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
        // Superficie cambiada: reconfigurar y reintentar una vez.
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            g.surface.configure(&g.device, &g.cfg); g.window.request_redraw(); return;
        }
        // Occluded/Timeout/Validation: sin redraw; Occluded(false) o el próximo evento lo reanudan.
        _ => return,
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
        if !chips.is_empty() {
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
        .with_inner_size(LogicalSize::new(960.0, 600.0)).with_title("mi-terminal")
        .with_titlebar_transparent(true).with_title_hidden(true).with_fullsize_content_view(true)).unwrap());
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
        view_formats: vec![], desired_maximum_frame_latency: 1, color_space: wgpu::SurfaceColorSpace::Auto };
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
        chip: Buffer::new_empty(Metrics::new(1.0, 1.0)), icons: vec![], side: vec![], scale: 1.0, cell: (8.0, 16.0) };
    g.relayout();
    g
}

struct App {
    g: Option<Gfx>, tabs: Vec<Tab>, active_tab: usize, next_id: u64, proxy: EventLoopProxy<Ev>,
    mods: ModifiersState, mouse: (f32, f32), scroll_acc: f32, drag_sep: bool, mouse_fwd: bool,
    side: sb::Side,
}

impl App {
    fn sep(&self) -> f32 { (SEP_W * self.g.as_ref().map_or(1.0, |g| g.scale)).round().max(1.0) }
    /// Área de paneles: la ventana menos la franja de pestañas.
    fn area(&self) -> R {
        let g = self.g.as_ref().unwrap();
        let (top, left) = (TABBAR_H * g.scale, if self.side.visible { sb::SIDE_W * g.scale } else { 0.0 });
        (left, top, (g.cfg.width as f32 - left).max(1.0), (g.cfg.height as f32 - top).max(1.0))
    }
    fn redraw(&self) { if let Some(g) = &self.g { g.window.request_redraw(); } }
    fn new_pane(&mut self, cmd: Option<Vec<String>>) -> Pane {
        let id = self.next_id;
        self.next_id += 1;
        let (p1, p2) = (self.proxy.clone(), self.proxy.clone());
        let session = Session::spawn(cmd, move || { let _ = p1.send_event(Ev::Out(id)); }, move || { let _ = p2.send_event(Ev::Exit(id)); });
        Pane { id, session, anchor: None, bufs: vec![], prev: vec![], chip_rect: None, cols: 0, rows: 0, rect: (0.0, 0.0, 1.0, 1.0),
            born: Instant::now(), last_out: Instant::now(), unread: false, agent: sb::Agent::Shell, had_agent: false, project: "~".into() }
    }
    fn new_tab(&mut self) { self.new_tab_with(None); }
    fn new_tab_with(&mut self, cmd: Option<Vec<String>>) {
        let p = self.new_pane(cmd);
        self.tabs.push(Tab { panes: vec![p], active: 0, split: 0.5, tbuf: Buffer::new_empty(Metrics::new(1.0, 1.0)), tprev: String::new() });
        self.active_tab = self.tabs.len() - 1;
        self.sync_size(false);
        self.redraw();
    }
    /// Ajusta cada panel de cada pestaña a su rect; `force` rehace buffers (escala o ventana nueva).
    fn sync_size(&mut self, force: bool) {
        if self.g.is_none() || self.tabs.is_empty() { return; }
        let (area, sep) = (self.area(), self.sep());
        let g = self.g.as_mut().unwrap();
        if force { g.relayout(); }
        for t in &mut self.tabs {
            if force { t.tprev.clear(); }
            let rs = pane_rects(area, t.panes.len(), t.split, sep);
            for (p, r) in t.panes.iter_mut().zip(rs) { p.fit(g, r, force); }
        }
    }
    /// Guarda en memoria (con su proyecto y hora) una sesión que se cierra. shortcut: sin persistir en disco, tope 200.
    fn archive(&mut self, ti: usize, pi: usize) {
        let p = &mut self.tabs[ti].panes[pi];
        p.refresh();
        let t = p.session.title.lock().unwrap().clone();
        let title = sb::clean_title(if t.is_empty() { "zsh" } else { &t }, TITLE_CH);
        self.side.archived.push(sb::Archived { title, project: p.project.clone(), at: SystemTime::now() });
        if self.side.archived.len() > 200 { self.side.archived.remove(0); }
    }
    fn toggle_side(&mut self) {
        self.side.visible = !self.side.visible;
        self.side.searching = false;
        if self.side.visible { for t in &mut self.tabs { for p in &mut t.panes { p.refresh(); } } }
        self.sync_size(false);
        self.redraw();
    }
    fn side_click(&mut self) {
        let (g, m) = (self.g.as_ref().unwrap(), self.mouse);
        let (sc, sz) = (g.scale, (g.cfg.width as f32, g.cfg.height as f32));
        if m.1 < TABBAR_H * sc { let _ = g.window.drag_window(); return; }
        let ents = sb::filter(entries(&self.tabs, self.active_tab), self.side.filter, &self.side.query);
        let lay = sb::layout(sz, sc, &sb::group(&ents));
        self.side.focus = true;
        self.side.searching = false;
        match sb::hit_at(&lay, m) {
            Some(sb::Hit::New | sb::Hit::Folder) => { self.side.focus = false; self.new_tab(); } // sin selector nativo: ~
            Some(sb::Hit::Search) => self.side.searching = true,
            Some(sb::Hit::Filter(f)) => self.side.filter = f,
            Some(sb::Hit::Row(j)) => { let e = &ents[j]; self.side.focus = false; self.active_tab = e.tab; self.tabs[e.tab].active = e.pane; }
            Some(sb::Hit::Launch(i)) if self.side.installed[i] => {
                self.side.focus = false;
                self.new_tab_with(Some(vec!["zsh".into(), "-lic".into(), format!("exec {}", sb::LAUNCH[i].1)]));
            }
            _ => {}
        }
        self.redraw();
    }
    /// Teclas de la barra: `/` con foco abre el buscador; en búsqueda se escribe en él. true = consumida.
    fn side_key(&mut self, ev: &winit::event::KeyEvent) -> bool {
        let s = &mut self.side;
        if s.searching {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => { s.searching = false; s.query.clear(); }
                Key::Named(NamedKey::Enter) => s.searching = false,
                Key::Named(NamedKey::Backspace) => { s.query.pop(); }
                _ => if let Some(t) = &ev.text { s.query.extend(t.chars().filter(|c| !c.is_control())); },
            }
        } else if s.visible && s.focus && ev.logical_key.as_ref() == Key::Character("/") {
            s.searching = true;
        } else {
            return false;
        }
        self.redraw();
        true
    }
    /// Cierra un panel (y su pestaña si queda vacía; sale de la app si era la última).
    fn close_pane(&mut self, el: &ActiveEventLoop, ti: usize, pi: usize) {
        self.archive(ti, pi);
        let t = &mut self.tabs[ti];
        t.panes.remove(pi);
        if t.panes.is_empty() {
            self.close_tab(el, ti);
        } else {
            t.active = t.active.min(t.panes.len() - 1);
            self.sync_size(false);
            self.redraw();
        }
    }
    fn close_tab(&mut self, el: &ActiveEventLoop, ti: usize) {
        for pi in 0..self.tabs[ti].panes.len() { self.archive(ti, pi); }
        self.tabs.remove(ti);
        if self.tabs.is_empty() { el.exit(); return; }
        if self.active_tab > ti || self.active_tab == self.tabs.len() { self.active_tab -= 1; }
        self.sync_size(false);
        self.redraw();
    }
    fn pane(&mut self) -> &mut Pane {
        let t = &mut self.tabs[self.active_tab];
        &mut t.panes[t.active]
    }
    /// Entrada de teclado/pegado: vuelve al final del scroll y quita la selección, como una terminal.
    fn send_input(&mut self, b: &[u8]) {
        let p = self.pane();
        {
            let mut t = p.session.term.lock().unwrap();
            t.selection = None;
            t.scroll_display(Scroll::Bottom);
        }
        p.anchor = None;
        p.session.send(b);
        self.redraw();
    }
    /// Celda (col, fila) de pantalla bajo el ratón en el panel activo.
    fn cell(&mut self) -> (usize, usize) {
        let (m, g) = (self.mouse, self.g.as_ref().unwrap());
        let (cell, pad) = (g.cell, PAD * g.scale);
        let p = self.pane();
        mouse_cell(m, p.rect, cell, pad, p.cols, p.rows)
    }
    fn point(&mut self, off: usize) -> Point {
        let (m, g) = (self.mouse, self.g.as_ref().unwrap());
        let (cell, pad) = (g.cell, PAD * g.scale);
        self.pane().point(m, cell, pad, off)
    }
    /// ¿El ratón está sobre el separador del par de paneles de la pestaña activa?
    fn on_sep(&self) -> bool {
        let t = &self.tabs[self.active_tab];
        let [a, _] = &t.panes[..] else { return false };
        let tol = 3.0 * self.g.as_ref().unwrap().scale;
        self.mouse.1 >= a.rect.1 && (self.mouse.0 - (a.rect.0 + a.rect.2 + self.sep() / 2.0)).abs() <= tol
    }
}

impl winit::application::ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.g.is_none() { self.g = Some(init_gfx(el)); self.sync_size(true); self.redraw(); }
    }
    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        if self.tabs.is_empty() { return; }
        match ev {
            Ev::Out(id) => {
                let at = self.active_tab;
                for (ti, t) in self.tabs.iter_mut().enumerate() {
                    let act = t.active;
                    for (pi, p) in t.panes.iter_mut().enumerate().filter(|(_, p)| p.id == id) {
                        p.last_out = Instant::now();
                        p.unread |= !(ti == at && pi == act);
                    }
                }
                self.redraw();
            }
            Ev::Tick => if self.side.visible {
                for t in &mut self.tabs { for p in &mut t.panes { p.refresh(); } }
                self.redraw();
            },
            Ev::Exit(id) => {
                let pos = self.tabs.iter().enumerate().find_map(|(ti, t)| t.panes.iter().position(|p| p.id == id).map(|pi| (ti, pi)));
                if let Some((ti, pi)) = pos { self.close_pane(el, ti, pi); }
            }
        }
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        if self.g.is_none() || self.tabs.is_empty() { return; }
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(z) => {
                let g = self.g.as_mut().unwrap();
                g.cfg.width = z.width.max(1);
                g.cfg.height = z.height.max(1);
                g.surface.configure(&g.device, &g.cfg);
                self.sync_size(true);
                self.redraw();
            }
            WindowEvent::Occluded(false) => self.redraw(),
            WindowEvent::RedrawRequested => {
                let t = &mut self.tabs[self.active_tab];
                let a = t.active;
                t.panes[a].unread = false;
                draw(self.g.as_mut().unwrap(), &mut self.tabs, self.active_tab, self.mouse, &self.side);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let (cmd, ctrl) = (self.mods.super_key(), self.mods.control_key());
                if ctrl && event.logical_key == Key::Named(NamedKey::Tab) {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.redraw();
                    return;
                }
                if !cmd && !ctrl && self.side_key(&event) { return; }
                if cmd {
                    match event.logical_key.as_ref() {
                        Key::Character("b" | "\\") => self.toggle_side(),
                        Key::Character("c") => {
                            if let Some(s) = self.pane().session.term.lock().unwrap().selection_to_string() { pbcopy(&s); }
                        }
                        Key::Character("v") => {
                            let br = self.pane().session.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE);
                            self.send_input(&paste_bytes(&pbpaste(), br));
                        }
                        Key::Character("t" | "T") => self.new_tab(),
                        Key::Character("w") => { let (ti, pi) = (self.active_tab, self.tabs[self.active_tab].active); self.close_pane(el, ti, pi); }
                        Key::Character("d") => {
                            if self.tabs[self.active_tab].panes.len() < 2 {
                                let p = self.new_pane(None);
                                let t = &mut self.tabs[self.active_tab];
                                t.panes.push(p);
                                t.active = 1;
                                self.sync_size(false);
                                self.redraw();
                            }
                        }
                        Key::Character("[" | "]") => {
                            let t = &mut self.tabs[self.active_tab];
                            if t.panes.len() == 2 { t.active = 1 - t.active; self.redraw(); }
                        }
                        Key::Character(s) if s.len() == 1 && s.as_bytes()[0].is_ascii_digit() && s != "0" => {
                            let n = (s.as_bytes()[0] - b'1') as usize;
                            if n < self.tabs.len() { self.active_tab = n; self.redraw(); }
                        }
                        _ => {}
                    }
                    return;
                }
                let m = Mods { shift: self.mods.shift_key(), alt: self.mods.alt_key(), ctrl };
                // Con Option, macOS compone el carácter (∫): usar la tecla base para enviarla como Meta.
                let base = if m.alt { event.key_without_modifiers() } else { event.logical_key.clone() };
                let key = match &base {
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
                    Key::Character(s) if ctrl || m.alt => s.chars().next()
                        .map(|c| K::Char(if m.shift && !ctrl { c.to_ascii_uppercase() } else { c })),
                    _ => None,
                };
                let app_cursor = self.pane().session.term.lock().unwrap().mode().contains(TermMode::APP_CURSOR);
                if let Some(b) = key.and_then(|k| key_bytes(k, m, app_cursor)) {
                    self.send_input(&b);
                } else if !ctrl && !m.alt {
                    if let Some(t) = &event.text { self.send_input(t.as_bytes()); }
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
                let m = self.mouse;
                if let (true, Some(p)) = (lines != 0, self.tabs[self.active_tab].panes.iter().find(|p| p.has(m.0, m.1))) {
                    p.session.term.lock().unwrap().scroll_display(Scroll::Delta(lines));
                    self.redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let bar_h = TABBAR_H * self.g.as_ref().unwrap().scale;
                let was_bar = self.mouse.1 < bar_h;
                self.mouse = (position.x as f32, position.y as f32);
                if was_bar || self.mouse.1 < bar_h { self.redraw(); }
                if self.drag_sep {
                    let (a, sep) = (self.area(), self.sep());
                    let t = &mut self.tabs[self.active_tab];
                    t.split = ((self.mouse.0 - a.0 - sep / 2.0) / (a.2 - sep)).clamp(0.1, 0.9);
                    self.sync_size(false);
                    self.redraw();
                    return;
                }
                let icon = if self.on_sep() { CursorIcon::ColResize } else { CursorIcon::Default };
                self.g.as_ref().unwrap().window.set_cursor(icon);
                if let Some(a) = self.pane().anchor {
                    let off = self.pane().session.term.lock().unwrap().grid().display_offset();
                    let p = self.point(off);
                    let mut t = self.pane().session.term.lock().unwrap();
                    // Solo hay selección si el ratón salió de la celda del clic; si no, es un clic simple.
                    match t.selection.as_mut() {
                        Some(s) => s.update(p, Side::Right),
                        None if p != a => {
                            let mut s = Selection::new(SelectionType::Simple, a, Side::Left);
                            s.update(p, Side::Right);
                            t.selection = Some(s);
                        }
                        None => {}
                    }
                    drop(t);
                    self.redraw();
                }
            }
            WindowEvent::MouseInput { button: MouseButton::Left, state, .. } => {
                if state == ElementState::Released {
                    self.drag_sep = false;
                    let a = self.pane().anchor.take();
                    let (cell, off) = (self.cell(), self.pane().session.term.lock().unwrap().grid().display_offset());
                    let hit = a.is_some_and(|a| self.point(off) == a);
                    let fwd = std::mem::take(&mut self.mouse_fwd);
                    let p = self.pane();
                    let t = p.session.term.lock().unwrap();
                    let mode = *t.mode();
                    let c = t.grid().cursor.point;
                    let (cols, sel) = (t.columns(), t.selection.is_some());
                    if fwd {
                        drop(t);
                        let b = mouse_report(false, cell.0, cell.1, mode.contains(TermMode::SGR_MOUSE));
                        if debug_on() { eprintln!("[mt] release fwd cell={cell:?} bytes={b:?}"); }
                        p.session.send(&b);
                        return;
                    }
                    let Some(a) = a else { return };
                    let (r0, r1) = (c.line.0.min(a.line.0), c.line.0.max(a.line.0));
                    // Clic simple (misma celda, sin selección), en pantalla normal y sin ratón de la app; si cursor y clic están en la misma fila no importa WRAPLINE.
                    let ok = off == 0 && !sel && hit && !mode.intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
                        && (r0..r1).all(|l| t.grid()[Line(l)][Column(cols - 1)].flags.contains(Flags::WRAPLINE));
                    drop(t);
                    let b = if ok { click_move_bytes((c.line.0 as usize, c.column.0), (a.line.0 as usize, a.column.0), cols, mode.contains(TermMode::APP_CURSOR)) } else { vec![] };
                    if debug_on() {
                        eprintln!("[mt] release click={:?} cursor={:?} alt={} mouse={} app_cursor={} off={off} sel={sel} hit={hit} ok={ok} bytes={b:?}",
                            (a.line.0, a.column.0), (c.line.0, c.column.0), mode.contains(TermMode::ALT_SCREEN),
                            mode.intersects(TermMode::MOUSE_MODE), mode.contains(TermMode::APP_CURSOR));
                    }
                    if !b.is_empty() { p.session.send(&b); }
                    self.redraw();
                    return;
                }
                let sc = self.g.as_ref().unwrap().scale;
                if inside((BAR_LEFT * sc, 0.0, 28.0 * sc, TABBAR_H * sc), self.mouse) { self.toggle_side(); return; }
                if self.side.visible && self.mouse.0 < sb::SIDE_W * sc { self.side_click(); return; }
                self.side.focus = false;
                self.side.searching = false;
                if self.mouse.1 < TABBAR_H * sc {
                    let w = self.g.as_ref().unwrap().cfg.width as f32;
                    match bar_hit(self.mouse, self.tabs.len(), w, sc, bar_left(self.side.visible, sc)) {
                        Some(BarHit::Tab(i)) => { self.active_tab = i; self.redraw(); }
                        Some(BarHit::Close(i)) => self.close_tab(el, i),
                        Some(BarHit::Plus) => self.new_tab(),
                        None => { let _ = self.g.as_ref().unwrap().window.drag_window(); }
                    }
                    return;
                }
                if self.on_sep() { self.drag_sep = true; return; }
                let (mx, my) = self.mouse;
                let Some(i) = self.tabs[self.active_tab].panes.iter().position(|p| p.has(mx, my)) else { return };
                self.tabs[self.active_tab].active = i;
                let shift = self.mods.shift_key();
                let p = self.pane();
                if p.chip_rect.is_some_and(|[x, y, w, h]| mx >= x && mx <= x + w && my >= y && my <= y + h) {
                    p.session.term.lock().unwrap().scroll_display(Scroll::Bottom);
                } else if p.session.term.lock().unwrap().mode().intersects(TermMode::MOUSE_MODE) && !shift {
                    // La app pidió ratón (p. ej. Claude Code): el clic va como evento, no como flechas. Shift = selección.
                    let sgr = p.session.term.lock().unwrap().mode().contains(TermMode::SGR_MOUSE);
                    let cell = self.cell();
                    let b = mouse_report(true, cell.0, cell.1, sgr);
                    if debug_on() { eprintln!("[mt] press fwd cell={cell:?} sgr={sgr} bytes={b:?}"); }
                    self.pane().session.send(&b);
                    self.mouse_fwd = true;
                } else {
                    let off = {
                        let mut t = p.session.term.lock().unwrap();
                        t.selection = None;
                        t.grid().display_offset()
                    };
                    let pt = self.point(off);
                    self.pane().anchor = Some(pt);
                }
                self.redraw();
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
    let tick = el.create_proxy();
    std::thread::spawn(move || while tick.send_event(Ev::Tick).is_ok() { std::thread::sleep(std::time::Duration::from_secs(1)); });
    let side = sb::Side { visible: true, focus: false, searching: false, query: String::new(), filter: sb::Filter::All,
        archived: vec![], installed: sb::LAUNCH.map(|(_, b)| sb::on_path(b)) };
    let mut app = App { g: None, tabs: vec![], active_tab: 0, next_id: 0, proxy: el.create_proxy(),
        mods: ModifiersState::empty(), mouse: (0.0, 0.0), scroll_acc: 0.0, drag_sep: false, mouse_fwd: false, side };
    app.new_tab();
    // Solo para pruebas: escribe este comando al PTY al inicio.
    if let Ok(c) = std::env::var("MI_TERMINAL_CMD") { app.pane().session.send(format!("{c}\n").as_bytes()); }
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
    #[test]
    fn pixel_a_celda_con_origen_y_escala() {
        // Escala 2.0: celda 16x32 px, pad 16, barra 72 px, 2º panel en x=1000. Clic en la 3ª col, 2ª fila.
        let rect = (1000.0, 72.0, 900.0, 600.0);
        let m = (1000.0 + 16.0 + 2.0 * 16.0 + 5.0, 72.0 + 16.0 + 32.0 + 5.0);
        assert_eq!(mouse_cell(m, rect, (16.0, 32.0), 16.0, 50, 15), (2, 1));
        // Sobre el relleno (arriba-izquierda) se acota a (0,0).
        assert_eq!(mouse_cell((1001.0, 73.0), rect, (16.0, 32.0), 16.0, 50, 15), (0, 0));
    }
    #[test]
    fn reporte_de_raton() {
        assert_eq!(mouse_report(true, 0, 4, true), b"\x1b[<0;1;5M".to_vec());
        assert_eq!(mouse_report(false, 9, 0, true), b"\x1b[<0;10;1m".to_vec());
        assert_eq!(mouse_report(true, 0, 0, false), vec![0x1b, b'[', b'M', 32, 33, 33]);
    }
    #[test]
    fn barra_de_pestanas() {
        let (tabs, plus) = bar_rects(2, 1920.0, 2.0, 160.0);
        assert_eq!(tabs[0].0, 160.0);
        assert_eq!(plus.0, tabs[1].0 + tabs[1].2);
        let c = close_rect(tabs[0], 2.0);
        assert_eq!(bar_hit((c.0 + 2.0, 30.0), 2, 1920.0, 2.0, 160.0), Some(BarHit::Close(0)));
        assert_eq!(bar_hit((tabs[1].0 + 10.0, 30.0), 2, 1920.0, 2.0, 160.0), Some(BarHit::Tab(1)));
        assert_eq!(bar_hit((plus.0 + 5.0, 30.0), 2, 1920.0, 2.0, 160.0), Some(BarHit::Plus));
        assert_eq!(bar_hit((20.0, 30.0), 2, 1920.0, 2.0, 160.0), None);
    }
    #[test]
    fn layout_paneles() {
        let area = (0.0, 36.0, 1000.0, 600.0);
        assert_eq!(pane_rects(area, 1, 0.5, 2.0), vec![area]);
        let r = pane_rects(area, 2, 0.5, 2.0);
        assert_eq!(r[0].2 + 2.0 + r[1].2, 1000.0);
        assert_eq!(r[1].0, r[0].0 + r[0].2 + 2.0);
        assert_eq!(pane_rects(area, 2, 0.0, 2.0)[0].2, (998.0f32 * 0.1).round());
        // (500-16)/10 = 48 columnas; (300-16)/20 = 14 filas.
        assert_eq!(pty_size((0.0, 0.0, 500.0, 300.0), (10.0, 20.0), 8.0), (48, 14));
    }
}
