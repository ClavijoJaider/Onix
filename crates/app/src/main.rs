//! mi-terminal: ventana winit + wgpu (Metal) + glyphon sobre term-core.
mod glyphs;
mod marquee;
mod procs;
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
const FOCUS_LINE: Rgb = [0x4a, 0x4a, 0x56];
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

enum Ev { Out(u64), Exit(u64), Tick, Folder(Option<std::path::PathBuf>) }

/// Salida de `osascript choose folder` -> carpeta elegida; None si se canceló, falló o vino vacía.
fn parse_folder_output(stdout: &str, status_ok: bool, stderr: &str) -> Option<std::path::PathBuf> {
    if !status_ok || stderr.contains("-128") || stderr.contains("User canceled") { return None; }
    let s = stdout.strip_suffix('\n').unwrap_or(stdout);
    let s = if s.len() > 1 { s.trim_end_matches('/') } else { s };
    let s = if s.is_empty() && stdout.starts_with('/') { "/" } else { s };
    (!s.is_empty()).then(|| std::path::PathBuf::from(s))
}

/// Selector nativo de carpeta (osascript) con `start` como ubicación inicial, pasada como argv y nunca interpolada; 10 min máx.
fn choose_folder(start: &str) -> Option<std::path::PathBuf> {
    let mut ch = Command::new("osascript")
        .args(["-e", "on run argv", "-e", "POSIX path of (choose folder with prompt \"Carpeta para la nueva sesión\" default location (POSIX file (item 1 of argv)))", "-e", "end run", start])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = ch.try_wait().ok()? { break s; }
        if t0.elapsed().as_secs() > 600 { let _ = ch.kill(); let _ = ch.wait(); return None; }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let rd = |p: Option<&mut dyn std::io::Read>| { let mut b = String::new(); if let Some(p) = p { let _ = p.read_to_string(&mut b); } b };
    parse_folder_output(&rd(ch.stdout.as_mut().map(|p| p as &mut dyn std::io::Read)), status.success(), &rd(ch.stderr.as_mut().map(|p| p as &mut dyn std::io::Read)))
}

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

/// Suma `d` líneas (fraccionarias) al acumulador y devuelve las enteras, dejando el resto para el próximo evento.
fn take_lines(acc: &mut f32, d: f32) -> i32 {
    *acc += d;
    let n = *acc as i32;
    *acc -= n as f32;
    n
}

/// Rueda en pantalla alterna (sin historial propio): con ratón de la app, eventos de rueda (64 arriba, 65 abajo);
/// si no, flechas (modo ALTERNATE_SCROLL). None = desplazar el historial local. Tope de 10 por evento.
fn wheel_bytes(lines: i32, cell: (usize, usize), mode: TermMode) -> Option<Vec<u8>> {
    if !mode.contains(TermMode::ALT_SCREEN) || lines == 0 { return None; }
    let (up, n) = (lines > 0, lines.unsigned_abs().min(10) as usize);
    if mode.intersects(TermMode::MOUSE_MODE) {
        let one = if mode.contains(TermMode::SGR_MOUSE) { format!("\x1b[<{};{};{}M", if up { 64 } else { 65 }, cell.0 + 1, cell.1 + 1).into_bytes() }
            else { vec![0x1b, b'[', b'M', if up { 96 } else { 97 }, (33 + cell.0.min(222)) as u8, (33 + cell.1.min(222)) as u8] };
        Some(one.repeat(n))
    } else if mode.contains(TermMode::ALTERNATE_SCROLL) {
        let a = [0x1b, if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' }, if up { b'A' } else { b'B' }];
        Some(a.repeat(n))
    } else { None }
}

/// Pestañas -> texto: una línea por pestaña, `cwd` o `cwd\tsessionId` (se omiten cwd vacíos o con salto/tab; id no UUID se descarta).
fn tabs_to_text(tabs: &[(String, Option<String>)]) -> String {
    tabs.iter().filter(|(c, _)| !c.is_empty() && !c.contains(['\n', '\t'])).map(|(c, id)| match id.as_deref().filter(|i| sb::is_uuid(i)) {
        Some(i) => format!("{c}\t{i}\n"), None => format!("{c}\n"),
    }).collect()
}

/// Texto -> (cwd, id de Claude) a restaurar: solo rutas absolutas, id opcional validado, máx 8. Acepta el formato viejo de una columna.
fn parse_tabs(text: &str) -> Vec<(String, Option<String>)> {
    text.lines().filter(|l| l.starts_with('/')).take(8).map(|l| {
        let (c, id) = l.split_once('\t').unwrap_or((l, ""));
        (c.to_string(), Some(id).filter(|i| sb::is_uuid(i)).map(String::from))
    }).collect()
}

/// Comando para retomar una sesión de Claude: argumentos en vector (el id va como $1, sin interpolar); None si el id no es UUID.
fn resume_cmd(id: &str) -> Option<Vec<String>> {
    sb::is_uuid(id).then(|| ["zsh", "-lic", "exec claude --resume \"$1\"", "_", id].map(String::from).to_vec())
}

/// sessionId de Claude Code del proceso `pid` según `~/.claude/sessions/<pid>.json` (solo lectura); exige que el pid coincida.
fn claude_session(pid: i32) -> Option<String> {
    let f = std::path::Path::new(&std::env::var_os("HOME")?).join(format!(".claude/sessions/{pid}.json"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
    let id = v.get("sessionId")?.as_str()?;
    (v.get("pid")?.as_i64()? == pid as i64 && sb::is_uuid(id)).then(|| id.to_string())
}

extern "C" { fn kill(pid: i32, sig: i32) -> i32; fn getuid() -> u32; fn getpid() -> i32; fn getppid() -> i32; }

/// ¿Hay un proceso vivo de Claude con esa sesión? Lee `<dir>/<pid>.json` (solo lectura); el pid del json debe coincidir con el nombre.
fn session_alive(dir: &std::path::Path, id: &str) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| {
        let p = e.path();
        let Some(pid) = p.extension().is_some_and(|x| x == "json").then(|| p.file_stem()?.to_str()?.parse::<i32>().ok()).flatten() else { return false };
        let Some(v) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return false };
        v.get("sessionId").and_then(|s| s.as_str()) == Some(id) && v.get("pid").and_then(|n| n.as_i64()) == Some(pid as i64)
            && pid > 0 && unsafe { kill(pid, 0) } == 0
    })
}

fn sessions_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".claude/sessions"))
}

fn alive_now(id: &str) -> bool { sessions_dir().is_some_and(|d| session_alive(&d, id)) }

fn tabs_file() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join("Library/Application Support/mi-terminal/tabs.txt"))
}

/// Qué hay bajo el ratón en la barra de pestañas.
#[derive(Debug, PartialEq, Clone, Copy)]
enum BarHit { Tab(usize), Close(usize), Plus }

const BAR_LEFT: f32 = 80.0; // semáforos
const BAR_RIGHT: f32 = 104.0; // pulso, robot y globo
const TAB_MAX_W: f32 = 200.0;
const PLUS_W: f32 = 30.0;
const CLOSE_S: f32 = 16.0;

/// Inicio (px) de las pestañas: tras la barra lateral si se ve; si no, tras semáforos y botón de la barra.
fn bar_left(side: bool, sc: f32) -> f32 { (if side { sb::SIDE_W } else { BAR_LEFT + 62.0 }) * sc }

/// Rects (px) de las `n` pestañas y del botón `+`, desde `x0`, para una ventana de `width` px y escala `sc`.
fn bar_rects(n: usize, width: f32, sc: f32, x0: f32) -> (Vec<R>, R) {
    let h = TABBAR_H * sc;
    let tw = ((width - x0 - BAR_RIGHT * sc - PLUS_W * sc) / n.max(1) as f32).clamp(40.0 * sc, TAB_MAX_W * sc);
    ((0..n).map(|i| (x0 + i as f32 * tw, 0.0, tw, h)).collect(), (x0 + n as f32 * tw, 0.0, PLUS_W * sc, h))
}

/// Rect del botón cerrar dentro de una pestaña.
fn close_rect(t: R, sc: f32) -> R { (t.0 + t.2 - (CLOSE_S + 8.0) * sc, (t.3 - CLOSE_S * sc) / 2.0, CLOSE_S * sc, CLOSE_S * sc) }

/// Botones de arriba a la derecha (de izquierda a derecha: pulso, robot, globo), 28 px cada uno.
const BTN_PULSE: usize = 0;
const BTN_ROBOT: usize = 1;
const BTN_GLOBE: usize = 2;
fn bar_btn(width: f32, sc: f32, k: usize) -> R { (width - (96.0 - 30.0 * k as f32) * sc, 4.0 * sc, 28.0 * sc, (TABBAR_H - 8.0) * sc) }

fn inside(r: R, m: (f32, f32)) -> bool { m.0 >= r.0 && m.0 < r.0 + r.2 && m.1 >= r.1 && m.1 < r.1 + r.3 }

fn bar_hit(m: (f32, f32), n: usize, width: f32, sc: f32, x0: f32) -> Option<BarHit> {
    let (tabs, plus) = bar_rects(n, width, sc, x0);
    if inside(plus, m) { return Some(BarHit::Plus); }
    tabs.iter().position(|&t| inside(t, m)).map(|i| if inside(close_rect(tabs[i], sc), m) { BarHit::Close(i) } else { BarHit::Tab(i) })
}

fn g_w(g: &Gfx) -> f32 { g.cfg.width as f32 }

fn debug_on() -> bool { std::env::var("MI_TERMINAL_DEBUG").is_ok_and(|v| v == "1") }

/// (texto, color, negrita, ancha): las celdas anchas (CJK, emoji) van en su propio tramo.
type Span = (String, Rgb, bool, bool);

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
    resume_id: Option<String>, cwd: String,
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
        if name == "node" || path.ends_with("/node") { self.agent = sb::agent_from_args(&sb::proc_args(pid)); }
        self.had_agent |= self.agent != sb::Agent::Shell;
        if !cwd.is_empty() {
            let home = std::env::var("HOME").unwrap_or_default();
            self.project = sb::project_of(&cwd, &home);
            self.cwd = sb::tilde(&cwd, &home);
        }
    }
    fn point(&self, mouse: (f32, f32), cell: (f32, f32), pad: f32, off: usize) -> Point {
        let (c, r) = mouse_cell(mouse, self.rect, cell, pad, self.cols, self.rows);
        Point::new(Line(r as i32 - off as i32), Column(c))
    }
}

struct Tab { panes: Vec<Pane>, active: usize, split: f32, tbuf: Buffer, tprev: String, ttitle: String, tfull: f32 }

impl Tab {
    /// Título de la pestaña: la ruta del panel activo (`~/Dev/x`), nunca "zsh" ni el título OSC.
    fn title(&self) -> String { self.panes[self.active].cwd.clone() }
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
        // Glifos sueltos de la barra de pestañas: vacío (para los íconos de línea), + y x.
        let bm = self.bar_metrics();
        self.icons = ["", "+", "×"].iter().chain(marquee::SPINNER.iter()).map(|t| {
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
            title: if p.agent == sb::Agent::Shell || title.is_empty() { p.cwd.clone() } else { sb::clean_title(&title, TITLE_CH) },
            secs: now.duration_since(p.born).as_secs(),
        }
    })).collect()
}

type Inst = [f32; 8];
fn rect(out: &mut Vec<Inst>, sz: (f32, f32), x: f32, y: f32, w: f32, h: f32, c: Rgb) {
    let (nx, ny) = (x / sz.0 * 2.0 - 1.0, 1.0 - y / sz.1 * 2.0);
    out.push([nx, ny, w / sz.0 * 2.0, -h / sz.1 * 2.0, c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, 1.0]);
}

/// Ancho útil (px) del título de una pestaña de ancho `tw`.
fn title_avail(tw: f32, sc: f32) -> f32 { tw - (28.0 + CLOSE_S + 10.0) * sc }
/// Ancho reservado (px lógicos) a la derecha del título para el ícono del agente.
const ICON_W: f32 = 20.0;

/// Devuelve `true` si hay carrusel en marcha (pide otro frame). `mt`: segundos desde que el ratón entró a la pestaña.
fn draw(g: &mut Gfx, tabs: &mut [Tab], active_tab: usize, mouse: (f32, f32), side: &sb::Side, mt: f32) -> bool {
    let bm = g.bar_metrics();
    let tabs_len = tabs.len();
    let sz = (g.cfg.width as f32, g.cfg.height as f32);
    let left = bar_left(side.visible, g.scale);
    let (trs, plus) = bar_rects(tabs_len, sz.0, g.scale, left);
    let hover = bar_hit(mouse, tabs_len, sz.0, g.scale, left);
    let mut animating = false;
    let mut moff = vec![0.0f32; tabs_len];
    let mut center = vec![0.0f32; tabs_len];
    let mut avails = vec![0.0f32; tabs_len];
    for (i, t) in tabs.iter_mut().enumerate() {
        let title = t.title();
        let avail = title_avail(trs[i].2, g.scale) - if t.panes[t.active].agent.icon().is_some() { ICON_W * g.scale } else { 0.0 };
        avails[i] = avail;
        if t.ttitle != title {
            t.tbuf.set_metrics(bm);
            t.tbuf.set_size(Some(1.0e4), Some(bm.line_height));
            t.tbuf.set_wrap(Wrap::None);
            t.tbuf.set_text(&title, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
            t.tbuf.shape_until_scroll(&mut g.fs, false);
            t.tfull = t.tbuf.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
            t.ttitle = title.clone();
            t.tprev = title.clone();
        }
        let long = t.tfull > avail;
        let hov = matches!(hover, Some(BarHit::Tab(j) | BarHit::Close(j)) if j == i);
        let want = if !long || hov { title.clone() } else {
            // Menlo es monoespaciada: ancho por carácter = ancho total / nº de caracteres.
            let n = title.chars().count().max(1);
            let k = ((avail / (t.tfull / n as f32)) as usize).max(1);
            format!("{}…", title.chars().take(k - 1).collect::<String>())
        };
        if t.tprev != want {
            t.tbuf.set_text(&want, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
            t.tbuf.shape_until_scroll(&mut g.fs, false);
            t.tprev = want;
        }
        if long && hov { moff[i] = marquee::marquee_offset(mt, t.tfull - avail, 35.0 * g.scale, 1.0); animating = true; }
        if !long { center[i] = (avail - t.tfull) / 2.0; }
    }
    let ents_all = entries(tabs, active_tab);
    // Pestañas con algún panel en Working: spinner a la izquierda del título y redibujo continuo.
    let working: Vec<bool> = (0..tabs_len).map(|i| ents_all.iter().any(|e| e.tab == i && e.status == sb::Status::Working)).collect();
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let spin = marquee::spinner_frame(START.get_or_init(Instant::now).elapsed().as_secs_f32(), 10.0, marquee::SPINNER.len());
    animating |= working.contains(&true);
    let tab = &mut tabs[active_tab];
    let (cw, ch) = g.cell;
    let pad = PAD * g.scale;
    let mut rects: Vec<Inst> = vec![];
    rect(&mut rects, sz, 0.0, 0.0, sz.0, TABBAR_H * g.scale, TABBAR_BG);
    // Barra lateral: rects y textos (los buffers se reutilizan por índice, solo lo visible).
    let mut stxt: Vec<(String, f32, f32, Color, Option<(u16, f32)>)> = vec![];
    if side.visible {
        let sc = g.scale;
        let (all, wt, wk) = sb::counts(&ents_all);
        let ents = sb::filter(ents_all.clone(), side.filter, &side.query);
        let items = sb::group(&ents);
        let (dimc, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
        rect(&mut rects, sz, 0.0, 0.0, sb::SIDE_W * sc, sz.1, SIDE_BG);
        rect(&mut rects, sz, sb::SIDE_W * sc - 1.0, 0.0, 1.0, sz.1, SEP_COLOR);
        let lay = sb::layout(sz, sc, &items, if side.open_archived { side.archived.len().min(sb::ARCH_MAX) } else { 0 });
        for (r, hit) in lay {
            let (x, y, w, h) = r;
            let ty = y + (h - bm.line_height) / 2.0;
            // `gl` marca el próximo `put` como logo de marca (id, lado en px); el texto sale vacío.
            let gl = std::cell::Cell::new(None);
            let mut put = |t: String, dx: f32, dy: f32, c: Color| stxt.push((t, x + dx * sc, ty + dy * sc, c, gl.take()));
            let logo = |a: sb::Agent, s: f32| { let ic = a.icon(); gl.set(ic.map(|(id, _)| (id, s * sc))); if ic.is_some() { String::new() } else { a.glyph().into() } };
            let ico = |id: u16| gl.set(Some((id, 16.0 * sc)));
            let iy = (bm.line_height / sc - 16.0) / 2.0;
            match hit {
                sb::Hit::New => { rect(&mut rects, sz, x, y, w, h, BTN_BG); ico(sb::ICON_NEW); put(String::new(), 8.0, iy, lit); put("New session".into(), 30.0, 0.0, lit); put("⇧⌘T".into(), (w - 44.0 * sc) / sc, 0.0, dimc); }
                sb::Hit::Folder => { rect(&mut rects, sz, x, y, w, h, BTN_BG); ico(sb::ICON_FOLDER); put(String::new(), 6.0, iy, dimc); }
                sb::Hit::Search => {
                    let t = if side.searching { format!("{}▏", side.query) } else if side.query.is_empty() { "Search agents".into() } else { side.query.clone() };
                    ico(sb::ICON_SEARCH); put(String::new(), 8.0, iy, dimc); put(t, 30.0, 0.0, if side.query.is_empty() && !side.searching { dimc } else { lit });
                    put("/".into(), (w - 20.0 * sc) / sc, 0.0, dimc);
                }
                sb::Hit::Filter(f) => {
                    if side.filter == f { rect(&mut rects, sz, x + 2.0 * sc, y, w - 4.0 * sc, h, [0x24, 0x24, 0x2a]); }
                    let t = match f { sb::Filter::All => format!("All {all}"), sb::Filter::Waiting if wt > 0 => format!("Waiting {wt}"), sb::Filter::Waiting => "Waiting".into(), sb::Filter::Working => format!("Working {wk}") };
                    put(t, 12.0, 0.0, if side.filter == f { lit } else { dimc });
                }
                sb::Hit::Group(i) => if let sb::Item::Group(p) = &items[i] { put(p.clone(), 14.0, 0.0, dimc) },
                sb::Hit::Row(j) => {
                    let e = &ents[j];
                    if e.focused { rect(&mut rects, sz, x + 6.0 * sc, y, w - 12.0 * sc, h, [0x1a, 0x1a, 0x20]); }
                    let c = e.agent.color();
                    let st = match e.status { sb::Status::Done => Color::rgb(0x5a, 0xc0, 0x6a), _ => dimc };
                    let t = logo(e.agent, 18.0); put(t, 16.0, -9.0 + (bm.line_height / sc - 18.0) / 2.0, Color::rgb(c[0], c[1], c[2]));
                    if e.unread { put("●".into(), 8.0, -12.0, Color::rgb(0x5a, 0xc0, 0x6a)); }
                    put(e.title.clone(), 38.0, -9.0, lit);
                    put(sb::fmt_elapsed(e.secs), (w - 40.0 * sc) / sc, -9.0, dimc);
                    put(if e.status == sb::Status::Working { format!("{} Working", marquee::SPINNER[spin]) } else { e.status.label().into() }, 38.0, 7.0, st);
                }
                sb::Hit::Launch(i) => {
                    let a = sb::LAUNCH[i].0;
                    let c = if side.installed[i] { a.color() } else { [0x3a, 0x3a, 0x42] };
                    let t = logo(a, 18.0); put(t, 6.0, (bm.line_height / sc - 18.0) / 2.0, Color::rgb(c[0], c[1], c[2]));
                }
                sb::Hit::Archived => { ico(sb::ICON_ARCHIVE); put(String::new(), 8.0, iy, dimc); put("Archived".into(), 30.0, 0.0, dimc); put(side.archived.len().to_string(), (w - 24.0 * sc) / sc, 0.0, dimc);
                    if side.open_archived {
                        let rows = sb::archived_rows(&side.archived, SystemTime::now());
                        let tiny = Color::rgb(0x5c, 0x5c, 0x66);
                        for (k, (t, when)) in rows.iter().enumerate() {
                            let ry = sb::arch_y(sz.1 / sc, rows.len(), k) * sc + (sb::ARCH_H * sc - bm.line_height) / 2.0;
                            stxt.push((t.clone(), 14.0 * sc, ry, tiny, None));
                            stxt.push((when.clone(), (sb::SIDE_W - 96.0) * sc, ry, tiny, None));
                        }
                    }
                }
            }
        }
    }
    // Columna derecha: Agents arriba y Processes abajo (cada uno con su encabezado, filas y pie).
    let (col_a, col_p) = sb::column(sz, g.scale, TABBAR_H * g.scale, side.agents_open, side.procs_open);
    if col_a.is_some() || col_p.is_some() {
        let sc = g.scale;
        let lh = bm.line_height;
        let (dimc, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
        let cx = sz.0 - sb::RIGHT_W * sc;
        rect(&mut rects, sz, cx, TABBAR_H * sc, sb::RIGHT_W * sc, sz.1 - TABBAR_H * sc, SIDE_BG);
        rect(&mut rects, sz, cx, TABBAR_H * sc, 1.0, sz.1 - TABBAR_H * sc, SEP_COLOR);
        if let Some((px, py, pw, ph)) = col_a {
            let arows = sb::agent_rows(ents_all.clone());
            let ty0 = py + (40.0 * sc - lh) / 2.0;
            rect(&mut rects, sz, px, py + 40.0 * sc - 1.0, pw, 1.0, SEP_COLOR);
            stxt.push((String::new(), px + 14.0 * sc, py + (40.0 - 16.0) / 2.0 * sc, lit, Some((sb::ICON_ROBOT, 16.0 * sc))));
            stxt.push(("Agents".into(), px + 38.0 * sc, ty0, lit, None));
            stxt.push((format!("{} working", sb::counts(&arows).2), px + 100.0 * sc, ty0, dimc, None));
            stxt.push(("×".into(), px + pw - 26.0 * sc, ty0, dimc, None));
            for ((x, y, w, h), hit) in sb::agents_layout((px, py, pw, ph), sc, arows.len()) {
                let sb::AHit::Row(i) = hit else { continue };
                let e = &arows[i];
                let hov = inside((x, y, w, h), mouse);
                if (side.col_focus && i == side.agents_sel) || hov { rect(&mut rects, sz, x + 6.0 * sc, y, w - 12.0 * sc, h, [0x1a, 0x1a, 0x20]); }
                let (c, ty) = (e.agent.color(), y + (h - lh) / 2.0);
                stxt.push((String::new(), x + 16.0 * sc, y + (h - 18.0 * sc) / 2.0, Color::rgb(c[0], c[1], c[2]), e.agent.icon().map(|(id, _)| (id, 18.0 * sc))));
                stxt.push((e.project.clone(), x + 42.0 * sc, ty - 9.0 * sc, lit, None));
                stxt.push((sb::fmt_elapsed(e.secs), x + w - 44.0 * sc, ty - 9.0 * sc, dimc, None));
                let wl = format!("{} Working", marquee::SPINNER[spin]);
                let (label, col) = match e.status { sb::Status::Working => (wl.as_str(), dimc), sb::Status::Waiting => ("● Waiting for your message", dimc), sb::Status::Done => ("Done", Color::rgb(0x5a, 0xc0, 0x6a)) };
                stxt.push((label.into(), x + 42.0 * sc, ty + 7.0 * sc, col, None));
                if hov { stxt.push(("→".into(), x + w - 28.0 * sc, ty + 7.0 * sc, lit, None)); }
            }
            let armed = side.stop_at.is_some_and(|t| t.elapsed().as_secs() < sb::STOP_SECS);
            rect(&mut rects, sz, px, py + ph - 32.0 * sc, pw, 1.0, SEP_COLOR);
            stxt.push((if armed { "press s again to stop" } else { "↑↓ select   ⏎ go   s stop   esc back" }.into(), px + 14.0 * sc, py + ph - 32.0 * sc + (32.0 * sc - lh) / 2.0, dimc, None));
        }
        if let Some((px, py, pw, ph)) = col_p {
            if col_a.is_some() { rect(&mut rects, sz, px, py, pw, 1.0, SEP_COLOR); }
            let ty0 = py + (40.0 * sc - lh) / 2.0;
            let (amber, blue) = (Color::rgb(0xe0, 0xa0, 0x40), Color::rgb(0x5a, 0x9c, 0xf0));
            let (snap, gb) = { let s = side.mon.snap.lock().unwrap(); (s.clone(), 1.0 / (1u64 << 30) as f64) };
            let prows = procs::rows(&snap.procs, &snap.ports, side.psort, &side.pquery);
            stxt.push((String::new(), px + 14.0 * sc, py + (40.0 - 16.0) / 2.0 * sc, lit, Some((sb::ICON_PULSE, 16.0 * sc))));
            stxt.push(("Processes".into(), px + 38.0 * sc, ty0, lit, None));
            if snap.total > 0 { stxt.push((format!("CPU {:.0} %  RAM {:.0}/{:.0} GB", snap.cpu, snap.used as f64 * gb, snap.total as f64 * gb), px + 118.0 * sc, ty0, dimc, None)); }
            stxt.push(("×".into(), px + pw - 26.0 * sc, ty0, dimc, None));
            rect(&mut rects, sz, px, py + 40.0 * sc - 1.0, pw, 1.0, SEP_COLOR);
            let sy = py + 40.0 * sc + (28.0 * sc - lh) / 2.0;
            stxt.push((String::new(), px + 14.0 * sc, py + 40.0 * sc + 6.0 * sc, dimc, Some((sb::ICON_SEARCH, 16.0 * sc))));
            stxt.push((if side.pquery.is_empty() { "Search…".into() } else { format!("{}▏", side.pquery) }, px + 36.0 * sc, sy, if side.pquery.is_empty() { dimc } else { lit }, None));
            let by_cpu = side.psort == procs::Sort::Cpu;
            stxt.push(("CPU".into(), px + pw - 118.0 * sc, sy, if by_cpu { blue } else { dimc }, None));
            stxt.push(("Memory".into(), px + pw - 66.0 * sc, sy, if by_cpu { dimc } else { blue }, None));
            rect(&mut rects, sz, px, py + 68.0 * sc - 1.0, pw, 1.0, SEP_COLOR);
            for ((x, y, w, h), hit) in sb::procs_layout((px, py, pw, ph), sc, prows.len()) {
                let sb::AHit::Row(i) = hit else { continue };
                let r = &prows[i];
                if (side.col_focus && i == side.psel) || inside((x, y, w, h), mouse) { rect(&mut rects, sz, x + 6.0 * sc, y, w - 12.0 * sc, h, [0x1a, 0x1a, 0x20]); }
                let ty = y + (h - lh) / 2.0;
                let cut = |s: &str, n: usize| { let c: Vec<char> = s.chars().collect(); if c.len() <= n { s.to_string() } else { format!("{}…", c[..n - 1].iter().collect::<String>()) } };
                stxt.push((cut(&r.name, 22), x + 14.0 * sc, ty - 9.0 * sc, lit, None));
                if let Some(p) = &r.parent { stxt.push((cut(p, 14), x + 14.0 * sc, ty + 7.0 * sc, dimc, None)); }
                if let Some(p) = r.port { stxt.push((format!(":{p}"), x + 14.0 * sc + 15.0 * 0.6 * lh, ty + 7.0 * sc, blue, None)); }
                stxt.push((format!("{:.1} %", r.cpu), x + w - 118.0 * sc, ty - 9.0 * sc, if r.cpu >= 50.0 { amber } else { dimc }, None));
                stxt.push((procs::fmt_mem(r.rss_kb), x + w - 66.0 * sc, ty - 9.0 * sc, dimc, None));
            }
            let fy = py + ph - 32.0 * sc + (32.0 * sc - lh) / 2.0;
            rect(&mut rects, sz, px, py + ph - 32.0 * sc, pw, 1.0, SEP_COLOR);
            if let Some((pid, name, ..)) = &side.pstop {
                stxt.push((format!("¿Terminar {} ({pid})?  ⏎ sí · esc no", name.chars().take(14).collect::<String>()), px + 14.0 * sc, fy, lit, None));
            } else {
                stxt.push((if side.pchanged { "el proceso cambió" } else if side.pnope { "no permitido" } else { "↑↓ select   ⏎ go   ⌘⌫ stop   ⇥ sort   esc" }.into(), px + 14.0 * sc, fy, dimc, None));
            }
        }
    }
    {
        let sc = g.scale;
        let bm = g.bar_metrics();
        for (i, (t, ..)) in stxt.iter().enumerate() {
            if i >= g.side.len() { let b = Buffer::new(&mut g.fs, bm); g.side.push((String::new(), b)); }
            let (k, b) = &mut g.side[i];
            if k != t {
                b.set_size(Some(sb::SIDE_W.max(sb::RIGHT_W) * sc), Some(bm.line_height));
                b.set_wrap(Wrap::None);
                b.set_text(t, &Attrs::new().family(Family::Name("Menlo")), Shaping::Advanced, None);
                b.shape_until_scroll(&mut g.fs, false);
                *k = t.clone();
            }
        }
    }
    for (btn, on) in [(BTN_PULSE, side.procs_open), (BTN_ROBOT, side.agents_open), (BTN_GLOBE, false)] {
        let r = bar_btn(sz.0, g.scale, btn);
        if on { rect(&mut rects, sz, r.0, r.1, r.2, r.3, [0x16, 0x26, 0x3c]); } else if inside(r, mouse) { rect(&mut rects, sz, r.0, r.1, r.2, r.3, TAB_HOVER); }
    }
    for (i, &t) in trs.iter().enumerate() {
        if i == active_tab { rect(&mut rects, sz, t.0, t.1, t.2, t.3, TAB_ACTIVE); }
        else if hover == Some(BarHit::Tab(i)) || hover == Some(BarHit::Close(i)) { rect(&mut rects, sz, t.0, t.1, t.2, t.3, TAB_HOVER); }
        if i + 1 < trs.len() { rect(&mut rects, sz, t.0 + t.2 - 1.0, 8.0 * g.scale, 1.0, t.3 - 16.0 * g.scale, SEP_COLOR); }
        if hover == Some(BarHit::Close(i)) { let c = close_rect(t, g.scale); rect(&mut rects, sz, c.0, c.1, c.2, c.3, [0x34, 0x34, 0x3c]); }
    }
    if hover == Some(BarHit::Plus) { rect(&mut rects, sz, plus.0, plus.1, plus.2, plus.3, TAB_HOVER); }
    if let [a, b] = &tab.panes[..] {
        let x = a.rect.0 + a.rect.2;
        rect(&mut rects, sz, x, a.rect.1, b.rect.0 - x, a.rect.3, SEP_COLOR);
        let f = &tab.panes[tab.active].rect;
        rect(&mut rects, sz, f.0, f.1, f.2, 2.0 * g.scale, FOCUS_LINE);
    }
    let act = tab.active;
    let mut chips: Vec<[f32; 4]> = vec![];
    let wide_ls = 2.0 * cw / g.metrics().font_size - 1.0;
    for (pi, p) in tab.panes.iter_mut().enumerate() {
    let (rx, ry, rw, rh) = p.rect;
    let (ox, oy) = (rx + pad, ry + pad);
    let mut grid: Vec<Vec<(usize, usize, char, Rgb, Rgb, bool)>> = vec![vec![]; p.rows];
    let off;
    let mut hollow = None;
    let mut cbar = None;
    {
        let term = p.session.term.lock().unwrap();
        let content = term.renderable_content();
        off = content.display_offset;
        let cursor = (content.cursor.shape != CursorShape::Hidden && off == 0).then_some(content.cursor.point);
        // Solo el panel activo lleva cursor sólido; en los demás, hueco.
        let solid = pi == act;
        if !solid { hollow = cursor; }
        // Cursor sólido en bloque: barra fina azul (el resto de formas se dibuja como antes).
        let bar_cur = solid && content.cursor.shape == CursorShape::Block;
        if bar_cur { cbar = cursor; }
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
            if c.flags.contains(Flags::INVERSE) ^ selected ^ (solid && !bar_cur && cursor == Some(it.point)) {
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
            let mut chr = c.2;
            let mut drawn = false;
            // Cajas, bloques, braille y símbolos: se dibujan como rects y el texto deja un espacio.
            if let Some(rs) = glyphs::cell_rects(chr, cw * c.1 as f32, ch) {
                let cx = ox + c.0 as f32 * cw;
                for [rx, ry, rw, rh, a] in rs {
                    let col = if a < 1.0 { [0, 1, 2].map(|k| (c.4[k] as f32 + (c.3[k] as f32 - c.4[k] as f32) * a).round() as u8) } else { c.3 };
                    let (x0, y0) = ((cx + rx).round(), (y + ry).round());
                    rect(&mut rects, sz, x0, y0, (cx + rx + rw).round() - x0, (y + ry + rh).round() - y0, col);
                }
                chr = ' ';
                drawn = true;
            }
            // Celda ancha dibujada a mano: 2 espacios normales (sin letter_spacing de CJK/emoji).
            let (n, wide) = if drawn { (c.1, false) } else { (1, c.1 == 2) };
            match spans.last_mut() {
                Some(l) if l.1 == c.3 && l.2 == c.5 && !l.3 && !wide => for _ in 0..n { l.0.push(chr) },
                _ => spans.push((std::iter::repeat(chr).take(n).collect(), c.3, c.5, wide)),
            }
        }
        let base = Attrs::new().family(Family::Name("Menlo"));
        let it = spans.iter().map(|(t, f, b, wide)| {
            let a = base.clone().color(Color::rgb(f[0], f[1], f[2])).weight(if *b { Weight::BOLD } else { Weight::NORMAL });
            // shortcut: se asume avance de 1 em en la fuente de respaldo (CJK/emoji); el sobrante hasta 2 celdas va como espaciado.
            (t.as_str(), if *wide { a.letter_spacing(wide_ls) } else { a })
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
    if let Some(c) = cbar.filter(|c| c.line.0 >= 0 && (c.line.0 as usize) < p.rows) {
        rect(&mut rects, sz, ox + c.column.0 as f32 * cw, oy + c.line.0 as f32 * ch, 2.0 * g.scale, ch, [0x5a, 0x8d, 0xee]);
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
    // Crece por potencias de 2 y se encoge cuando el uso cae a menos de 1/4 (braille denso).
    if rects.len() > g.icap || (g.icap > 4096 && rects.len() * 4 < g.icap) {
        g.icap = rects.len().max(1024).next_power_of_two();
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
    // Ícono del agente de cada pestaña (Shell no lleva).
    let ix: Vec<f32> = tabs.iter().enumerate().map(|(i, t)| center[i] + t.tfull.min(title_avail(trs[i].2, sc) - ICON_W * sc) + 5.0 * sc).collect();
    let tabg: Vec<Option<[CustomGlyph; 1]>> = tabs.iter().enumerate().map(|(i, t)| { let a = t.panes[t.active].agent; let c = a.color(); a.icon().map(|(id, _)| lg(id, 15.0 * sc, ix[i], (bm.line_height - 15.0 * sc) / 2.0, Color::rgb(c[0], c[1], c[2]))) }).collect();
    let sideg: Vec<[CustomGlyph; 1]> = stxt.iter().map(|e| match e.4 { Some((id, s)) => lg(id, s, 0.0, 0.0, e.3), None => lg(u16::MAX, 0.0, 0.0, 0.0, e.3) }).collect();
    for (i, (&t, tb)) in trs.iter().zip(tabs).enumerate() {
        let col = if i == active_tab { lit } else { dim_c };
        let b = TextBounds { left: t.0 as i32, top: 0, right: (t.0 + t.2 - (CLOSE_S + 10.0) * sc) as i32, bottom: full.bottom };
        // Logo de Claude de la pestaña: área aparte, fija mientras el título se desplaza.
        bar.push(TextArea { buffer: &g.icons[0], left: t.0 + 28.0 * sc, top: ty, scale: 1.0, bounds: b, default_color: col, custom_glyphs: tabg[i].as_ref().map_or(&[], |g| &g[..]) });
        if working[i] {
            bar.push(TextArea { buffer: &g.icons[3 + spin], left: t.0 + 10.0 * sc, top: ty, scale: 1.0, bounds: b, default_color: Color::rgb(0x5c, 0x5c, 0x66), custom_glyphs: &[] });
        }
        // El título (y su carrusel) se recorta entre el spinner y el ícono del agente: nunca los pisa.
        let tb_b = TextBounds { left: (t.0 + 26.0 * sc) as i32, right: (t.0 + 28.0 * sc + avails[i]) as i32, ..b };
        bar.push(TextArea { buffer: &tb.tbuf, left: t.0 + 28.0 * sc + center[i] - moff[i], top: ty, scale: 1.0, bounds: tb_b, default_color: col, custom_glyphs: &[] });
        if i == active_tab || matches!(hover, Some(BarHit::Tab(j) | BarHit::Close(j)) if j == i) {
            let c = close_rect(t, sc);
            bar.push(TextArea { buffer: &g.icons[2], left: c.0 + 4.0 * sc, top: ty, scale: 1.0, bounds: full, default_color: lit, custom_glyphs: &[] });
        }
    }
    bar.push(TextArea { buffer: &g.icons[1], left: plus.0 + 10.0 * sc, top: ty, scale: 1.0, bounds: full, default_color: dim_c, custom_glyphs: &[] });
    // Sin color propio: toman el de su TextArea (resaltado cuando el panel está abierto).
    let barg = [sb::ICON_PANEL, sb::ICON_PULSE, sb::ICON_GLOBE, sb::ICON_ROBOT].map(|id| { let mut a = lg(id, 16.0 * sc, 0.0, (bm.line_height - 16.0 * sc) / 2.0, dim_c); a[0].color = None; a });
    bar.push(TextArea { buffer: &g.icons[0], left: (BAR_LEFT + 2.0) * sc, top: ty, scale: 1.0, bounds: full, default_color: dim_c, custom_glyphs: &barg[0] });
    let sb_bounds = TextBounds { left: 0, top: 0, right: (sb::SIDE_W * sc) as i32, bottom: g.cfg.height as i32 };
    let col_x = sz.0 - sb::RIGHT_W * sc;
    let col_bounds = TextBounds { left: col_x as i32, top: (TABBAR_H * sc) as i32, right: g.cfg.width as i32, bottom: g.cfg.height as i32 };
    for (((_, x, y, c, ic), (_, b)), gl) in stxt.iter().zip(&g.side).zip(&sideg) {
        let bounds = if *x >= col_x && (side.agents_open || side.procs_open) { col_bounds } else { sb_bounds };
        bar.push(TextArea { buffer: b, left: *x, top: *y, scale: 1.0, bounds, default_color: *c, custom_glyphs: if ic.is_some() { gl } else { &[] } });
    }
    // Pulso, robot y globo arriba a la derecha; resaltados (azul) mientras su panel está abierto.
    let blue_on = Color::rgb(0x6a, 0xa6, 0xff);
    for (k, btn, on) in [(1, BTN_PULSE, side.procs_open), (3, BTN_ROBOT, side.agents_open), (2, BTN_GLOBE, false)] {
        let r = bar_btn(sz.0, sc, btn);
        bar.push(TextArea { buffer: &g.icons[0], left: r.0 + 6.0 * sc, top: ty, scale: 1.0, bounds: full, default_color: if on { blue_on } else { dim_c }, custom_glyphs: &barg[k] });
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
            g.surface.configure(&g.device, &g.cfg); g.window.request_redraw(); return false;
        }
        // Occluded/Timeout/Validation: sin redraw; Occluded(false) o el próximo evento lo reanudan.
        _ => return false,
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
    animating
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
    // Respaldo para emoji, CJK, símbolos e íconos Nerd Font: solo se carga lo que existe (fontdb las mapea, no las copia).
    let home = std::env::var("HOME").unwrap_or_default();
    let nerd = [format!("{home}/Library/Fonts"), "/Library/Fonts".into()].into_iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains("Nerd")));
    let sys = ["/System/Library/Fonts/Menlo.ttc", "/System/Library/Fonts/Apple Symbols.ttf", "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
        "/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Apple Color Emoji.ttc", "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/AppleSDGothicNeo.ttc", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"].map(std::path::PathBuf::from);
    for p in sys.into_iter().chain(nerd) { let _ = db.load_font_file(p); }
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

#[derive(Clone, Copy, PartialEq, Debug)]
enum DcAction { Minimize, Maximize, Nothing }
static DOUBLE_CLICK_ACTION: std::sync::OnceLock<DcAction> = std::sync::OnceLock::new();
/// Valor de `AppleActionOnDoubleClick` -> acción; ausente o desconocido = maximizar.
fn parse_dc_action(v: &str) -> DcAction {
    match v.trim() { "Minimize" => DcAction::Minimize, "None" => DcAction::Nothing, _ => DcAction::Maximize }
}
fn is_double_click(prev: Option<(Instant, (f32, f32))>, now: Instant, pos: (f32, f32), tol: f32) -> bool {
    prev.is_some_and(|(t, p)| now.duration_since(t).as_millis() <= 400 && (pos.0 - p.0).abs() <= tol && (pos.1 - p.1).abs() <= tol)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum TogAct { ExitFullscreen, Restore, Maximize }
/// Decide el doble clic de "maximizar": is_maximized() de winit falla en ventanas sin decoración, por eso hay estado propio (`restore`).
fn toggle_action(fullscreen: bool, restore_some: bool, is_max: bool) -> TogAct {
    if fullscreen { TogAct::ExitFullscreen } else if restore_some || is_max { TogAct::Restore } else { TogAct::Maximize }
}

struct App {
    g: Option<Gfx>, tabs: Vec<Tab>, active_tab: usize, next_id: u64, proxy: EventLoopProxy<Ev>,
    mods: ModifiersState, mouse: (f32, f32), scroll_acc: f32, drag_sep: bool, mouse_fwd: bool,
    picking: bool, mq: (Option<usize>, Instant),
    side: sb::Side, last_bar_click: Option<(Instant, (f32, f32))>, last_sep_click: Option<(Instant, (f32, f32))>, ticks: u32,
    restore: Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)>,
}

impl App {
    /// Clic en la franja superior sin control: doble clic = acción de ventana de macOS; si no, arrastrar.
    fn bar_press(&mut self) {
        let (now, pos) = (Instant::now(), self.mouse);
        let tol = 5.0 * self.g.as_ref().unwrap().scale;
        let w = &self.g.as_ref().unwrap().window;
        if is_double_click(self.last_bar_click, now, pos, tol) {
            self.last_bar_click = None;
            match *DOUBLE_CLICK_ACTION.get_or_init(|| parse_dc_action(&std::process::Command::new("defaults").args(["read", "-g", "AppleActionOnDoubleClick"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default())) {
                DcAction::Minimize => w.set_minimized(true),
                DcAction::Maximize => match toggle_action(w.fullscreen().is_some(), self.restore.is_some(), w.is_maximized()) {
                    TogAct::ExitFullscreen => { w.set_fullscreen(None); self.restore = None; }
                    TogAct::Restore => {
                        w.set_maximized(false);
                        if let Some((p, sz)) = self.restore.take() { w.set_outer_position(p); let _ = w.request_inner_size(sz); }
                    }
                    TogAct::Maximize => {
                        if let (Ok(p), sz) = (w.outer_position(), w.inner_size()) { self.restore = Some((p, sz)); }
                        w.set_maximized(true);
                    }
                },
                DcAction::Nothing => {}
            }
        } else {
            self.last_bar_click = Some((now, pos));
            let _ = w.drag_window();
        }
    }
    fn sep(&self) -> f32 { (SEP_W * self.g.as_ref().map_or(1.0, |g| g.scale)).round().max(1.0) }
    /// Área de paneles: la ventana menos la franja de pestañas.
    fn area(&self) -> R {
        let g = self.g.as_ref().unwrap();
        let (top, left) = (TABBAR_H * g.scale, if self.side.visible { sb::SIDE_W * g.scale } else { 0.0 });
        let right = if self.side.agents_open || self.side.procs_open { sb::RIGHT_W * g.scale } else { 0.0 };
        (left, top, (g.cfg.width as f32 - left - right).max(1.0), (g.cfg.height as f32 - top).max(1.0))
    }
    fn redraw(&self) { if let Some(g) = &self.g { g.window.request_redraw(); } }
    fn new_pane(&mut self, cmd: Option<Vec<String>>, cwd: Option<std::path::PathBuf>) -> Pane {
        let id = self.next_id;
        self.next_id += 1;
        let (p1, p2) = (self.proxy.clone(), self.proxy.clone());
        let session = Session::spawn(cmd, cwd, move || { let _ = p1.send_event(Ev::Out(id)); }, move || { let _ = p2.send_event(Ev::Exit(id)); });
        let mut p = Pane { id, session, anchor: None, bufs: vec![], prev: vec![], chip_rect: None, cols: 0, rows: 0, rect: (0.0, 0.0, 1.0, 1.0),
            born: Instant::now(), last_out: Instant::now(), unread: false, agent: sb::Agent::Shell, had_agent: false, project: "~".into(), resume_id: None, cwd: "~".into() };
        p.refresh();
        p
    }
    fn new_tab(&mut self) { self.new_tab_with(None, None); }
    /// Abre el selector de carpeta en un hilo (uno a la vez); el resultado vuelve como Ev::Folder.
    fn pick_folder(&mut self) {
        if self.picking || self.tabs.is_empty() { return; }
        self.picking = true;
        let p = self.pane();
        let pid = p.session.fg_pid().or(p.session.child_pid.map(|p| p as i32));
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let start = pid.map(|pid| sb::proc_info(pid).2).filter(|c| std::path::Path::new(c).is_dir()).unwrap_or(home);
        let px = self.proxy.clone();
        std::thread::spawn(move || { let _ = px.send_event(Ev::Folder(choose_folder(&start))); });
    }
    fn new_tab_with(&mut self, cmd: Option<Vec<String>>, cwd: Option<std::path::PathBuf>) {
        let p = self.new_pane(cmd, cwd);
        self.tabs.push(Tab { panes: vec![p], active: 0, split: 0.5, tbuf: Buffer::new_empty(Metrics::new(1.0, 1.0)), tprev: String::new(), ttitle: String::new(), tfull: 0.0 });
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
        let title = if p.agent == sb::Agent::Shell || t.is_empty() { p.cwd.clone() } else { sb::clean_title(&t, TITLE_CH) };
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
    /// Abre (o enfoca) una sesión de Claude; None si no es UUID.
    fn open_resume(&mut self, id: &str, cwd: std::path::PathBuf) {
        if let Some(cmd) = resume_cmd(id) {
            self.new_tab_with(Some(cmd), Some(cwd));
            self.tabs.last_mut().unwrap().panes[0].resume_id = Some(id.to_string());
        }
    }
    /// Enfoca la pestaña y el panel de un agente.
    fn go_agent(&mut self, e: &sb::Entry) {
        self.side.focus = false;
        self.active_tab = e.tab;
        self.tabs[e.tab].active = e.pane;
    }
    /// Agents y Processes van en la columna derecha (pueden estar los dos); los paneles de terminal se reajustan.
    fn toggle_agents(&mut self) {
        let s = &mut self.side;
        (s.agents_open, s.agents_sel, s.stop_at, s.searching) = (!s.agents_open, 0, None, false);
        s.col_focus = s.agents_open || s.procs_open;
        self.sync_size(false);
        self.redraw();
    }
    fn toggle_procs(&mut self) {
        let s = &mut self.side;
        (s.procs_open, s.psel, s.pquery, s.pstop, s.pnope, s.searching) = (!s.procs_open, 0, String::new(), None, false, false);
        s.mon.set(s.procs_open);
        s.col_focus = s.agents_open || s.procs_open;
        self.sync_size(false);
        self.redraw();
    }
    fn close_agents(&mut self) { self.side.agents_open = false; self.sync_size(false); }
    fn close_procs(&mut self) { self.side.procs_open = false; self.side.mon.set(false); self.sync_size(false); }
    /// Rects (px) de Agents y Processes en la columna derecha.
    fn column(&self) -> (Option<R>, Option<R>) {
        let g = self.g.as_ref().unwrap();
        sb::column((g.cfg.width as f32, g.cfg.height as f32), g.scale, TABBAR_H * g.scale, self.side.agents_open, self.side.procs_open)
    }
    /// Enfoca el panel de la app cuyo pty es el proceso (o un ancestro suyo); si no hay, nada.
    fn go_proc(&mut self, pid: i32) {
        let ch = procs::chain(&self.side.mon.snap.lock().unwrap().procs, pid);
        let hit = self.tabs.iter().enumerate().find_map(|(ti, t)| t.panes.iter().position(|p| {
            [p.session.fg_pid(), p.session.child_pid.map(|c| c as i32)].into_iter().flatten().any(|x| ch.contains(&x))
        }).map(|pi| (ti, pi)));
        if let Some((ti, pi)) = hit { self.side.focus = false; self.active_tab = ti; self.tabs[ti].active = pi; }
    }
    /// Teclas del panel Processes (abierto y visible): consume todo para que nada llegue al PTY.
    fn procs_key(&mut self, ev: &winit::event::KeyEvent) {
        let rows = { let s = self.side.mon.snap.lock().unwrap(); procs::rows(&s.procs, &s.ports, self.side.psort, &self.side.pquery) };
        let sel = self.side.psel.min(rows.len().saturating_sub(1));
        let pending = self.side.pstop.take();
        (self.side.pnope, self.side.pchanged) = (false, false);
        match ev.logical_key.as_ref() {
            Key::Named(NamedKey::ArrowUp) => self.side.psel = sb::move_sel(sel, -1, rows.len()),
            Key::Named(NamedKey::ArrowDown) => self.side.psel = sb::move_sel(sel, 1, rows.len()),
            Key::Named(NamedKey::Tab) => self.side.psort = if self.side.psort == procs::Sort::Cpu { procs::Sort::Mem } else { procs::Sort::Cpu },
            Key::Named(NamedKey::Escape) => if pending.is_none() { self.close_procs() },
            Key::Named(NamedKey::Enter) => match pending {
                // SAFETY: getuid/getpid/getppid no tocan memoria; kill con SIGTERM (15), nunca SIGKILL.
                Some((pid, name, ppid, uid)) => {
                    let ok = procs::same_proc(&self.side.mon.snap.lock().unwrap().procs, pid, &name, ppid, uid);
                    if !ok { self.side.pchanged = true; }
                    else if unsafe { procs::can_stop(uid, getuid(), pid, getpid(), getppid()) } { unsafe { kill(pid, 15); } } else { self.side.pnope = true; }
                },
                None => if let Some(r) = rows.get(sel) { self.go_proc(r.pid); },
            },
            Key::Named(NamedKey::Backspace) if self.mods.super_key() => if let Some(r) = rows.get(sel) {
                // SAFETY: llamadas libc sin punteros.
                if unsafe { procs::can_stop(r.uid, getuid(), r.pid, getpid(), getppid()) } { self.side.pstop = Some((r.pid, r.name.clone(), r.ppid, r.uid)); } else { self.side.pnope = true; }
            },
            Key::Named(NamedKey::Backspace) => { self.side.pquery.pop(); self.side.psel = 0; }
            _ => if let Some(t) = &ev.text { self.side.pquery.extend(t.chars().filter(|c| !c.is_control())); self.side.psel = 0; },
        }
        self.redraw();
    }
    /// Teclas del panel Agents (abierto y visible): consume todo para que nada llegue al PTY.
    fn agents_key(&mut self, ev: &winit::event::KeyEvent) {
        let rows = sb::agent_rows(entries(&self.tabs, self.active_tab));
        let sel = self.side.agents_sel.min(rows.len().saturating_sub(1));
        let moved = |s: &mut sb::Side, d| { s.agents_sel = sb::move_sel(sel, d, rows.len()); s.stop_at = None; };
        match ev.logical_key.as_ref() {
            Key::Named(NamedKey::ArrowUp) => moved(&mut self.side, -1),
            Key::Named(NamedKey::ArrowDown) => moved(&mut self.side, 1),
            Key::Named(NamedKey::Escape) => self.close_agents(),
            Key::Named(NamedKey::Enter) => if let Some(e) = rows.get(sel) { self.go_agent(e); },
            Key::Character("s" | "S") => if let Some(e) = rows.get(sel) {
                if self.side.stop_at.take().is_some_and(|t| t.elapsed().as_secs() < sb::STOP_SECS) {
                    self.tabs[e.tab].panes[e.pane].session.send(b"\x03");
                } else { self.side.stop_at = Some(Instant::now()); }
            },
            _ => {}
        }
        self.redraw();
    }
    /// Clic en la columna derecha (Agents arriba, Processes abajo).
    fn col_click(&mut self) {
        let (m, sc) = (self.mouse, self.g.as_ref().unwrap().scale);
        self.side.col_focus = true;
        self.side.focus = false;
        let (ra, rp) = self.column();
        if let Some(r) = ra.filter(|r| inside(*r, m)) {
            let rows = sb::agent_rows(entries(&self.tabs, self.active_tab));
            match sb::ahit_at(&sb::agents_layout(r, sc, rows.len()), m) {
                Some(sb::AHit::Close) => self.close_agents(),
                Some(sb::AHit::Row(i)) => { self.side.agents_sel = i; self.go_agent(&rows[i]); }
                None => {}
            }
        } else if let Some(r) = rp.filter(|r| inside(*r, m)) {
            let n = { let s = self.side.mon.snap.lock().unwrap(); procs::rows(&s.procs, &s.ports, self.side.psort, &self.side.pquery) };
            match sb::ahit_at(&sb::procs_layout(r, sc, n.len()), m) {
                Some(sb::AHit::Close) => self.close_procs(),
                Some(sb::AHit::Row(i)) => { self.side.psel = i; self.go_proc(n[i].pid); }
                None => {}
            }
        }
        self.redraw();
    }
    fn side_click(&mut self) {
        let (g, m) = (self.g.as_ref().unwrap(), self.mouse);
        let sc = g.scale;
        if m.1 < TABBAR_H * sc { self.bar_press(); return; }
        let ents = sb::filter(entries(&self.tabs, self.active_tab), self.side.filter, &self.side.query);
        let lay = sb::layout((g.cfg.width as f32, g.cfg.height as f32), g.scale, &sb::group(&ents), if self.side.open_archived { self.side.archived.len().min(sb::ARCH_MAX) } else { 0 });
        self.side.focus = true;
        self.side.searching = false;
        match sb::hit_at(&lay, m) {
            Some(sb::Hit::New) => { self.side.focus = false; self.new_tab(); }
            Some(sb::Hit::Folder) => { self.side.focus = false; self.pick_folder(); }
            Some(sb::Hit::Search) => self.side.searching = true,
            Some(sb::Hit::Archived) => self.side.open_archived = !self.side.open_archived,
            Some(sb::Hit::Filter(f)) => self.side.filter = f,
            Some(sb::Hit::Row(j)) => { let e = &ents[j]; self.side.focus = false; self.active_tab = e.tab; self.tabs[e.tab].active = e.pane; }
            Some(sb::Hit::Launch(i)) if self.side.installed[i] => {
                self.side.focus = false;
                self.new_tab_with(Some(vec!["zsh".into(), "-lic".into(), format!("exec {}", sb::LAUNCH[i].1)]), None);
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
    /// Al salir (cierre de ventana, Cmd+Q o última pestaña) guarda el cwd de cada pestaña; con MI_TERMINAL_CMD no (modo prueba).
    fn exiting(&mut self, _: &ActiveEventLoop) {
        let Some(f) = tabs_file().filter(|_| std::env::var_os("MI_TERMINAL_CMD").is_none()) else { return };
        let cwds: Vec<(String, Option<String>)> = self.tabs.iter().map(|t| {
            let p = &t.panes[t.active];
            let pid = p.session.fg_pid().or(p.session.child_pid.map(|p| p as i32));
            (pid.map(|pid| sb::proc_info(pid).2).unwrap_or_default(), pid.and_then(claude_session))
        }).collect();
        if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
        let _ = std::fs::write(f, tabs_to_text(&cwds));
    }
    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        if let Ev::Folder(f) = ev {
            self.picking = false;
            // Ruta ilegible o inexistente: pestaña en HOME (cwd None).
            if let Some(d) = f { self.new_tab_with(None, std::fs::read_dir(&d).is_ok().then_some(d)); }
            return;
        }
        if self.tabs.is_empty() { return; }
        match ev {
            Ev::Folder(_) => {}
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
            Ev::Tick => {
                // Oculta la barra, se refresca cada 2 ticks y solo se redibuja si cambió agente o proyecto.
                self.ticks = self.ticks.wrapping_add(1);
                if self.side.visible {
                    for t in &mut self.tabs { for p in &mut t.panes { p.refresh(); } }
                    self.redraw();
                } else if self.ticks % 2 == 0 {
                    let mut ch = false;
                    for t in &mut self.tabs { for p in &mut t.panes {
                        let old = (p.agent, p.cwd.clone());
                        p.refresh();
                        ch |= old != (p.agent, p.cwd.clone());
                    } }
                    if ch { self.redraw(); }
                }
            }
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
            WindowEvent::DroppedFile(path) => {
                let br = self.pane().session.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE);
                self.send_input(&paste_bytes(&term_core::drop_path_text(&path.to_string_lossy()), br));
            }
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
                let (hi, bh) = (bar_hit(self.mouse, self.tabs.len(), g_w(self.g.as_ref().unwrap()), self.g.as_ref().unwrap().scale, bar_left(self.side.visible, self.g.as_ref().unwrap().scale)), self.mq);
                // Reinicia el reloj al cambiar de pestaña bajo el ratón.
                let hi = match hi { Some(BarHit::Tab(i) | BarHit::Close(i)) => Some(i), _ => None };
                if hi != bh.0 { self.mq = (hi, Instant::now()); }
                let mt = self.mq.1.elapsed().as_secs_f32();
                // Redibujo continuo solo mientras el carrusel está activo (hover sobre título largo).
                if draw(self.g.as_mut().unwrap(), &mut self.tabs, self.active_tab, self.mouse, &self.side, mt) { self.redraw(); }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let (cmd, ctrl) = (self.mods.super_key(), self.mods.control_key());
                if ctrl && event.logical_key == Key::Named(NamedKey::Tab) {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.redraw();
                    return;
                }
                if self.side.col_focus && self.side.procs_open && !ctrl && (!cmd || event.logical_key == Key::Named(NamedKey::Backspace)) { self.procs_key(&event); return; }
                if !cmd && !ctrl && self.side.col_focus && self.side.agents_open && !self.side.searching { self.agents_key(&event); return; }
                if !cmd && !ctrl && self.side_key(&event) { return; }
                if cmd {
                    match event.logical_key.as_ref() {
                        Key::Character("b" | "\\") => self.toggle_side(),
                        Key::Character("a" | "A") if self.mods.shift_key() => self.toggle_agents(),
                        Key::Character("p" | "P") if self.mods.shift_key() => self.toggle_procs(),
                        Key::Character("c") => {
                            if let Some(s) = self.pane().session.term.lock().unwrap().selection_to_string() { pbcopy(&s); }
                        }
                        Key::Character("v") => {
                            // Portapapeles sin texto (p. ej. una imagen): no se envía nada.
                            let t = pbpaste();
                            if !t.is_empty() {
                                let br = self.pane().session.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE);
                                self.send_input(&paste_bytes(&t, br));
                            }
                        }
                        Key::Named(NamedKey::Home) => { self.pane().session.term.lock().unwrap().scroll_display(Scroll::Top); self.redraw(); }
                        Key::Named(NamedKey::End) => { self.pane().session.term.lock().unwrap().scroll_display(Scroll::Bottom); self.redraw(); }
                        Key::Character("t" | "T") => self.new_tab(),
                        Key::Character("o" | "O") if self.mods.shift_key() => self.pick_folder(),
                        Key::Character("w") => { let (ti, pi) = (self.active_tab, self.tabs[self.active_tab].active); self.close_pane(el, ti, pi); }
                        Key::Character("d") => {
                            if self.tabs[self.active_tab].panes.len() < 2 {
                                let p = self.new_pane(None, None);
                                let t = &mut self.tabs[self.active_tab];
                                t.panes.push(p);
                                t.active = 1;
                                t.split = 0.5;
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
                // Shift+RePág/AvPág: historial local (no llega al shell), como las demás terminales.
                if let (true, false, false, Key::Named(k @ (NamedKey::PageUp | NamedKey::PageDown))) = (m.shift, ctrl, m.alt, &event.logical_key) {
                    let s = if *k == NamedKey::PageUp { Scroll::PageUp } else { Scroll::PageDown };
                    self.pane().session.term.lock().unwrap().scroll_display(s);
                    self.redraw();
                    return;
                }
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
                let lines = take_lines(&mut self.scroll_acc, match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / ch,
                });
                let m = self.mouse;
                if let (true, Some(i)) = (lines != 0, self.tabs[self.active_tab].panes.iter().position(|p| p.has(m.0, m.1))) {
                    let (g, shift) = (self.g.as_ref().unwrap(), self.mods.shift_key());
                    let p = &self.tabs[self.active_tab].panes[i];
                    let cell = mouse_cell(m, p.rect, g.cell, PAD * g.scale, p.cols, p.rows);
                    let mode = *p.session.term.lock().unwrap().mode();
                    match wheel_bytes(lines, cell, mode).filter(|_| !shift) {
                        Some(b) => p.session.send(&b),
                        None => p.session.term.lock().unwrap().scroll_display(Scroll::Delta(lines)),
                    }
                    self.redraw();
                }
            }
            // Sin esto el carrusel seguiría girando con el ratón fuera de la ventana.
            WindowEvent::CursorLeft { .. } => { self.mouse = (-1.0, -1.0); self.redraw(); }
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
                    let on_side = self.side.visible && self.mouse.0 < sb::SIDE_W * self.g.as_ref().unwrap().scale;
                    let (ra, rp) = self.column();
                    if on_side || ra.is_some_and(|r| inside(r, self.mouse)) || rp.is_some_and(|r| inside(r, self.mouse)) { return; }
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
                let wpx = self.g.as_ref().unwrap().cfg.width as f32;
                if inside(bar_btn(wpx, sc, BTN_PULSE), self.mouse) { self.toggle_procs(); return; }
                if inside(bar_btn(wpx, sc, BTN_ROBOT), self.mouse) { self.toggle_agents(); return; }
                if inside(bar_btn(wpx, sc, BTN_GLOBE), self.mouse) { return; }
                { let (ra, rp) = self.column(); if ra.is_some_and(|r| inside(r, self.mouse)) || rp.is_some_and(|r| inside(r, self.mouse)) { self.col_click(); return; } }
                if self.side.visible && self.mouse.0 < sb::SIDE_W * sc { self.side.col_focus = false; self.side_click(); return; }
                self.side.focus = false;
                self.side.col_focus = false;
                self.side.searching = false;
                if self.mouse.1 < TABBAR_H * sc {
                    let w = self.g.as_ref().unwrap().cfg.width as f32;
                    match bar_hit(self.mouse, self.tabs.len(), w, sc, bar_left(self.side.visible, sc)) {
                        Some(BarHit::Tab(i)) => { self.active_tab = i; self.redraw(); }
                        Some(BarHit::Close(i)) => self.close_tab(el, i),
                        Some(BarHit::Plus) => self.new_tab(),
                        None => self.bar_press(),
                    }
                    return;
                }
                if self.on_sep() {
                    let (now, pos) = (Instant::now(), self.mouse);
                    let tol = 5.0 * self.g.as_ref().unwrap().scale;
                    if is_double_click(self.last_sep_click, now, pos, tol) {
                        self.last_sep_click = None;
                        self.tabs[self.active_tab].split = 0.5;
                        self.sync_size(false);
                        self.redraw();
                    } else { self.last_sep_click = Some((now, pos)); self.drag_sep = true; }
                    return;
                }
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
    let side = sb::Side { col_focus: false, procs_open: false, psel: 0, psort: procs::Sort::Cpu, pquery: String::new(), pstop: None, pnope: false, pchanged: false, mon: procs::Mon::start(), agents_open: false, agents_sel: 0, stop_at: None, visible: true, focus: false, searching: false, query: String::new(), filter: sb::Filter::All,
        archived: vec![], open_archived: false, installed: sb::LAUNCH.map(|(_, b)| sb::on_path(b)) };
    let mut app = App { g: None, tabs: vec![], active_tab: 0, next_id: 0, proxy: el.create_proxy(), last_bar_click: None, last_sep_click: None, ticks: 0, restore: None, picking: false, mq: (None, Instant::now()),
        mods: ModifiersState::empty(), mouse: (0.0, 0.0), scroll_acc: 0.0, drag_sep: false, mouse_fwd: false, side };
    // Reabre las pestañas guardadas (cwd existente); sin archivo, o con MI_TERMINAL_CMD, una pestaña en HOME.
    let saved = tabs_file().filter(|_| std::env::var_os("MI_TERMINAL_CMD").is_none())
        .and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse_tabs(&t)).unwrap_or_default();
    let dirs: Vec<_> = saved.into_iter().map(|(d, id)| (std::path::PathBuf::from(d), id)).filter(|(d, _)| std::fs::read_dir(d).is_ok()).collect();
    if dirs.is_empty() { app.new_tab(); } else { for (d, id) in dirs {
        // Viva fuera de la app (otra terminal): solo el cwd, sin --resume.
        match id.filter(|i| !alive_now(i)) { Some(i) => app.open_resume(&i, d), None => app.new_tab_with(None, Some(d)) }
    } }
    // Solo para pruebas: escribe este comando al PTY al inicio.
    if let Ok(c) = std::env::var("MI_TERMINAL_CMD") { app.pane().session.send(format!("{c}\n").as_bytes()); }
    el.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn carpeta_elegida() {
        let p = |o: &str, ok, e: &str| parse_folder_output(o, ok, e).map(|p| p.to_string_lossy().into_owned());
        assert_eq!(p("/Users/x/Proyecto/\n", true, ""), Some("/Users/x/Proyecto".into()));
        assert_eq!(p("/Users/x/Mi Carpeta /\n", true, ""), Some("/Users/x/Mi Carpeta ".into()));
        assert_eq!(p("/Users/x/Música y vídeos/\n", true, ""), Some("/Users/x/Música y vídeos".into()));
        assert_eq!(p("/\n", true, ""), Some("/".into()));
        assert_eq!(p("", true, ""), None);
        assert_eq!(p("\n", true, ""), None);
        assert_eq!(p("", false, "execution error: User canceled. (-128)"), None);
        assert_eq!(p("/tmp/\n", true, "execution error: User canceled. (-128)"), None);
    }
    #[test]
    fn doble_clic() {
        let t = Instant::now();
        let ms = std::time::Duration::from_millis;
        assert!(is_double_click(Some((t, (10.0, 10.0))), t + ms(200), (12.0, 11.0), 5.0));
        assert!(!is_double_click(Some((t, (10.0, 10.0))), t + ms(500), (10.0, 10.0), 5.0));
        assert!(!is_double_click(Some((t, (10.0, 10.0))), t + ms(100), (30.0, 10.0), 5.0));
        assert!(!is_double_click(None, t, (10.0, 10.0), 5.0));
    }
    #[test]
    fn alternar_maximizar() {
        assert_eq!(toggle_action(true, true, true), TogAct::ExitFullscreen);
        assert_eq!(toggle_action(false, true, false), TogAct::Restore);
        assert_eq!(toggle_action(false, false, true), TogAct::Restore);
        assert_eq!(toggle_action(false, false, false), TogAct::Maximize);
    }
    #[test]
    fn accion_doble_clic() {
        assert_eq!(parse_dc_action("Minimize\n"), DcAction::Minimize);
        assert_eq!(parse_dc_action("Maximize\n"), DcAction::Maximize);
        assert_eq!(parse_dc_action("None\n"), DcAction::Nothing);
        assert_eq!(parse_dc_action(""), DcAction::Maximize);
    }
    fn sesiones(pid: u32, name_pid: u32, id: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mt-sess-{}-{pid}-{name_pid}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{name_pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{id}"}}"#)).unwrap();
        d
    }
    #[test]
    fn session_alive_pid_propio_vivo() {
        let me = std::process::id();
        assert!(session_alive(&sesiones(me, me, "abc"), "abc"));
    }
    #[test]
    fn session_alive_pid_inexistente_no_vivo() {
        assert!(!session_alive(&sesiones(2_000_000_000, 2_000_000_000, "abc"), "abc"));
    }
    #[test]
    fn session_alive_otro_id_o_pid_distinto_al_archivo_no_vivo() {
        let me = std::process::id();
        assert!(!session_alive(&sesiones(me, me, "abc"), "otro"));
        assert!(!session_alive(&sesiones(me, 1234, "abc"), "abc"));
    }
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
    fn rueda_acumula_fracciones() {
        let mut acc = 0.0;
        assert_eq!(take_lines(&mut acc, 0.4), 0);
        assert_eq!(take_lines(&mut acc, 0.4), 0);
        assert_eq!(take_lines(&mut acc, 0.4), 1); // 1.2 -> 1, queda 0.2
        assert_eq!(take_lines(&mut acc, -2.5), -2); // -2.3 -> -2, queda -0.3
        assert!((acc + 0.3).abs() < 1e-5);
    }
    #[test]
    fn rueda_en_pantalla_alterna() {
        let alt = TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(wheel_bytes(3, (0, 0), TermMode::empty()), None); // pantalla normal: historial local
        assert_eq!(wheel_bytes(0, (0, 0), alt), None);
        assert_eq!(wheel_bytes(2, (0, 0), alt), Some(b"\x1b[A\x1b[A".to_vec()));
        assert_eq!(wheel_bytes(-1, (0, 0), alt | TermMode::APP_CURSOR), Some(b"\x1bOB".to_vec()));
        assert_eq!(wheel_bytes(1, (4, 9), alt | TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE), Some(b"\x1b[<64;5;10M".to_vec()));
        assert_eq!(wheel_bytes(-1, (0, 0), TermMode::ALT_SCREEN), None); // sin ALTERNATE_SCROLL ni ratón
    }
    #[test]
    fn pestanas_guardadas() {
        let id = "0a1b2c3d-1111-4222-8333-444455556666";
        let n = |c: &str| (c.to_string(), None);
        let t = tabs_to_text(&[n("/a/b"), n(""), n("/x\ny"), n("/c d"), ("/e".into(), Some(id.into())), ("/f".into(), Some("x;rm".into())), ("/g".into(), Some(format!("{id}\nz")))]);
        assert_eq!(t, format!("/a/b\n/c d\n/e\t{id}\n/f\n/g\n"));
        assert_eq!(parse_tabs(&t), [n("/a/b"), n("/c d"), ("/e".into(), Some(id.into())), n("/f"), n("/g")]);
        assert_eq!(parse_tabs("rel\n\n/ok\n"), [n("/ok")]);
        assert_eq!(parse_tabs("/h\t1;rm -rf\n/i\t\n"), [n("/h"), n("/i")]); // id inválido se descarta
        assert_eq!(resume_cmd(id).unwrap()[4], id);
        assert!(resume_cmd("x;rm").is_none());
        let many: String = (0..12).map(|i| format!("/d{i}\n")).collect();
        assert_eq!(parse_tabs(&many).len(), 8);
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
