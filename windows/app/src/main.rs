//! Ónix: ventana winit + IOSurface con texto en CPU (cosmic-text) sobre term-core.
mod glyphs;
mod activity;
mod marquee;
mod palette;
mod procs;
mod render;
mod sidebar;
mod surface;
mod win;
mod usage_view;

use cosmic_text::{fontdb, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use render::{CustomGlyph, TextArea, TextBounds};
use sidebar as sb;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use term_core::alacritty_terminal::grid::{Dimensions, Scroll};
use term_core::alacritty_terminal::index::{Column, Line, Point, Side};
use term_core::alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use term_core::alacritty_terminal::term::cell::Flags;
use term_core::alacritty_terminal::term::{Term, TermMode};
use term_core::alacritty_terminal::event::EventListener;
use term_core::alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor};
use term_core::{click_move_bytes, dim, key_bytes, FG, paste_bytes, to_color, Key as K, Mods, Rgb, Session, BG};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};

const FONT_SIZE: f32 = 13.0;
const FONT_RANGE: (f32, f32) = (9.0, 28.0);
const PAD: f32 = 8.0;
/// Aire extra bajo la última fila, en px lógicos: sin él la línea de estado queda pegada al borde.
const BOTTOM_SPACE: f32 = 10.0;
/// Franja reservada arriba para la barra de pestañas (parte 2), en px lógicos.
const TABBAR_H: f32 = 36.0;
const TABBAR_BG: Rgb = [0x08, 0x08, 0x0a];
const TAB_ACTIVE: Rgb = [0x16, 0x16, 0x1a];
const TAB_HOVER: Rgb = [0x10, 0x10, 0x13];
/// Fondo con el que Claude Code pinta tus mensajes, y cómo se ven aquí.
const USER_BG_IN: Rgb = [0x37, 0x37, 0x37];
const USER_BG: Rgb = [0x17, 0x19, 0x1f];
const USER_BAR: Rgb = [0x5a, 0x8d, 0xee];
const INPUT_BG: Rgb = [0x0f, 0x0f, 0x12];
/// Radio (px lógicos) de botones, filas y pestañas.
const RADIUS: f32 = 7.0;
const SEP_W: f32 = 1.0;
const SEP_COLOR: Rgb = [0x26, 0x26, 0x2c];
const FOCUS_LINE: Rgb = [0x4a, 0x4a, 0x56];
const SIDE_BG: Rgb = [0x09, 0x09, 0x0b];
const BTN_BG: Rgb = [0x16, 0x16, 0x1a];
const TITLE_CH: usize = 24;
const CHIP: &str = "Jump to bottom (click) ↓";
enum Ev { Out(u64), Exit(u64), Tick, Folder(Option<std::path::PathBuf>), Usage }

/// Salida de `osascript choose folder` -> carpeta elegida; None si se canceló, falló o vino vacía.
fn parse_folder_output(stdout: &str, status_ok: bool, stderr: &str) -> Option<std::path::PathBuf> {
    if !status_ok || stderr.contains("-128") || stderr.contains("User canceled") { return None; }
    let s = stdout.strip_suffix('\n').unwrap_or(stdout);
    let s = if s.len() > 1 { s.trim_end_matches('/') } else { s };
    let s = if s.is_empty() && stdout.starts_with('/') { "/" } else { s };
    (!s.is_empty()).then(|| std::path::PathBuf::from(s))
}

/// Selector de carpeta de Windows; la carpeta inicial va por variable de entorno, nunca interpolada.
fn choose_folder(start: &str) -> Option<std::path::PathBuf> {
    let out = win::powershell("Add-Type -AssemblyName System.Windows.Forms; [Console]::OutputEncoding = [Text.Encoding]::UTF8; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.SelectedPath = $env:ONIX_DIR; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }")
        .env("ONIX_DIR", start.replace('/', "\\")).output().ok()?;
    let s = win::slashes(String::from_utf8_lossy(&out.stdout).trim());
    (!s.is_empty()).then(|| s.into())
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

/// Baja la grilla para que abajo no quede relleno; el sobrante de filas enteras pasa arriba.
fn bottom_dy(h: f32, rows: usize, ch: f32, pad: f32) -> f32 { (h - rows as f32 * ch - pad).max(0.0) }

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
    tabs.iter().filter(|(c, _)| !c.is_empty() && !c.contains(['\n', '\t'])).map(|(c, id)| match id.as_deref().filter(|i| sb::valid_token(i)) {
        Some(i) => format!("{c}\t{i}\n"), None => format!("{c}\n"),
    }).collect()
}

/// Texto -> (cwd, id de Claude) a restaurar: solo rutas absolutas, id opcional validado, máx 8. Acepta el formato viejo de una columna.
fn parse_tabs(text: &str) -> Vec<(String, Option<String>)> {
    text.lines().filter(|l| sb::is_abs(l)).take(8).map(|l| {
        let (c, id) = l.split_once('\t').unwrap_or((l, ""));
        (c.to_string(), Some(id).filter(|i| sb::valid_token(i)).map(String::from))
    }).collect()
}

/// Comando para retomar una sesión. Claude: sesión exacta (el id va como $1, sin interpolar). Los demás no dicen qué sesión
/// es de cada pestaña: con `last` continúan la última; sin él, Codex muestra su selector y el resto empieza limpio.
/// Grok y Junie siempre empiezan limpios (no instalados: no se pudo comprobar cómo retoman).
/// Un comando de agente dentro de PowerShell (resuelve los `.cmd` de npm).
fn shell_cmd(line: String) -> Vec<String> { ["powershell", "-NoLogo", "-Command"].map(String::from).into_iter().chain([line]).collect() }

fn resume_cmd(token: &str, last: bool) -> Option<Vec<String>> {
    let line = match token {
        "codex" => if last { "exec codex resume --last" } else { "exec codex resume" },
        "agy" => if last { "exec agy --continue" } else { "exec agy" },
        "opencode" => if last { "exec opencode --continue" } else { "exec opencode" },
        "grok" => "exec grok",
        "junie" => "exec junie",
        id if sb::is_uuid(id) => return Some(shell_cmd(format!("claude --resume {id}"))),
        _ => return None,
    };
    Some(shell_cmd(line.trim_start_matches("exec ").to_string()))
}

/// sessionId de Claude Code del proceso `pid` según `~/.claude/sessions/<pid>.json` (solo lectura); exige que el pid coincida.
/// Lo que Claude Code dice de sí mismo en su archivo de sesión: (pregunta, trabaja). `None` si no lo dice.
fn claude_state(json: &str) -> Option<(bool, bool)> {
    match serde_json::from_str::<serde_json::Value>(json).ok()?.get("status")?.as_str()? {
        "waiting" => Some((true, false)), "busy" => Some((false, true)), "idle" => Some((false, false)), _ => None,
    }
}

fn claude_status(pid: i32) -> Option<(bool, bool)> {
    let f = std::path::Path::new(&std::env::var_os("HOME")?).join(format!(".claude/sessions/{pid}.json"));
    claude_state(&std::fs::read_to_string(f).ok()?)
}

fn claude_session(pid: i32) -> Option<String> {
    let f = std::path::Path::new(&std::env::var_os("HOME")?).join(format!(".claude/sessions/{pid}.json"));
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
    let id = v.get("sessionId")?.as_str()?;
    (v.get("pid")?.as_i64()? == pid as i64 && sb::is_uuid(id)).then(|| id.to_string())
}

/// El chat dormido es este `sh`: espera un Enter (lo manda la app al volver a la pestaña) y retoma. $@ = comando de `resume_cmd`.
const SLEEP_PS: &str = "Write-Host '\n  Chat dormido para liberar memoria. Pulsa Enter para retomarlo.'; $null = Read-Host; ";
const SLEEP_AFTER: u64 = 15 * 60;

/// ¿La pantalla es Claude Code esperando con el cuadro vacío? `rows` sin el texto atenuado (sugerencias).
/// Un diálogo de permiso quita el pie y un borrador deja texto tras `❯`: en ambos casos no se duerme.
fn idle_prompt(rows: &[String]) -> bool {
    let footer = rows.iter().any(|r| r.contains(" mode on") || r.contains(" edits on") || r.contains("? for shortcuts"));
    footer && rows.windows(2).any(|w| w[0].trim() == "❯" && w[1].starts_with('─'))
}

fn screen_rows(s: &Session) -> Vec<String> {
    let t = s.term.lock().unwrap();
    (0..t.screen_lines()).map(|l| (0..t.columns()).map(|c| {
        let cell = &t.grid()[Line(l as i32)][Column(c)];
        if cell.flags.contains(Flags::DIM) { ' ' } else { cell.c }
    }).collect()).collect()
}

/// Salida de `ps -axo ppid=,comm=`: ¿`pid` tiene hijos que no sean `caffeinate`? (tareas en segundo plano, servidores MCP).
fn busy_children(ps: &str, pid: i32) -> bool {
    ps.lines().filter_map(|l| l.trim().split_once(' ')).any(|(p, c)| p.parse() == Ok(pid) && !c.trim().ends_with("caffeinate"))
}

/// Solo se duerme lo que luego se puede retomar: la conversación tiene que estar guardada.
fn has_transcript(id: &str) -> bool {
    let Some(h) = std::env::var_os("HOME") else { return false };
    std::fs::read_dir(std::path::Path::new(&h).join(".claude/projects")).is_ok_and(|d| d.flatten().any(|p| p.path().join(format!("{id}.jsonl")).is_file()))
}

use win::kill;

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

/// Archivo propio de la app; con MI_TERMINAL_CMD (modo prueba) no se lee ni se escribe.
fn data_file(name: &str) -> Option<std::path::PathBuf> {
    if std::env::var_os("MI_TERMINAL_CMD").is_some() { return None; }
    std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join("AppData/Roaming/Onix").join(name))
}

/// Escribe en un temporal y renombra: un cierre a medias no deja el archivo cortado.
fn save_file(name: &str, text: &str) {
    let Some(f) = data_file(name) else { return };
    if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    let tmp = f.with_extension("tmp");
    if std::fs::write(&tmp, text).is_ok() { let _ = std::fs::rename(&tmp, &f); }
}

/// Qué hay bajo el ratón en la barra de pestañas.
#[derive(Debug, PartialEq, Clone, Copy)]
enum BarHit { Tab(usize), Close(usize), Plus }

const BAR_LEFT: f32 = 8.0; // semáforos
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

/// Botón "Usage" del encabezado de Agents (a la izquierda de la ×).
fn usage_btn(r: R, sc: f32) -> R { (r.0 + r.2 - 100.0 * sc, r.1 + 6.0 * sc, 62.0 * sc, 28.0 * sc) }

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

fn g_w(g: &Gfx) -> f32 { g.w as f32 }

fn debug_on() -> bool { std::env::var("MI_TERMINAL_DEBUG").is_ok_and(|v| v == "1") }

/// (texto, color, negrita, ancha): las celdas anchas (CJK, emoji) van en su propio tramo.
type Span = (String, Rgb, bool, bool);

struct Gfx {
    window: Arc<Window>, screen: surface::Screen, w: u32, h: u32, fs: FontSystem, sc: SwashCache, frame: render::Frame,
    /// El compositor aún usaba la superficie libre: hay que repetir el frame.
    busy: bool, chip: Buffer, icons: Vec<Buffer>, side: Vec<(String, Buffer)>,
    /// Textos de la pantalla Agent usage: (texto, tamaño, negrita, buffer).
    ov: Vec<(String, f32, bool, Buffer)>, scale: f32, cell: (f32, f32), font: f32,
    /// Barra de búsqueda abierta: (panel, su texto).
    find_bar: Option<(u64, Buffer)>,
    /// Paleta de comandos abierta: (filas de texto, la resaltada).
    pal: Option<(Vec<Buffer>, usize)>,
}

/// Todo lo que cambia el dibujo de un panel. Si algo nuevo influye en cómo se ve, va aquí.
#[derive(PartialEq)]
struct PaneKey {
    id: u64, version: u64, size: (usize, usize), rect: R, dy: f32, cell: (f32, f32), scale: f32,
    offset: usize, gap: usize, solid: bool, selection: Option<SelectionRange>, link: Option<(Point, usize)>,
}

/// Un panel ya dibujado.
struct Drawn { key: PaneKey, rects: Vec<Inst> }

/// Una celda lista para pintar: columna, ancho, carácter, color, fondo y negrita.
type Cell = (usize, usize, char, Rgb, Rgb, bool);

/// Un renglón ya dibujado: vale mientras no cambien sus celdas ni dónde cae (rect, bajada, celda y escala).
struct Row { at: (R, f32, (f32, f32), f32), cells: Vec<Cell>, spans: Vec<Span>, rects: Vec<Inst>, text: std::cell::OnceCell<Vec<render::Op>> }

struct Pane {
    id: u64, session: Session, anchor: Option<Point>, bufs: Vec<Buffer>, done_rows: Vec<Option<Row>>,
    chip_rect: Option<[f32; 4]>, cols: usize, rows: usize, rect: R, dy: f32, merged: Option<usize>, gap: usize, cleared: bool, resized: bool, working: Option<Instant>, asking: bool, done: bool, drawn: Option<Drawn>, picked: Option<(Point, Point)>, asleep: bool, last_busy: Instant,
    born: Instant, last_out: Instant, unread: bool, agent: sb::Agent, had_agent: bool, project: String,
    token: Option<String>, dir: String, cwd: String,
    /// % de contexto que muestra su pie (Claude Code); se conserva si deja de verse.
    ctx: Option<u8>,
}

impl Pane {
    fn has(&self, x: f32, y: f32) -> bool {
        x >= self.rect.0 && x < self.rect.0 + self.rect.2 && y >= self.rect.1 && y < self.rect.1 + self.rect.3
    }
    /// Ajusta el panel a su rect: rehace buffers de fila y redimensiona el PTY solo si cambió algo.
    fn fit(&mut self, g: &mut Gfx, rect: R, force: bool) {
        self.rect = rect;
        let inner = (rect.0, rect.1, rect.2, rect.3 - BOTTOM_SPACE * g.scale);
        let (c, r) = pty_size(inner, g.cell, PAD * g.scale);
        if force || (c, r) != (self.cols, self.rows) {
            (self.cols, self.rows) = (c, r);
            self.bufs = (0..r).map(|_| g.new_buf()).collect();
            self.done_rows = (0..r).map(|_| None).collect();
            self.drawn = None;
        }
        self.dy = bottom_dy(inner.3, r, g.cell.1, PAD * g.scale);
        let cur = { let t = self.session.term.lock().unwrap(); (t.columns(), t.screen_lines()) };
        if (c, r) != cur { self.session.resize(c, r); self.drawn = None; }
        self.resized |= c != cur.0;
    }
    /// Agente en primer plano y proyecto (cwd) vía libproc.
    fn refresh(&mut self) {
        let Some(pid) = win::fg_pid(&self.session).or(self.session.child_pid.map(|p| p as i32)) else { return };
        let (name, path, cwd) = sb::proc_info(pid);
        // Proceso ya muerto: se conserva lo último visto para poder archivarlo.
        if name.is_empty() { return; }
        self.agent = sb::agent_from(&name, &path);
        if name == "node" || path.ends_with("/node") { self.agent = sb::agent_from_args(&sb::proc_args(pid)); }
        self.had_agent |= self.agent != sb::Agent::Shell;
        match self.agent {
            sb::Agent::Claude => self.token = claude_session(pid).or_else(|| self.token.take().filter(|t| sb::is_uuid(t))),
            // Al retomar, el `zsh` previo al agente no borra el token; salir del agente sí.
            sb::Agent::Shell => if self.had_agent { self.token = None },
            a => self.token = sb::LAUNCH.iter().find(|(g, _)| *g == a).map(|(_, b)| b.to_string()),
        }
        if !cwd.is_empty() {
            self.dir = cwd.clone();
            let home = std::env::var("HOME").unwrap_or_default();
            self.project = sb::project_of(&cwd, &home);
            self.cwd = sb::tilde(&cwd, &home);
        }
    }
    fn point(&self, mouse: (f32, f32), cell: (f32, f32), pad: f32, off: usize) -> Point {
        let (c, r) = mouse_cell((mouse.0, mouse.1 - self.dy), self.rect, cell, pad, self.cols, self.rows);
        Point::new(Line(unmerge(r, self.merged) as i32 - (off + self.gap) as i32), Column(c))
    }
}

/// Ruta bajo el ratón, para subrayarla: panel, celda inicial y sus caracteres (si cambian, no se marca).
struct Link { pane: u64, start: Point, text: Vec<char> }

impl Link {
    fn covers(&self, at: Point, c: char, cols: usize) -> bool {
        let i = (at.line.0 - self.start.line.0) as isize * cols as isize + at.column.0 as isize - self.start.column.0 as isize;
        i >= 0 && self.text.get(i as usize) == Some(&c)
    }
}

struct Tab { panes: Vec<Pane>, active: usize, split: f32, tbuf: Buffer, tprev: String, ttitle: String, tfull: f32 }

impl Tab {
    /// Título de la pestaña: la ruta del panel activo (`~/Dev/x`), nunca "zsh" ni el título OSC.
    /// Con agente: `~/Dev · claude` (el ícono va aparte, a la derecha).
    fn title(&self) -> String {
        let p = &self.panes[self.active];
        // Con agente: el nombre de la sesión (título OSC); si aún no hay, `~/Dev · claude`.
        let t = p.session.title.lock().unwrap().clone();
        let t = sb::clean_title(&t, 200); // sin cortar: el carrusel de la pestaña muestra el nombre completo
        match p.agent.name() { Some(_) if !t.is_empty() => format!("{} · {t}", p.cwd), Some(n) => format!("{} · {n}", p.cwd), None => p.cwd.clone() }
    }
}

impl Gfx {
    fn metrics(&self) -> Metrics { Metrics::new(self.font * self.scale, (self.font * self.scale * 1.2).round()) }
    fn bar_metrics(&self) -> Metrics { Metrics::new(11.0 * self.scale, (14.0 * self.scale).round()) }
    fn new_buf(&mut self) -> Buffer {
        let m = self.metrics();
        let mut b = Buffer::new(&mut self.fs, m);
        b.set_size(Some(self.w as f32), Some(self.cell.1));
        b.set_wrap(Wrap::None);
        b
    }
    /// Recalcula el tamaño de celda y el chip; los paneles se reajustan con `Pane::fit`.
    fn relayout(&mut self) {
        self.scale = self.window.scale_factor() as f32;
        let m = self.metrics();
        let mut probe = Buffer::new(&mut self.fs, m);
        probe.set_size(Some(1000.0), Some(m.line_height));
        probe.set_text("M", &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
        probe.shape_until_scroll(&mut self.fs, false);
        let w = probe.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(m.font_size * 0.6);
        self.cell = (w, m.line_height);
        // Chip "Jump to bottom": se crea una vez por relayout, no por frame.
        let mut b = Buffer::new(&mut self.fs, m);
        b.set_size(Some((CHIP.chars().count() + 2) as f32 * w), Some(self.cell.1));
        b.set_text(CHIP, &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
        b.shape_until_scroll(&mut self.fs, false);
        self.chip = b;
        // Glifos sueltos de la barra de pestañas: vacío (para los íconos de línea), + y x.
        let bm = self.bar_metrics();
        self.icons = ["", "+", "×"].iter().map(|t| {
            let mut b = Buffer::new(&mut self.fs, bm);
            b.set_size(Some(40.0 * self.scale), Some(bm.line_height));
            b.set_text(t, &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
            b.shape_until_scroll(&mut self.fs, false);
            b
        }).collect();
    }
}

/// Filas vacías bajo lo último escrito (y bajo el cursor) que se pueden rellenar bajando la vista con historial.
/// Claude Code las deja al encogerse (cerrar un diálogo): así el prompt se ve siempre abajo.
fn bottom_gap<L: EventListener>(t: &Term<L>) -> usize {
    let (g, bg) = (t.grid(), AColor::Named(NamedColor::Background));
    let used = |l: &i32| (0..t.columns()).any(|c| { let c = &g[Line(*l)][Column(c)]; c.c != ' ' || c.bg != bg || c.flags.contains(Flags::INVERSE) });
    let rows = t.screen_lines() as i32;
    let cursor = g.cursor.point.line.0;
    let last = (cursor + 1..rows).rev().find(used).unwrap_or(cursor);
    ((rows - 1 - last).max(0) as usize).min(g.history_size())
}

/// Cuánto más arriba del desplazamiento del usuario se pinta. Vale también al subir: así el hueco no reaparece.
fn view_gap<L: EventListener>(t: &Term<L>) -> usize { bottom_gap(t).min(t.grid().history_size() - t.grid().display_offset()) }

/// Más de media pantalla vacía es una limpieza (/clear), no un diálogo que se cierra:
/// no se rellena hasta que lo nuevo vuelva a llegar abajo.
fn fill_gap<L: EventListener>(t: &Term<L>, cleared: &mut bool) -> usize {
    let gap = bottom_gap(t);
    if gap * 2 > t.screen_lines() { *cleared = true; } else if gap == 0 { *cleared = false; }
    if *cleared { 0 } else { view_gap(t) }
}

/// Tras `/clear` Claude Code deja arriba lo que había antes de su cabecera y no pide borrar el historial: aquí la
/// cabecera sube a la primera fila y se borra el historial. Solo con la cabecera y el `/clear` a la vista.
fn forget_on_clear<L: EventListener>(t: &mut Term<L>) {
    if t.mode().contains(TermMode::ALT_SCREEN) { return; }
    let row = |l: i32| (0..t.columns()).map(|c| t.grid()[Line(l)][Column(c)].c).collect::<String>();
    let rows = t.screen_lines() as i32;
    let Some(clear) = (1..rows).rev().find(|&l| row(l).trim_end() == "❯ /clear") else { return };
    let Some(head) = (0..clear).rev().find(|&l| row(l).contains("Claude Code v")) else { return };
    if head > 0 {
        // Claude mueve el cursor en relativo: sube junto con el texto.
        if t.grid().cursor.point.line.0 < head { return; }
        let g = t.grid_mut();
        g.scroll_up(&(Line(0)..Line(rows)), head as usize);
        g.cursor.point.line = g.cursor.point.line - head;
    }
    t.grid_mut().clear_history();
}

/// Al cambiar el ancho, las líneas se reacomodan y Claude Code repinta desde arriba sin borrar el historial: queda
/// una copia vieja. Si su cabecera vuelve a la primera fila, la charla entera está en pantalla y el historial sobra.
/// En una charla larga la cabecera no está y no se toca nada.
fn forget_after_resize<L: EventListener>(t: &mut Term<L>, resized: &mut bool) {
    if !*resized || t.mode().contains(TermMode::ALT_SCREEN) || t.grid().history_size() == 0 { return; }
    let first: String = (0..t.columns()).map(|c| t.grid()[Line(0)][Column(c)].c).collect();
    if first.contains("Claude Code v") {
        t.grid_mut().clear_history();
        *resized = false;
    }
}

/// ⌘A: selecciona solo lo escrito tras el `❯` (cuadro de Claude Code o línea del shell), para copiarlo con ⌘C.
/// Sin ese prompt junto al cursor, vacío o con la sugerencia atenuada, no selecciona nada.
fn select_input<L: EventListener>(t: &mut Term<L>) {
    t.selection = None;
    let g = t.grid();
    let (cur, last_col) = (g.cursor.point.line.0, t.last_column());
    let first = |l: i32| g[Line(l)][Column(0)].c;
    let Some(start) = (0..=cur).rev().find(|&l| first(l) == '❯') else { return };
    let boxed = start > 0 && first(start - 1) == '─';
    let mut end = start;
    while end + 1 < t.screen_lines() as i32
        && if boxed { first(end + 1) != '─' } else { g[Line(end)][last_col].flags.contains(Flags::WRAPLINE) } { end += 1; }
    if cur > end || g[Line(start)][Column(2)].flags.contains(Flags::DIM) { return; }
    let written = |l: i32| (if l == start { 2 } else { 0 }..t.columns()).rev().find(|&c| !g[Line(l)][Column(c)].c.is_whitespace());
    let Some((line, col)) = (start..=end).rev().find_map(|l| Some((l, written(l)?))) else { return };
    let mut s = Selection::new(SelectionType::Simple, Point::new(Line(start), Column(2)), Side::Left);
    s.update(Point::new(Line(line), Column(col)), Side::Right);
    t.selection = Some(s);
}

/// Texto a copiar. Si es lo escrito tras `❯`, sin la sangría que Claude Code pone en las líneas siguientes.
fn selected_text<L: EventListener>(t: &Term<L>) -> Option<String> {
    let text = t.selection_to_string()?;
    let from = t.selection.as_ref()?.to_range(t)?.start;
    let input = from.column.0 == 2 && t.grid()[from.line][Column(0)].c == '❯';
    Some(if input { text.replace("\n  ", "\n") } else { text })
}

/// Filas de Processes (`k` = 0) o Ports (`k` = 1).
fn proc_rows(side: &sb::Side, k: usize) -> Vec<procs::Row> {
    let s = side.mon.snap.lock().unwrap();
    let q = &side.lists[k].query;
    if k == 1 { procs::port_rows(&s.procs, &s.listen, q) } else { procs::rows(&s.procs, &s.ports, side.psort, q) }
}

fn local_url(r: &procs::Row) -> String { format!("http://localhost:{}", r.port.unwrap_or(0)) }

/// Marca desde cuándo trabaja un agente; `true` cuando deja de hacerlo tras al menos 10 s (menos es ruido).
fn finished(since: &mut Option<Instant>, working: bool, now: Instant) -> bool {
    if working { since.get_or_insert(now); false } else { since.take().is_some_and(|t| now.duration_since(t).as_secs() >= 10) }
}

/// Globo de aviso de Windows; los textos van por variables de entorno, nunca dentro del script.
fn notify(title: &str, body: &str) {
    const TOAST: &str = "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; $n = New-Object System.Windows.Forms.NotifyIcon; $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; $n.ShowBalloonTip(5000, $env:ONIX_TITLE, $env:ONIX_BODY, 'Info'); Start-Sleep 6; $n.Dispose()";
    if let Ok(mut c) = win::powershell(TOAST).env("ONIX_TITLE", title).env("ONIX_BODY", body).spawn() {
        std::thread::spawn(move || { let _ = c.wait(); });
    }
}

/// Extremos de la selección, para saber si sigue siendo la que hizo ⌘A.
fn span<L: EventListener>(t: &Term<L>) -> Option<(Point, Point)> { t.selection.as_ref()?.to_range(t).map(|r| (r.start, r.end)) }

/// Lleva el cursor al final y borra hacia atrás `n` caracteres (con margen: lo que sobra no hace nada).
/// Supr no sirve: zsh sin configurar no la entiende.
fn erase_bytes(n: usize, app_cursor: bool) -> Vec<u8> {
    let right: &[u8] = if app_cursor { b"\x1bOC" } else { b"\x1b[C" };
    [right.repeat(n + 8), vec![0x7f; 2 * (n + 8)]].concat()
}

/// Una fila por panel de cada pestaña, con su estado actual.
fn entries(tabs: &[Tab], active_tab: usize) -> Vec<sb::Entry> {
    let now = Instant::now();
    tabs.iter().enumerate().flat_map(|(ti, t)| t.panes.iter().enumerate().map(move |(pi, p)| {
        let title = p.session.title.lock().unwrap().clone();
        sb::Entry {
            tab: ti, pane: pi, project: p.project.clone(), done: p.done, agent: p.agent, unread: p.unread, focused: ti == active_tab && pi == t.active,
            status: if p.asking { sb::Status::Asking } else if p.working.is_some() { sb::Status::Working } else { sb::status(p.agent, p.had_agent, now.duration_since(p.last_out), &title) },
            title: if p.agent == sb::Agent::Shell || title.is_empty() { p.cwd.clone() } else { sb::clean_title(&title, 200) },
            secs: now.duration_since(p.born).as_secs(),
        }
    })).collect()
}

/// Toda la pantalla viva del panel, con lo atenuado (los pies de los menús suelen estarlo).
fn live_rows<L: EventListener>(t: &Term<L>) -> Vec<String> {
    (0..t.screen_lines()).map(|l| (0..t.columns()).map(|c| t.grid()[Line(l as i32)][Column(c)].c).collect()).collect()
}

/// Texto que se oculta al dibujar (no se toca lo que llega del pty ni lo que se copia).
const HIDE_HINTS: [&str; 2] = [" (shift+tab to cycle)", " · esc to interrupt"];
/// Quita los `HIDE_HINTS` de una fila y corre a la izquierda lo que seguía (celdas: columna, ancho, carácter, …).
fn hide_hint<T: Copy>(row: &mut [(usize, usize, char, T, T, bool)]) {
    for hint in HIDE_HINTS { hide_text(row, hint); }
}
fn hide_text<T: Copy>(row: &mut [(usize, usize, char, T, T, bool)], hint: &str) {
    let h: Vec<char> = hint.chars().collect();
    if row.len() < h.len() { return; }
    let mut i = 0;
    while i + h.len() <= row.len() {
        if row[i..i + h.len()].iter().zip(&h).all(|(c, k)| c.2 == *k) {
            // Lo que venía después (p. ej. "· esc to interrupt") se corre a la izquierda: sin hueco.
            let n = row.len();
            for k in i..n {
                let src = k + h.len();
                if src < n { (row[k].2, row[k].3, row[k].5) = (row[src].2, row[src].3, row[src].5); } else { row[k].2 = ' '; }
            }
        } else { i += 1; }
    }
}

/// Solo en pantalla: la fila de arriba del prompt de Claude Code trae el nombre de la sesión al final
/// (`────── nombre ─`). Ese nombre ya va en la pestaña: aquí se tapa con la misma raya.
fn hide_label<T: Copy>(row: &mut [(usize, usize, char, T, T, bool)]) {
    let n = row.len();
    let s = row.iter().take_while(|c| c.2 == '─').count();
    if s < 3 || n < s + 4 { return; }
    let Some(last) = row.iter().rposition(|c| c.2 != ' ' ) else { return };
    if row[last].2 != '─' || last <= s + 2 || row[s].2 != ' ' || row[last - 1].2 != ' ' { return; }
    if row[s..last].iter().any(|c| c.2 == '─' || c.1 != 1) { return; }
    let (fg, bg, b) = (row[0].3, row[0].4, row[0].5);
    for c in &mut row[s..last] { (c.2, c.3, c.4, c.5) = ('─', fg, bg, b); }
}

/// Solo en pantalla: convierte el "ctx NN%" del inicio de la barra de estado en una barra fina en su misma fila
/// (si no cabe, solo la cifra). `col`: etiqueta, tramo vacío, y barra normal, desde 60 % y desde 80 %.
fn ctx_bar<T: Copy>(grid: &mut [Vec<(usize, usize, char, T, T, bool)>], col: [T; 5]) {
    for row in grid.iter_mut().rev() {
        let t: String = row.iter().map(|c| c.2).collect();
        let s = t.chars().take_while(|c| *c == ' ').count();
        let Some((p, after)) = ctx_of(&t) else { continue };
        let fill = col[match p { 80.. => 4, 60.. => 3, _ => 2 }];
        // 8 tramos; desde 1 % ya se ve uno.
        let n = (p.min(100) as usize * 8).div_ceil(100);
        let rest: Vec<(char, T, bool)> = row.iter().skip(t.chars().count() - after.chars().count()).map(|c| (c.2, c.3, c.5)).collect();
        let used = rest.iter().rposition(|c| c.0 != ' ').map_or(0, |i| i + 1);
        let chip = |bar: bool| -> Vec<(char, T, bool)> {
            "contexto ".chars().map(|c| (c, col[0], false))
                .chain((0..if bar { 8 } else { 0 }).map(|k| (if k < n { '━' } else { '─' }, if k < n { fill } else { col[1] }, false)))
                .chain(format!("{}{p}%", if bar { " " } else { "" }).chars().map(|c| (c, col[2], false))).collect()
        };
        let full = chip(true);
        let new = if s + full.len() + used <= row.len() { full } else { chip(false) };
        let mut cells = new.into_iter().chain(rest);
        for d in &mut row[s..] { (d.2, d.3, d.5) = cells.next().unwrap_or((' ', d.3, false)); }
        return;
    }
}

/// "ctx NN% resto" -> (NN, resto).
fn ctx_of(line: &str) -> Option<(u8, &str)> {
    let (num, after) = line.trim_start().strip_prefix("ctx ")?.split_once('%')?;
    Some((num.parse().ok()?, after))
}

/// ¿Fila del pie de Claude Code (modo, contexto o uso)? `.1`: es la que lleva la raya de arriba.
fn footer_row(line: &str) -> Option<bool> {
    let t = line.trim_start();
    let meter = t.starts_with("contexto ") || t.starts_with("5h ");
    let mode = (t.starts_with("▸▸ ") || t.starts_with("⏸ ")) && t.contains(" on");
    (meter || mode).then(|| meter || t.contains("contexto ") || t.contains("5h "))
}

/// Las dos rayas que rodean el cuadro de escritura de Claude Code: (fila de arriba, fila de abajo).
fn input_box<T>(grid: &[Vec<(usize, usize, char, T, T, bool)>], cols: usize) -> Option<(usize, usize)> {
    let mut rules = (0..grid.len()).rev().filter(|&r| grid[r].len() == cols && grid[r].iter().all(|c| c.2 == '─'));
    let (b, a) = (rules.next()?, rules.next()?);
    matches!(grid.get(a + 1)?.first()?.2, '❯' | '!').then_some((a, b))
}

/// Rect de esquinas redondeadas (radio `r`) hecho de franjas; `under` es el fondo, para suavizar el borde.
/// Con `bottom` en falso solo se redondean las de arriba.
fn round(out: &mut Vec<Inst>, (x, y, w, h): R, r: f32, c: Rgb, under: Rgb, bottom: bool) {
    let (x, y, w, h) = (x.round(), y.round(), w.round(), h.round());
    let r = r.min(w / 2.0).min(h / 2.0).floor();
    let n = r as usize;
    for i in 0..n {
        let d = r - i as f32 - 0.5;
        let inset = r - (r * r - d * d).sqrt();
        let (whole, part) = (inset.ceil(), inset.ceil() - inset);
        let edge = [0, 1, 2].map(|k| (under[k] as f32 + (c[k] as f32 - under[k] as f32) * part).round() as u8);
        for (on, ly) in [(true, y + i as f32), (bottom, y + h - 1.0 - i as f32)] {
            if !on { continue; }
            rect(out, x + whole, ly, w - 2.0 * whole, 1.0, c);
            if part > 0.0 { for ex in [x + whole - 1.0, x + w - whole] { rect(out, ex, ly, 1.0, 1.0, edge); } }
        }
    }
    rect(out, x, y + r, w, h - r - if bottom { r } else { 0.0 }, c);
}

/// Solo en pantalla: da a la línea de uso ("5h ━━── 50% …") los mismos colores que la barra de contexto, mande
/// lo que mande Claude Code (la atenúa entera). `col` como en `lift_context`. Solo en las últimas filas escritas: el pie.
fn paint_usage<T: Copy>(grid: &mut [Vec<(usize, usize, char, T, T, bool)>], col: [T; 5]) {
    let bar = |c: char| c == '━' || c == '─';
    for row in grid.iter_mut().rev().filter(|r| r.iter().any(|c| c.2 != ' ')).take(6) {
        let Some(from) = row.windows(4).position(|w| w[0].2 == '5' && w[1].2 == 'h' && w[2].2 == ' ' && bar(w[3].2)) else { continue };
        let text: Vec<char> = row.iter().map(|c| c.2).collect();
        let mut fill = col[2];
        let mut i = from;
        while i < row.len() {
            if bar(text[i]) {
                // El color del tramo lleno sale del porcentaje que sigue a la barra.
                let p: u32 = text[i..].iter().skip_while(|c| bar(**c) || **c == ' ').take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0);
                fill = col[match p { 80.. => 4, 60.. => 3, _ => 2 }];
                while i < row.len() && bar(text[i]) { row[i].3 = if text[i] == '━' { fill } else { col[1] }; i += 1; }
                continue;
            }
            let digits = text[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && text.get(i + digits) == Some(&'%') {
                for c in &mut row[i..=i + digits] { c.3 = col[2]; }
                i += digits + 1;
                continue;
            }
            row[i].3 = col[0];
            i += 1;
        }
    }
}

/// Fila vista -> fila real de la grilla cuando `merge_status` quitó la fila `gone`.
fn unmerge(row: usize, gone: Option<usize>) -> usize { if gone.is_some_and(|g| row <= g) { row.saturating_sub(1) } else { row } }

/// Solo en pantalla: si la fila de arriba es la barra de uso ("5h …") y la de abajo el modo de
/// Claude Code ("▸▸ auto mode on"), la barra se dibuja en esa fila pegada a la derecha (con el mismo
/// margen que el modo) y su fila se quita: lo de arriba baja una fila y la vacía queda en el borde
/// superior, así el pie sigue en el último renglón. Si no cabe, no se toca nada.
/// Devuelve la fila quitada (para bajar el cursor y corregir el ratón en lo de encima).
fn merge_status<T: Copy>(grid: &mut Vec<Vec<(usize, usize, char, T, T, bool)>>) -> Option<usize> {
    let text = |r: &[(usize, usize, char, T, T, bool)]| r.iter().map(|c| c.2).collect::<String>();
    let span = |r: &[(usize, usize, char, T, T, bool)]| {
        let s = r.iter().position(|c| c.2 != ' ')?;
        let e = r.iter().rposition(|c| c.2 != ' ')? + 1;
        Some((s, e))
    };
    for i in 1..grid.len() {
        let (up, down) = (text(&grid[i - 1]), text(&grid[i]));
        if !(up.trim_start().starts_with("5h ") || up.trim_start().starts_with("contexto ")) || !(down.contains(" mode on") || down.contains(" edits on")) { continue; }
        let (Some((s, e)), Some((m, d))) = (span(&grid[i - 1]), span(&grid[i])) else { continue };
        let n = grid[i].len();
        let Some(at) = n.checked_sub(e - s + m) else { continue };
        if at < d + 3 || grid[i - 1][s..e].iter().any(|c| c.1 != 1) { continue; }
        let moved: Vec<_> = grid[i - 1][s..e].to_vec();
        for (k, c) in moved.into_iter().enumerate() { let t = &mut grid[i][at + k]; (t.2, t.3, t.5) = (c.2, c.3, c.5); }
        let mut blank = grid.remove(i - 1);
        for (k, c) in blank.iter_mut().enumerate() { c.0 = k; c.2 = ' '; }
        grid.insert(0, blank);
        return Some(i - 1);
    }
    None
}

type Inst = (f32, f32, f32, f32, Rgb);
fn rect(out: &mut Vec<Inst>, x: f32, y: f32, w: f32, h: f32, c: Rgb) { out.push((x, y, w, h, c)); }

/// Ancho útil (px) del título de una pestaña de ancho `tw`.
fn title_avail(tw: f32, sc: f32) -> f32 { tw - (28.0 + CLOSE_S + 10.0) * sc }
/// Semáforo de un agente: (color, texto). Amarillo pide respuesta, blanco trabaja, verde terminó y no lo has visto.
fn light(e: &sb::Entry) -> Option<(Rgb, &'static str)> {
    match e.status {
        sb::Status::Asking => Some(([0xd9, 0xb0, 0x5a], "Needs you")),
        sb::Status::Working => Some(([0xd4, 0xd4, 0xd4], "Working")),
        _ => e.done.then_some(([0x5a, 0xc0, 0x6a], "Done")),
    }
}
/// Insignia del logo: (ícono, lado lógico, color). Cometa que orbita al trabajar, punto que late si te necesita, fijo al terminar.
fn badge(e: &sb::Entry, secs: f32) -> Option<(u16, f32, Rgb)> {
    let (c, _) = light(e)?;
    Some(match e.status {
        sb::Status::Working => (sb::ICON_SPIN + (secs * sb::SPIN_FRAMES as f32) as u16 % sb::SPIN_FRAMES, 10.0, c),
        sb::Status::Asking => (sb::ICON_DOT, 7.0, pulse(c, secs)),
        _ => (sb::ICON_DOT, 7.0, c),
    })
}
/// Latido de los puntos del semáforo: el brillo va y viene entre 25 % y 100 % cada 1,2 s.
fn pulse(c: Rgb, secs: f32) -> Rgb {
    let k = 0.625 + 0.375 * (secs * std::f32::consts::TAU / 1.2).cos();
    c.map(|v| (v as f32 * k).round() as u8)
}
/// Ancho (px) de `t` en la letra de la barra; se recuerda para no volver a formarlo en cada cuadro.
fn text_w(fs: &mut FontSystem, m: Metrics, t: &str) -> f32 {
    static SEEN: std::sync::Mutex<Option<std::collections::HashMap<(String, u32), f32>>> = std::sync::Mutex::new(None);
    let mut seen = SEEN.lock().unwrap();
    let seen = seen.get_or_insert_with(Default::default);
    if seen.len() > 2000 { seen.clear(); }
    *seen.entry((t.to_string(), m.font_size.to_bits())).or_insert_with(|| {
        let mut b = Buffer::new(fs, m);
        b.set_size(Some(1.0e4), Some(m.line_height));
        b.set_wrap(Wrap::None);
        b.set_text(t, &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
        b.shape_until_scroll(fs, false);
        b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    })
}
/// `t` recortado con "…" para que quepa en `avail` px.
fn text_cut(fs: &mut FontSystem, m: Metrics, t: &str, avail: f32) -> String {
    if text_w(fs, m, t) <= avail { return t.into(); }
    let mut c: Vec<char> = t.chars().take(200).collect();
    while c.pop().is_some() {
        let s = format!("{}…", c.iter().collect::<String>());
        if text_w(fs, m, &s) <= avail { return s; }
    }
    "…".into()
}
/// Ancho reservado (px lógicos) a la derecha del título para el ícono del agente.
const ICON_W: f32 = 28.0;
/// Lado (px lógicos) de todos los íconos y logos de la interfaz.
const CHROME_PX: f32 = 16.0;

/// Devuelve `true` si hay carrusel en marcha (pide otro frame). `mt`: segundos desde que el ratón entró a la pestaña.
/// Pantalla Agent usage ya armada: (área, rects, textos).
type Ovl = (R, Vec<(R, Rgb)>, Vec<usage_view::Txt>, Vec<usage_view::Logo>);

fn draw(g: &mut Gfx, tabs: &mut [Tab], active_tab: usize, mouse: (f32, f32), side: &sb::Side, mt: f32, ovl: Option<Ovl>, link: Option<&Link>) -> bool {
    let bm = g.bar_metrics();
    let tabs_len = tabs.len();
    let sz = (g.w as f32, g.h as f32);
    let left = bar_left(side.visible, g.scale);
    let (trs, plus) = bar_rects(tabs_len, sz.0, g.scale, left);
    let hover = bar_hit(mouse, tabs_len, sz.0, g.scale, left);
    let mut animating = false;
    let mut moff = vec![0.0f32; tabs_len];
    let mut center = vec![0.0f32; tabs_len];
    let mut avails = vec![0.0f32; tabs_len];
    let ents_all = entries(tabs, active_tab);
    for (i, t) in tabs.iter_mut().enumerate() {
        let title = t.title();
        let iw = if t.panes[t.active].agent.icon().is_some() { ICON_W * g.scale } else { 0.0 };
        let avail = (title_avail(trs[i].2, g.scale) - iw).max(0.0);
        avails[i] = avail;
        if t.ttitle != title {
            t.tfull = text_w(&mut g.fs, bm, &title);
            t.ttitle = title.clone();
            t.tprev.clear();
        }
        let long = t.tfull > avail;
        let hov = matches!(hover, Some(BarHit::Tab(j) | BarHit::Close(j)) if j == i);
        let want = if hov { title.clone() } else { text_cut(&mut g.fs, bm, &title, avail) };
        if t.tprev != want {
            t.tbuf.set_metrics(bm);
            t.tbuf.set_size(Some(1.0e4), Some(bm.line_height));
            t.tbuf.set_wrap(Wrap::None);
            t.tbuf.set_text(&want, &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
            t.tbuf.shape_until_scroll(&mut g.fs, false);
            t.tprev = want;
        }
        if long && hov { moff[i] = marquee::marquee_offset(mt, t.tfull - avail, 35.0 * g.scale, 1.0); animating = true; }
        // Hueco a la izquierda del grupo logo + título.
        center[i] = (trs[i].2 - iw - text_w(&mut g.fs, bm, &t.tprev).min(avail)) / 2.0;
    }
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    // El spinner y los puntos del semáforo piden cuadros seguidos.
    animating |= ents_all.iter().any(|e| light(e).is_some());
    let secs = START.get_or_init(Instant::now).elapsed().as_secs_f32();
    // Qué está haciendo cada Claude que trabaja, por (pestaña, panel).
    let doing: std::collections::HashMap<(usize, usize), String> = ents_all.iter().filter(|e| side.visible && e.agent == sb::Agent::Claude && e.status == sb::Status::Working)
        .filter_map(|e| Some(((e.tab, e.pane), activity::of(tabs[e.tab].panes[e.pane].token.as_deref()?)?))).collect();
    let used: std::collections::HashMap<(usize, usize), u8> = tabs.iter().enumerate().flat_map(|(ti, t)| t.panes.iter().enumerate().filter_map(move |(pi, p)| Some(((ti, pi), p.ctx?)))).collect();
    let tab = &mut tabs[active_tab];
    let (cw, ch) = g.cell;
    let pad = PAD * g.scale;
    let mut rects: Vec<Inst> = vec![];
    rect(&mut rects, 0.0, 0.0, sz.0, TABBAR_H * g.scale, TABBAR_BG);
    // Barra lateral: rects y textos (los buffers se reutilizan por índice, solo lo visible).
    let mut stxt: Vec<(String, f32, f32, Color, Option<(u16, f32)>)> = vec![];
    // Carrusel de la barra lateral: posición (x, y) del texto -> (izq, der, ¿hover?) del título recortado.
    let mut sclip: std::collections::HashMap<(u32, u32), (f32, f32, bool)> = std::collections::HashMap::new();
    static SH: std::sync::Mutex<Option<(usize, Instant)>> = std::sync::Mutex::new(None);
    let mut shov: Option<usize> = None;
    if side.visible {
        let sc = g.scale;
        let (all, wt, wk) = sb::counts(&ents_all);
        let ents = sb::filter(ents_all.clone(), side.filter, &side.query);
        let items = sb::group(&ents);
        let (dimc, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
        rect(&mut rects, 0.0, 0.0, sb::SIDE_W * sc, sz.1, SIDE_BG);
        rect(&mut rects, sb::SIDE_W * sc - 1.0, 0.0, 1.0, sz.1, SEP_COLOR);
        let lay = sb::layout(sz, sc, &items, if side.open_archived { side.archived.len().min(sb::ARCH_MAX) } else { 0 });
        for (r, hit) in lay {
            let (x, y, w, h) = r;
            let ty = y + (h - bm.line_height) / 2.0;
            // `gl` marca el próximo `put` como logo de marca (id, lado en px); el texto sale vacío.
            let gl = std::cell::Cell::new(None);
            let mut put = |t: String, dx: f32, dy: f32, c: Color| stxt.push((t, x + dx * sc, ty + dy * sc, c, gl.take()));
            let logo = |a: sb::Agent, s: f32| { let ic = a.icon(); gl.set(ic.map(|(id, _)| (id, s * sc))); if ic.is_some() { String::new() } else { a.glyph().into() } };
            let ico = |id: u16| gl.set(Some((id, CHROME_PX * sc)));
            let iy = (bm.line_height / sc - CHROME_PX) / 2.0;
            match hit {
                sb::Hit::New => { round(&mut rects, r, RADIUS * sc, BTN_BG, SIDE_BG, true); ico(sb::ICON_NEW); put(String::new(), 8.0, iy, lit); put("New session".into(), 30.0, 0.0, lit); put("^⇧T".into(), (w - 44.0 * sc) / sc, 0.0, dimc); }
                sb::Hit::Folder => { round(&mut rects, r, RADIUS * sc, BTN_BG, SIDE_BG, true); ico(sb::ICON_FOLDER); put(String::new(), 6.0, iy, dimc); }
                sb::Hit::Search => {
                    let t = if side.searching { format!("{}▏", side.query) } else if side.query.is_empty() { "Search agents".into() } else { side.query.clone() };
                    ico(sb::ICON_SEARCH); put(String::new(), 8.0, iy, dimc); put(t, 30.0, 0.0, if side.query.is_empty() && !side.searching { dimc } else { lit });
                    put("/".into(), (w - 12.0 * sc - bm.font_size * 0.602) / sc, 0.0, dimc);
                }
                sb::Hit::Filter(f) => {
                    if side.filter == f { round(&mut rects, (x + 2.0 * sc, y, w - 4.0 * sc, h), RADIUS * sc, [0x24, 0x24, 0x2a], SIDE_BG, true); }
                    let t = match f { sb::Filter::All => format!("All {all}"), sb::Filter::Waiting if wt > 0 => format!("Waiting {wt}"), sb::Filter::Waiting => "Waiting".into(), sb::Filter::Working => format!("Working {wk}") };
                    put(t, 8.0, 0.0, if side.filter == f { lit } else { dimc });
                }
                sb::Hit::Group(i) => if let sb::Item::Group(p) = &items[i] { put(p.clone(), 16.0, 0.0, dimc) },
                sb::Hit::Row(j) => {
                    let e = &ents[j];
                    if e.focused { round(&mut rects, (x + 6.0 * sc, y + sc, w - 12.0 * sc, h - 2.0 * sc), RADIUS * sc, [0x1a, 0x1a, 0x20], SIDE_BG, true); }
                    // 2: barra fina con el contexto gastado de la sesión.
                    if let Some(&p) = used.get(&(e.tab, e.pane)) {
                        let (bx, by, bw, bh) = (x + 38.0 * sc, y + h - 5.0 * sc, w - 54.0 * sc, 2.0 * sc);
                        rect(&mut rects, bx, by, bw, bh, [0x2a, 0x2a, 0x30]);
                        let fill = match p { 80.. => [0xe0, 0x60, 0x60], 60.. => [0xe0, 0xa0, 0x40], _ => [0x8a, 0x8a, 0x94] };
                        rect(&mut rects, bx, by, (bw * p.min(100) as f32 / 100.0).max(2.0 * sc), bh, fill);
                    }
                    let c = e.agent.color();
                    let t = logo(e.agent, CHROME_PX); put(t, 16.0, -9.0 + iy, Color::rgb(c[0], c[1], c[2]));
                    let badge = badge(e, secs).or_else(|| e.unread.then_some((sb::ICON_DOT, 7.0, [0x5a, 0xc0, 0x6a])));
                    if let Some((icon, size, c)) = badge {
                        let bg = if e.focused { [0x1a, 0x1a, 0x20] } else { SIDE_BG };
                        for (id, d, c) in [(sb::ICON_DOT, 12.0, bg), (icon, size, c)] {
                            gl.set(Some((id, d * sc)));
                            put(String::new(), 31.0 - d / 2.0, -9.0 + iy + 2.0 - d / 2.0, Color::rgb(c[0], c[1], c[2]));
                        }
                    }
                    // Título largo: recortado con "…"; con el ratón encima se muestra entero y se desplaza.
                    let avail = w - 82.0 * sc;
                    let long = text_w(&mut g.fs, bm, &e.title) > avail;
                    let hov = long && inside((x, y, w, h), mouse);
                    if hov { shov = Some(j); }
                    let shown = if hov { e.title.clone() } else { text_cut(&mut g.fs, bm, &e.title, avail) };
                    if long { sclip.insert(((x + 38.0 * sc).to_bits(), (ty + -9.0 * sc).to_bits()), (x + 38.0 * sc, x + w - 44.0 * sc, hov)); }
                    let el = sb::fmt_elapsed(e.secs); let elx = (w - 16.0 * sc - text_w(&mut g.fs, bm, &el)) / sc;
                    put(shown, 38.0, -9.0, lit);
                    put(el, elx, -9.0, dimc);
                    match light(e) {
                        // El texto queda fijo en su color para leerse bien.
                        Some((c, label)) => {
                            let line = doing.get(&(e.tab, e.pane)).map_or_else(|| label.to_string(), |d| text_cut(&mut g.fs, bm, d, w - 54.0 * sc));
                            put(line, 38.0, 7.0, if e.status == sb::Status::Working { dimc } else { Color::rgb(c[0], c[1], c[2]) })
                        }
                        None => put(e.status.label().into(), 38.0, 7.0, if e.status == sb::Status::Done { Color::rgb(0x5a, 0xc0, 0x6a) } else { dimc }),
                    }
                }
                sb::Hit::Launch(i) => {
                    let a = sb::LAUNCH[i].0;
                    let c = if side.installed[i] { a.color() } else { [0x3a, 0x3a, 0x42] };
                    let t = logo(a, CHROME_PX); put(t, 6.0, iy, Color::rgb(c[0], c[1], c[2]));
                }
                // Las filas de Archived se dibujan junto con su encabezado.
                sb::Hit::ArchRow(_) | sb::Hit::ArchDel(_) => {}
                sb::Hit::Archived => { ico(sb::ICON_ARCHIVE); put(String::new(), 8.0, iy, dimc); put("Archived".into(), 30.0, 0.0, dimc); let n = side.archived.len().to_string(); let nw = n.len() as f32 * bm.font_size * 0.602; put(n, (w - 12.0 * sc - nw) / sc, 0.0, dimc);
                    if side.open_archived {
                        let rows = sb::archived_rows(&side.archived, SystemTime::now());
                        let tiny = Color::rgb(0x5c, 0x5c, 0x66);
                        for (k, (t, when)) in rows.iter().enumerate() {
                            let ry = sb::arch_y(sz.1 / sc, rows.len(), k) * sc + (sb::ARCH_H * sc - bm.line_height) / 2.0;
                            stxt.push((t.clone(), 16.0 * sc, ry, tiny, None));
                            stxt.push((when.clone(), (sb::SIDE_W - 64.0) * sc, ry, tiny, None));
                            stxt.push(("×".into(), (sb::SIDE_W - 24.0) * sc, ry, dimc, None));
                        }
                    }
                }
            }
        }
    }
    // Columna derecha: Agents arriba y Processes abajo (cada uno con su encabezado, filas y pie).
    let cols = sb::column(sz, g.scale, TABBAR_H * g.scale, side.open_panels());
    let col_a = cols[0];
    if cols.iter().any(Option::is_some) {
        let sc = g.scale;
        let lh = bm.line_height;
        let (dimc, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
        let cx = sz.0 - sb::RIGHT_W * sc;
        rect(&mut rects, cx, TABBAR_H * sc, sb::RIGHT_W * sc, sz.1 - TABBAR_H * sc, SIDE_BG);
        rect(&mut rects, cx, TABBAR_H * sc, 1.0, sz.1 - TABBAR_H * sc, SEP_COLOR);
        if let Some((px, py, pw, ph)) = col_a {
            let arows = sb::agent_rows(ents_all.clone());
            let ty0 = py + (40.0 * sc - lh) / 2.0;
            rect(&mut rects, px, py + 40.0 * sc - 1.0, pw, 1.0, SEP_COLOR);
            stxt.push((String::new(), px + 14.0 * sc, py + (40.0 - CHROME_PX) / 2.0 * sc, lit, Some((sb::ICON_ROBOT, CHROME_PX * sc))));
            stxt.push(("Agents".into(), px + 38.0 * sc, ty0, lit, None));
            stxt.push((format!("{} working", sb::counts(&arows).2), px + 100.0 * sc, ty0, dimc, None));
            stxt.push(("×".into(), px + pw - 26.0 * sc, ty0, dimc, None));
            let ub = usage_btn((px, py, pw, ph), sc);
            if inside(ub, mouse) { rect(&mut rects, ub.0, ub.1, ub.2, ub.3, [0x1a, 0x1a, 0x20]); }
            stxt.push(("Usage ↗".into(), ub.0 + 8.0 * sc, ty0, lit, None));
            for ((x, y, w, h), hit) in sb::agents_layout((px, py, pw, ph), sc, arows.len()) {
                let sb::AHit::Row(i) = hit else { continue };
                let e = &arows[i];
                let hov = inside((x, y, w, h), mouse);
                if (side.col_focus && side.kf == 0 && i == side.agents_sel) || hov { rect(&mut rects, x + 6.0 * sc, y, w - 12.0 * sc, h, [0x1a, 0x1a, 0x20]); }
                let (c, ty) = (e.agent.color(), y + (h - lh) / 2.0);
                stxt.push((String::new(), x + 16.0 * sc, y + (h - CHROME_PX * sc) / 2.0, Color::rgb(c[0], c[1], c[2]), e.agent.icon().map(|(id, _)| (id, CHROME_PX * sc))));
                stxt.push((e.project.clone(), x + 42.0 * sc, ty - 9.0 * sc, lit, None));
                stxt.push((sb::fmt_elapsed(e.secs), x + w - 44.0 * sc, ty - 9.0 * sc, dimc, None));
                if let Some((icon, size, c)) = badge(e, secs) {
                    let lit_row = (side.col_focus && side.kf == 0 && i == side.agents_sel) || hov;
                    let bg = if lit_row { [0x1a, 0x1a, 0x20] } else { SIDE_BG };
                    for (id, d, c) in [(sb::ICON_DOT, 12.0, bg), (icon, size, c)] {
                        let at = (x + (31.0 - d / 2.0) * sc, y + h / 2.0 - (6.0 + d / 2.0) * sc);
                        stxt.push((String::new(), at.0, at.1, Color::rgb(c[0], c[1], c[2]), Some((id, d * sc))));
                    }
                }
                let (label, col) = match light(e) {
                    Some((c, label)) => (label, if e.status == sb::Status::Working { dimc } else { Color::rgb(c[0], c[1], c[2]) }),
                    None => (if e.status == sb::Status::Done { "Done" } else { "Waiting for your message" }, dimc),
                };
                stxt.push((label.into(), x + 42.0 * sc, ty + 7.0 * sc, col, None));
                if hov { stxt.push(("→".into(), x + w - 28.0 * sc, ty + 7.0 * sc, lit, None)); }
            }
            let armed = side.stop_at.is_some_and(|t| t.elapsed().as_secs() < sb::STOP_SECS);
            rect(&mut rects, px, py + ph - 32.0 * sc, pw, 1.0, SEP_COLOR);
            stxt.push((if armed { "press s again to stop" } else { "↑↓ select   ⏎ go   s stop   esc back" }.into(), px + 14.0 * sc, py + ph - 32.0 * sc + (32.0 * sc - lh) / 2.0, dimc, None));
        }
        for k in 0..2 {
            let Some((px, py, pw, ph)) = cols[k + 1] else { continue };
            let list = &side.lists[k];
            if cols[..=k].iter().any(Option::is_some) { rect(&mut rects, px, py, pw, 1.0, SEP_COLOR); }
            let ty0 = py + (40.0 * sc - lh) / 2.0;
            let (amber, blue) = (Color::rgb(0xe0, 0xa0, 0x40), Color::rgb(0x5a, 0x9c, 0xf0));
            let gb = 1.0 / (1u64 << 30) as f64;
            let prows = proc_rows(side, k);
            let (cpu, used, total, sampled) = { let s = side.mon.snap.lock().unwrap(); (s.cpu, s.used, s.total, !s.procs.is_empty()) };
            let ports = k == 1;
            stxt.push((String::new(), px + 14.0 * sc, py + (40.0 - CHROME_PX) / 2.0 * sc, lit, Some((if ports { sb::ICON_GLOBE } else { sb::ICON_PULSE }, CHROME_PX * sc))));
            stxt.push((if ports { "Ports".into() } else { "Processes".into() }, px + 38.0 * sc, ty0, lit, None));
            if ports { if sampled { stxt.push((format!("{} open", prows.len()), px + 100.0 * sc, ty0, dimc, None)); } }
            else if total > 0 { stxt.push((format!("CPU {:.0} %  RAM {:.0}/{:.0} GB", cpu, used as f64 * gb, total as f64 * gb), px + 118.0 * sc, ty0, dimc, None)); }
            stxt.push(("×".into(), px + pw - 26.0 * sc, ty0, dimc, None));
            rect(&mut rects, px, py + 40.0 * sc - 1.0, pw, 1.0, SEP_COLOR);
            let sy = py + 40.0 * sc + (28.0 * sc - lh) / 2.0;
            stxt.push((String::new(), px + 14.0 * sc, py + 40.0 * sc + 6.0 * sc, dimc, Some((sb::ICON_SEARCH, CHROME_PX * sc))));
            stxt.push((if list.query.is_empty() { "Search…".into() } else { format!("{}▏", list.query) }, px + 36.0 * sc, sy, if list.query.is_empty() { dimc } else { lit }, None));
            let by_cpu = side.psort == procs::Sort::Cpu;
            if !ports {
                stxt.push(("CPU".into(), px + pw - 118.0 * sc, sy, if by_cpu { blue } else { dimc }, None));
                stxt.push(("Memory".into(), px + pw - 66.0 * sc, sy, if by_cpu { dimc } else { blue }, None));
            }
            rect(&mut rects, px, py + 68.0 * sc - 1.0, pw, 1.0, SEP_COLOR);
            for ((x, y, w, h), hit) in sb::procs_layout((px, py, pw, ph), sc, prows.len()) {
                let sb::AHit::Row(i) = hit else { continue };
                let r = &prows[i];
                if (side.col_focus && side.kf == k + 1 && i == list.sel) || inside((x, y, w, h), mouse) { rect(&mut rects, x + 6.0 * sc, y, w - 12.0 * sc, h, [0x1a, 0x1a, 0x20]); }
                let ty = y + (h - lh) / 2.0;
                let cut = |s: &str, n: usize| { let c: Vec<char> = s.chars().collect(); if c.len() <= n { s.to_string() } else { format!("{}…", c[..n - 1].iter().collect::<String>()) } };
                if ports {
                    let armed = list.stop.as_ref().is_some_and(|p| p.0 == r.pid);
                    stxt.push((format!(":{}", r.port.unwrap_or(0)), x + 14.0 * sc, ty, blue, None));
                    stxt.push((cut(&r.name, 18), x + 84.0 * sc, ty - 9.0 * sc, lit, None));
                    stxt.push((cut(&format!("pid {}{}", r.pid, r.parent.as_ref().map_or(String::new(), |p| format!(" · {p}"))), 24), x + 84.0 * sc, ty + 7.0 * sc, dimc, None));
                    stxt.push((if armed { "¿×?".into() } else { "×".into() }, x + w - 34.0 * sc, ty, if armed { amber } else { dimc }, None));
                    continue;
                }
                stxt.push((cut(&r.name, 22), x + 14.0 * sc, ty - 9.0 * sc, lit, None));
                if let Some(p) = &r.parent { stxt.push((cut(p, 14), x + 14.0 * sc, ty + 7.0 * sc, dimc, None)); }
                if let Some(p) = r.port { stxt.push((format!(":{p}"), x + 14.0 * sc + 15.0 * 0.6 * lh, ty + 7.0 * sc, blue, None)); }
                stxt.push((format!("{:.1} %", r.cpu), x + w - 118.0 * sc, ty - 9.0 * sc, if r.cpu >= 50.0 { amber } else { dimc }, None));
                stxt.push((procs::fmt_mem(r.rss_kb), x + w - 66.0 * sc, ty - 9.0 * sc, dimc, None));
            }
            if ports && prows.is_empty() {
                let msg = if !sampled { "Buscando puertos…" } else if list.query.is_empty() { "No hay puertos abiertos" } else { "Sin resultados" };
                stxt.push((msg.into(), px + 14.0 * sc, py + 84.0 * sc, dimc, None));
            }
            let fy = py + ph - 32.0 * sc + (32.0 * sc - lh) / 2.0;
            rect(&mut rects, px, py + ph - 32.0 * sc, pw, 1.0, SEP_COLOR);
            if let Some((pid, name, ..)) = &list.stop {
                stxt.push((format!("¿Terminar {} ({pid})?  ⏎ sí · esc no", name.chars().take(14).collect::<String>()), px + 14.0 * sc, fy, lit, None));
            } else {
                stxt.push((if list.changed { "el proceso cambió" } else if list.nope { "no permitido" } else { if ports { "⏎ abrir  ^C copiar  ⇥ ir  ^⌫ stop" } else { "↑↓ select   ⏎ go   Ctrl+⌫ stop   ⇥ sort   esc" } }.into(), px + 14.0 * sc, fy, dimc, None));
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
                b.set_text(t, &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
                b.shape_until_scroll(&mut g.fs, false);
                *k = t.clone();
            }
        }
    }
    for (btn, on) in [(BTN_PULSE, side.lists[0].open), (BTN_ROBOT, side.agents_open), (BTN_GLOBE, side.lists[1].open)] {
        let r = bar_btn(sz.0, g.scale, btn);
        if on { rect(&mut rects, r.0, r.1, r.2, r.3, [0x16, 0x26, 0x3c]); } else if inside(r, mouse) { rect(&mut rects, r.0, r.1, r.2, r.3, TAB_HOVER); }
    }
    for (i, &t) in trs.iter().enumerate() {
        let shape = (t.0 + 2.0 * g.scale, t.1 + 4.0 * g.scale, t.2 - 4.0 * g.scale, t.3 - 4.0 * g.scale);
        if i == active_tab { round(&mut rects, shape, RADIUS * g.scale, TAB_ACTIVE, TABBAR_BG, false); }
        else if hover == Some(BarHit::Tab(i)) || hover == Some(BarHit::Close(i)) { round(&mut rects, shape, RADIUS * g.scale, TAB_HOVER, TABBAR_BG, false); }
        if hover == Some(BarHit::Close(i)) { let c = close_rect(t, g.scale); round(&mut rects, c, 4.0 * g.scale, [0x34, 0x34, 0x3c], TAB_ACTIVE, true); }
    }
    if hover == Some(BarHit::Plus) { round(&mut rects, (plus.0, plus.1 + 4.0 * g.scale, plus.2, plus.3 - 8.0 * g.scale), RADIUS * g.scale, TAB_HOVER, TABBAR_BG, true); }
    if let [a, b] = &tab.panes[..] {
        let x = a.rect.0 + a.rect.2;
        rect(&mut rects, x, a.rect.1, b.rect.0 - x, a.rect.3, SEP_COLOR);
        let f = &tab.panes[tab.active].rect;
        rect(&mut rects, f.0, f.1, f.2, 2.0 * g.scale, FOCUS_LINE);
    }
    let act = tab.active;
    let mut chips: Vec<[f32; 4]> = vec![];
    let wide_ls = 2.0 * cw / g.metrics().font_size - 1.0;
    for (pi, p) in tab.panes.iter_mut().enumerate() {
    let (rx, ry, rw, rh) = p.rect;
    let (ox, oy) = (rx + pad, ry + pad + p.dy);
    let mut grid: Vec<Vec<(usize, usize, char, Rgb, Rgb, bool)>> = vec![vec![]; p.rows];
    let first_rect = rects.len();
    let (off, key);
    let mut hollow = None;
    let mut cbar = None;
    let mut under: Vec<(usize, usize, usize, Rgb)> = vec![];
    {
        let mut term = p.session.term.lock().unwrap();
        if p.agent == sb::Agent::Claude {
            forget_on_clear(&mut term);
            forget_after_resize(&mut term, &mut p.resized);
        }
        off = term.grid().display_offset();
        // Vista bajada solo para pintar este frame (con el candado tomado); luego vuelve al final.
        p.gap = if p.agent == sb::Agent::Claude { fill_gap(&term, &mut p.cleared) } else { 0 };
        key = PaneKey {
            id: p.id, version: p.session.version.load(std::sync::atomic::Ordering::Relaxed), size: (p.cols, p.rows), rect: p.rect, dy: p.dy,
            cell: g.cell, scale: g.scale, offset: off, gap: p.gap, solid: pi == act,
            selection: term.selection.as_ref().and_then(|s| s.to_range(&term)),
            link: link.filter(|l| l.pane == p.id).map(|l| (l.start, l.text.len())),
        };
        // Sin cambios (p. ej. solo gira un spinner): se reutiliza lo dibujado.
        if let Some(d) = p.drawn.as_ref().filter(|d| d.key == key) { rects.extend_from_slice(&d.rects); continue; }
        if p.gap > 0 { term.scroll_display(Scroll::Delta(p.gap as i32)); }
        let content = term.renderable_content();
        let cursor = (content.cursor.shape != CursorShape::Hidden && off == 0).then_some(content.cursor.point);
        // Solo el panel activo lleva cursor sólido; en los demás, hueco.
        let solid = pi == act;
        if !solid { hollow = cursor; }
        // Cursor sólido en bloque: barra fina azul (el resto de formas se dibuja como antes).
        let bar_cur = solid && content.cursor.shape == CursorShape::Block;
        if bar_cur { cbar = cursor; }
        let sel = content.selection;
        for it in content.display_iter {
            let line = it.point.line.0 + content.display_offset as i32;
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
            if link.is_some_and(|l| l.pane == p.id && l.covers(it.point, c.c, p.cols)) { under.push((line as usize, it.point.column.0, w, fg)); }
            grid[line as usize].push((it.point.column.0, w, ch, fg, bg, c.flags.contains(Flags::BOLD)));
        }
        if p.gap > 0 { term.scroll_display(Scroll::Delta(-(p.gap as i32))); }
    }
    // El cursor pasa de filas de la grilla a filas vistas.
    for c in [&mut hollow, &mut cbar] { if let Some(c) = c.as_mut() { c.line.0 += p.gap as i32; } }

    // Solo en pantalla: oculta los avisos que pinta Claude Code junto al modo.
    for row in grid.iter_mut() { hide_hint(row); hide_label(row); }
    let usage_col = [[0x8a, 0x8a, 0x94], [0x3a, 0x3a, 0x42], FG, to_color(AColor::Indexed(3), true), to_color(AColor::Indexed(1), true)];
    ctx_bar(&mut grid, usage_col);
    p.merged = merge_status(&mut grid);
    paint_usage(&mut grid, usage_col);
    grid.iter_mut().for_each(|row| paint_images(row));
    if let Some(gone) = p.merged {
        for c in [&mut hollow, &mut cbar] {
            if let Some(p) = c.as_mut().filter(|p| p.line.0 < gone as i32) { p.line.0 += 1; }
        }
    }
    let merged = p.merged;
    // Pasada 1: fondos fusionados por tramo. Texto: tramos por (color, negrita).
    let at = (p.rect, p.dy, g.cell, g.scale);
    let claude = p.agent == sb::Agent::Claude;
    if claude {
        let sc = g.scale;
        for (r, row) in grid.iter_mut().enumerate() {
            if row.first().is_none_or(|c| c.4 != USER_BG_IN) { continue; }
            for c in row.iter_mut().filter(|c| c.4 == USER_BG_IN) { c.4 = USER_BG; }
            rect(&mut rects, ox - 4.0 * sc, oy + r as f32 * ch, 4.0 * sc, ch, USER_BG);
            rect(&mut rects, ox - 4.0 * sc, oy + r as f32 * ch, 2.0 * sc, ch, USER_BAR);
        }
        if let Some((a, b)) = input_box(&grid, p.cols) {
            // Gris = borde normal; con color (modo bash, plan…) se respeta.
            let fg = grid[a][0].3;
            let edge = if fg.iter().max().unwrap() - fg.iter().min().unwrap() > 24 { fg } else if pi == act { [0x3d, 0x4c, 0x6e] } else { SEP_COLOR };
            let (x, y) = (ox - 5.0 * sc, oy + (a as f32 + 0.5) * ch);
            let (w, h) = (p.cols as f32 * cw + 10.0 * sc, (b - a) as f32 * ch);
            let line = sc.round().max(1.0);
            round(&mut rects, (x, y, w, h), 9.0 * sc, edge, BG, true);
            round(&mut rects, (x + line, y + line, w - 2.0 * line, h - 2.0 * line), 9.0 * sc - line, INPUT_BG, edge, true);
            for r in [a, b] { for c in grid[r].iter_mut() { c.2 = ' '; } }
        }
    }
    for (r, row) in grid.into_iter().enumerate() {
        // Renglón igual y en el mismo sitio: ya están sus rects, su texto formado y sus letras.
        if let Some(d) = p.done_rows[r].as_ref().filter(|d| d.at == at && d.cells == row) { rects.extend_from_slice(&d.rects); continue; }
        let first = rects.len();
        let y = oy + r as f32 * ch;
        if let Some(top) = claude.then(|| footer_row(&row.iter().map(|c| c.2).collect::<String>())).flatten() {
            rect(&mut rects, p.rect.0, y, p.rect.2, ch, TABBAR_BG);
            if top { rect(&mut rects, p.rect.0, y, p.rect.2, g.scale.round().max(1.0), SEP_COLOR); }
        }
        let mut i = 0;
        while i < row.len() {
            let bg = row[i].4;
            let mut j = i;
            while j < row.len() && row[j].4 == bg { j += 1; }
            if bg != BG {
                let (a, z) = (row[i].0, row[j - 1].0 + row[j - 1].1);
                rect(&mut rects, ox + a as f32 * cw, y, (z - a) as f32 * cw, ch, bg);
            }
            i = j;
        }
        let mut spans: Vec<Span> = vec![];
        for c in &row {
            let mut chr = c.2;
            let mut drawn = false;
            // Cajas, bloques, braille y símbolos: se dibujan como rects y el texto deja un espacio.
            if let Some(rs) = glyphs::cell_rects(chr, cw * c.1 as f32, ch) {
                let cx = ox + c.0 as f32 * cw;
                for [rx, ry, rw, rh, a] in rs {
                    let col = if a < 1.0 { [0, 1, 2].map(|k| (c.4[k] as f32 + (c.3[k] as f32 - c.4[k] as f32) * a).round() as u8) } else { c.3 };
                    let (x0, y0) = ((cx + rx).round(), (y + ry).round());
                    rect(&mut rects, x0, y0, (cx + rx + rw).round() - x0, (y + ry + rh).round() - y0, col);
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
        let base = Attrs::new().family(Family::Name("Consolas"));
        let it = spans.iter().map(|(t, f, b, wide)| {
            let a = base.clone().color(Color::rgb(f[0], f[1], f[2])).weight(if *b { Weight::BOLD } else { Weight::NORMAL });
            // shortcut: se asume avance de 1 em en la fuente de respaldo (CJK/emoji); el sobrante hasta 2 celdas va como espaciado.
            (t.as_str(), if *wide { a.letter_spacing(wide_ls) } else { a })
        });
        // El texto solo se vuelve a formar si cambió (un fondo o la posición no lo obligan).
        if p.done_rows[r].as_ref().map(|d| &d.spans) != Some(&spans) {
            p.bufs[r].set_rich_text(it, &base, Shaping::Advanced, None);
            p.bufs[r].shape_until_scroll(&mut g.fs, false);
        }
        p.done_rows[r] = Some(Row { at, cells: row, spans, rects: rects[first..].to_vec(), text: Default::default() });
    }
    for (r, col, w, fg) in under {
        let r = if merged.is_some_and(|gone| r < gone) { r + 1 } else { r };
        let t = g.scale.max(1.0);
        rect(&mut rects, ox + col as f32 * cw, oy + (r + 1) as f32 * ch - 2.0 * t, w as f32 * cw, t, fg);
    }
    if let Some(c) = hollow.filter(|c| c.line.0 >= 0 && (c.line.0 as usize) < p.rows) {
        let (x, y, t) = (ox + c.column.0 as f32 * cw, oy + c.line.0 as f32 * ch, g.scale.max(1.0));
        for (a, b, w, h) in [(x, y, cw, t), (x, y + ch - t, cw, t), (x, y, t, ch), (x + cw - t, y, t, ch)] {
            rect(&mut rects, a, b, w, h, FG);
        }
    }
    if let Some(c) = cbar.filter(|c| c.line.0 >= 0 && (c.line.0 as usize) < p.rows) {
        rect(&mut rects, ox + c.column.0 as f32 * cw, oy + c.line.0 as f32 * ch, 2.0 * g.scale, ch, [0x5a, 0x8d, 0xee]);
    }
    p.chip_rect = None;
    if off > 0 {
        let w = (CHIP.chars().count() + 2) as f32 * cw;
        p.chip_rect = Some([rx + rw / 2.0 - w / 2.0, ry + rh - pad - ch * 3.0, w, ch * 1.5]);
    }
    p.drawn = Some(Drawn { key, rects: rects[first_rect..].to_vec() });
    }
    // Agent usage tapa el área de paneles (sus rects van encima de los de la terminal).
    if let Some((_, ors, ..)) = &ovl {
        for &(r, c) in ors {
            // Las barras delgadas de modelos y agentes van como píldoras sobre el fondo.
            let pill = (r.3 - usage_view::PILL_H * g.scale).abs() < 0.5;
            match usage_view::rounded(c).or(pill.then_some(usage_view::BG)) { Some(under) => round(&mut rects, r, if pill { r.3 / 2.0 } else { 8.0 * g.scale }, c, under, true), None => rect(&mut rects, r.0, r.1, r.2, r.3, c) }
        }
    }
    let n_bg = rects.len();

    // Chips "Jump to bottom" (pasada 2 de rects + segundo TextRenderer, encima del texto).
    for p in tab.panes.iter().filter(|_| ovl.is_none()) {
        if let Some([x, y, w, h]) = p.chip_rect {
            rect(&mut rects, x, y, w, h, [0x2a, 0x2a, 0x32]);
            chips.push([x, y, w, h]);
        }
    }
    // Texto de la barra: ícono + título (+ x en activa/hover) por pestaña, +, y marcadores a la derecha.
    let (tabs, sc) = (&*tabs, g.scale);
    let full = TextBounds { left: 0, top: 0, right: g.w as i32, bottom: (TABBAR_H * sc) as i32 };
    let ty = (TABBAR_H * sc - bm.line_height) / 2.0;
    let (dim_c, lit) = (Color::rgb(0x8a, 0x8a, 0x94), Color::rgb(0xd4, 0xd4, 0xd4));
    let mut bar: Vec<TextArea> = vec![];
    let lg = |id, s: f32, dx: f32, dy: f32, c: Color| [CustomGlyph { id, left: dx, top: dy, width: s, height: s, color: Some(c) }];
    // Logo del agente de cada pestaña (Shell no lleva) con la insignia de su panel más urgente.
    let tabg: Vec<Vec<CustomGlyph>> = tabs.iter().enumerate().map(|(i, t)| {
        let a = t.panes[t.active].agent;
        let Some((id, _)) = a.icon() else { return vec![] };
        let (c, top) = (a.color(), (bm.line_height - CHROME_PX * sc) / 2.0);
        let mut v = lg(id, CHROME_PX * sc, 0.0, top, Color::rgb(c[0], c[1], c[2])).to_vec();
        let urgent = ents_all.iter().filter(|e| e.tab == i && light(e).is_some())
            .min_by_key(|e| match e.status { sb::Status::Asking => 0, sb::Status::Working => 1, _ => 2 });
        if let Some((badge, d, c)) = urgent.and_then(|e| badge(e, secs)) {
            let bg = if i == active_tab { TAB_ACTIVE } else if matches!(hover, Some(BarHit::Tab(j) | BarHit::Close(j)) if j == i) { TAB_HOVER } else { TABBAR_BG };
            for (id, d, c) in [(sb::ICON_DOT, 12.0, bg), (badge, d, c)] {
                v.extend(lg(id, d * sc, (15.0 - d / 2.0) * sc, top + (2.0 - d / 2.0) * sc, Color::rgb(c[0], c[1], c[2])));
            }
        }
        v
    }).collect();
    let sideg: Vec<[CustomGlyph; 1]> = stxt.iter().map(|e| match e.4 { Some((id, s)) => lg(id, s, 0.0, 0.0, e.3), None => lg(u16::MAX, 0.0, 0.0, 0.0, e.3) }).collect();
    for (i, (&t, tb)) in trs.iter().zip(tabs).enumerate() {
        let col = if i == active_tab { lit } else { dim_c };
        let b = TextBounds { left: t.0 as i32, top: 0, right: (t.0 + t.2 - (CLOSE_S + 10.0) * sc) as i32, bottom: full.bottom };
        let x = t.0 + center[i];
        let iw = if tabg[i].is_empty() { 0.0 } else { ICON_W * sc };
        // Logo: área aparte, fija mientras el título se desplaza.
        bar.push(TextArea { buffer: &g.icons[0], left: x, top: ty, bounds: b, default_color: col, custom_glyphs: &tabg[i] });
        // El título (y su carrusel) se recorta junto al logo: nunca lo pisa.
        let tb_b = TextBounds { left: (x + iw) as i32, right: (x + iw + avails[i]) as i32, ..b };
        bar.push(TextArea { buffer: &tb.tbuf, left: x + iw - moff[i], top: ty, bounds: tb_b, default_color: col, custom_glyphs: &[] });
        if i == active_tab || matches!(hover, Some(BarHit::Tab(j) | BarHit::Close(j)) if j == i) {
            let c = close_rect(t, sc);
            bar.push(TextArea { buffer: &g.icons[2], left: c.0 + 4.0 * sc, top: ty, bounds: full, default_color: lit, custom_glyphs: &[] });
        }
    }
    bar.push(TextArea { buffer: &g.icons[1], left: plus.0 + 10.0 * sc, top: ty, bounds: full, default_color: dim_c, custom_glyphs: &[] });
    // Sin color propio: toman el de su TextArea (resaltado cuando el panel está abierto).
    let barg = [sb::ICON_PANEL, sb::ICON_PULSE, sb::ICON_GLOBE, sb::ICON_ROBOT].map(|id| { let mut a = lg(id, CHROME_PX * sc, 0.0, (bm.line_height - CHROME_PX * sc) / 2.0, dim_c); a[0].color = None; a });
    bar.push(TextArea { buffer: &g.icons[0], left: (BAR_LEFT + 6.0) * sc, top: ty, bounds: full, default_color: dim_c, custom_glyphs: &barg[0] });
    let sb_bounds = TextBounds { left: 0, top: 0, right: (sb::SIDE_W * sc) as i32, bottom: g.h as i32 };
    let col_x = sz.0 - sb::RIGHT_W * sc;
    let col_bounds = TextBounds { left: col_x as i32, top: (TABBAR_H * sc) as i32, right: g.w as i32, bottom: g.h as i32 };
    // Reloj del carrusel de la barra lateral: se reinicia al cambiar de fila bajo el ratón.
    let st = {
        let mut h = SH.lock().unwrap();
        match (shov, *h) {
            (Some(j), Some((k, t0))) if j == k => t0.elapsed().as_secs_f32(),
            (Some(j), _) => { *h = Some((j, Instant::now())); 0.0 }
            (None, _) => { *h = None; 0.0 }
        }
    };
    for (((_, x, y, c, ic), (_, b)), gl) in stxt.iter().zip(&g.side).zip(&sideg) {
        let mut bounds = if *x >= col_x && side.open_panels().contains(&true) { col_bounds } else { sb_bounds };
        let mut left = *x;
        if let Some(&(l, r, hov)) = sclip.get(&(x.to_bits(), y.to_bits())) {
            bounds = TextBounds { left: l as i32, right: r as i32, ..sb_bounds };
            if hov {
                let full = b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
                left -= marquee::marquee_offset(st, full - (r - l), 35.0 * sc, 1.0);
                animating = true;
            }
        }
        bar.push(TextArea { buffer: b, left, top: *y, bounds, default_color: *c, custom_glyphs: if ic.is_some() { gl } else { &[] } });
    }
    // Pulso, robot y globo arriba a la derecha; resaltados (azul) mientras su panel está abierto.
    let blue_on = Color::rgb(0x6a, 0xa6, 0xff);
    for (k, btn, on) in [(1, BTN_PULSE, side.lists[0].open), (3, BTN_ROBOT, side.agents_open), (2, BTN_GLOBE, side.lists[1].open)] {
        let r = bar_btn(sz.0, sc, btn);
        bar.push(TextArea { buffer: &g.icons[0], left: r.0 + 6.0 * sc, top: ty, bounds: full, default_color: if on { blue_on } else { dim_c }, custom_glyphs: &barg[k] });
    }
    if let Some((a, _, otx, _)) = &ovl {
        // Un buffer por texto; solo se reforma si cambian texto, tamaño o negrita.
        for (i, (t, _, _, _, size, bold, _)) in otx.iter().enumerate() {
            if i >= g.ov.len() { g.ov.push((String::new(), 0.0, false, Buffer::new(&mut g.fs, bm))); }
            let (k, ks, kb, b) = &mut g.ov[i];
            if k != t || *ks != *size || *kb != *bold {
                let m = Metrics::new(size * sc, (size * sc * 1.25).round());
                b.set_metrics(m);
                b.set_size(Some(a.2), Some(m.line_height));
                b.set_wrap(Wrap::None);
                b.set_text(t, &Attrs::new().family(Family::Name("Segoe UI")).weight(if *bold { Weight::BOLD } else { Weight::NORMAL }), Shaping::Advanced, None);
                b.shape_until_scroll(&mut g.fs, false);
                (*k, *ks, *kb) = (t.clone(), *size, *bold);
            }
        }
    }
    let ob = ovl.as_ref().map(|(a, ..)| TextBounds { left: a.0 as i32, top: a.1 as i32, right: (a.0 + a.2) as i32, bottom: (a.1 + a.3) as i32 });
    // Logos de la pantalla de uso: (glifo, x, y); viven hasta que se pinta el texto.
    let ologos: Vec<([CustomGlyph; 1], f32, f32)> = ovl.iter().flat_map(|o| &o.3).filter_map(|&((x, y, w, _), i)| {
        use sb::Agent::*;
        let a = [Claude, Codex, Antigravity, Opencode, Grok, Junie][i];
        let c = a.color();
        Some((lg(a.icon()?.0, w, 0.0, 0.0, Color::rgb(c[0], c[1], c[2])), x, y))
    }).collect();
    if let (Some((_, _, otx, _)), Some(ob)) = (&ovl, ob) {
        for ((_, x, y, c, .., right), (.., b)) in otx.iter().zip(&g.ov) {
            let left = if *right { *x - b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max) } else { *x };
            bar.push(TextArea { buffer: b, left, top: *y, bounds: ob, default_color: Color::rgb(c[0], c[1], c[2]), custom_glyphs: &[] });
        }
        for (gl, x, y) in &ologos { bar.push(TextArea { buffer: &g.icons[0], left: *x, top: *y, bounds: ob, default_color: Color::rgb(255, 255, 255), custom_glyphs: gl }); }
    }
    let find_at = tabs.get(active_tab).map(|t| (t.panes[t.active].id, t.panes[t.active].rect));
    g.frame.begin(&mut g.sc);
    for &(x, y, w, h, c) in &rects[..n_bg] { g.frame.rect(x, y, w, h, c); }
    g.frame.text(bar.into_iter(), &mut g.fs, &mut g.sc);
    for p in tabs[active_tab].panes.iter().filter(|_| ovl.is_none()) {
        let bounds = TextBounds { left: p.rect.0 as i32, top: p.rect.1 as i32, right: (p.rect.0 + p.rect.2) as i32, bottom: (p.rect.1 + p.rect.3) as i32 };
        for (r, (b, d)) in p.bufs.iter().zip(&p.done_rows).enumerate() {
            let Some(d) = d else { continue };
            if let Some(ops) = d.text.get() { g.frame.ops.extend_from_slice(ops); continue; }
            let start = g.frame.ops.len();
            g.frame.text(std::iter::once(TextArea { buffer: b, left: p.rect.0 + pad, top: p.rect.1 + pad + p.dy + r as f32 * ch, bounds,
                default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] }), &mut g.fs, &mut g.sc);
            let _ = d.text.set(g.frame.ops[start..].to_vec());
        }
    }
    for &(x, y, w, h, c) in &rects[n_bg..] { g.frame.rect(x, y, w, h, c); }
    let bounds = TextBounds { left: 0, top: 0, right: g.w as i32, bottom: g.h as i32 };
    let chip = &g.chip;
    let areas = chips.iter().map(|&[x, y, _, _]| TextArea {
        buffer: chip, left: x + cw, top: y + ch * 0.25, bounds,
        default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] });
    g.frame.text(areas, &mut g.fs, &mut g.sc);
    if let Some((_, b)) = g.find_bar.as_ref().filter(|f| Some(f.0) == find_at.map(|a| a.0)) {
        let (_, (px, py, pw, _)) = find_at.unwrap();
        let (sc, lh) = (g.scale, b.metrics().line_height);
        let w = (b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max) + 24.0 * sc).min(pw - 16.0 * sc);
        let (x, y, h) = (px + pw - w - 8.0 * sc, py + 8.0 * sc, lh + 12.0 * sc);
        g.frame.rect(x - sc, y - sc, w + 2.0 * sc, h + 2.0 * sc, [0x3a, 0x3a, 0x42]);
        g.frame.rect(x, y, w, h, [0x16, 0x16, 0x1a]);
        let bounds = TextBounds { left: x as i32, top: y as i32, right: (x + w) as i32, bottom: (y + h) as i32 };
        g.frame.text(std::iter::once(TextArea { buffer: b, left: x + 12.0 * sc, top: y + 6.0 * sc, bounds,
            default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] }), &mut g.fs, &mut g.sc);
    }
    if let Some((rows, hi)) = &g.pal {
        let sc = g.scale;
        let (x, y, w, rh) = palette::geom(g.w as f32, sc, bm.line_height);
        let h = rh * rows.len() as f32;
        g.frame.rect(x - sc, y - sc, w + 2.0 * sc, h + 2.0 * sc, [0x3a, 0x3a, 0x42]);
        g.frame.rect(x, y, w, h, [0x16, 0x16, 0x1a]);
        g.frame.rect(x, y + rh - sc, w, sc, SEP_COLOR);
        if *hi > 0 { g.frame.rect(x, y + *hi as f32 * rh, w, rh, [0x2a, 0x2a, 0x32]); }
        let bounds = TextBounds { left: x as i32, top: y as i32, right: (x + w) as i32, bottom: (y + h) as i32 };
        let areas = rows.iter().enumerate().map(|(i, b)| TextArea { buffer: b, left: x + 12.0 * sc, top: y + i as f32 * rh + 6.0 * sc, bounds,
            default_color: Color::rgb(0xd4, 0xd4, 0xd4), custom_glyphs: &[] });
        g.frame.text(areas, &mut g.fs, &mut g.sc);
    }
    let (bands, ops) = g.frame.bands(g.h as usize);
    let (frame, fs, sc) = (&g.frame, &mut g.fs, &mut g.sc);
    g.busy = !g.screen.present(&bands, |px, stride, i| frame.paint_band(px, stride, i, &ops[i], BG, fs, sc));
    animating
}

/// `font.txt`: tamaño de letra; si no es válido, el normal.
fn parse_font(text: &str) -> f32 { text.trim().parse::<f32>().ok().filter(|f| (FONT_RANGE.0..=FONT_RANGE.1).contains(f)).unwrap_or(FONT_SIZE) }

/// Coincidencia de `q` (sin distinguir mayúsculas) en el panel e historial: la anterior (`up`) o siguiente a `from`, dando la vuelta.
// shortcut: no encuentra texto partido entre dos renglones; unir las líneas envueltas si hace falta.
fn find_match<L: EventListener>(t: &Term<L>, q: &str, from: Option<Point>, up: bool) -> Option<(Point, Point)> {
    let low = |c: char| c.to_lowercase().next().unwrap_or(c);
    let q: Vec<char> = q.chars().map(low).collect();
    if q.is_empty() { return None; }
    let mut all = vec![];
    for l in t.topmost_line().0..=t.bottommost_line().0 {
        let row: Vec<char> = (0..t.columns()).map(|c| low(t.grid()[Line(l)][Column(c)].c)).collect();
        all.extend(row.windows(q.len()).enumerate().filter(|(_, w)| *w == q.as_slice())
            .map(|(c, _)| (Point::new(Line(l), Column(c)), Point::new(Line(l), Column(c + q.len() - 1)))));
    }
    let next = match (from, up) {
        (Some(p), true) => all.iter().rev().find(|m| m.0 < p),
        (Some(p), false) => all.iter().find(|m| m.0 > p),
        (None, _) => None,
    };
    next.or(if up { all.last() } else { all.first() }).copied()
}

/// `window.txt`: "x y ancho alto" en puntos; None si falta algo o el tamaño no es razonable.
fn parse_window(text: &str) -> Option<[f64; 4]> {
    let v: Vec<f64> = text.split_whitespace().map(|n| n.parse().ok().filter(|n: &f64| n.is_finite())).collect::<Option<_>>()?;
    let v: [f64; 4] = v.try_into().ok()?;
    ((400.0..=10000.0).contains(&v[2]) && (300.0..=10000.0).contains(&v[3])).then_some(v)
}

/// Guarda tamaño y posición para abrir igual: si Claude arranca pequeño y luego se agranda, deja filas vacías abajo.
fn save_window(w: &Window) {
    if w.fullscreen().is_some() || w.is_minimized() == Some(true) { return; }
    let (sc, Ok(p)) = (w.scale_factor(), w.outer_position()) else { return };
    let (p, z) = (p.to_logical::<f64>(sc), w.inner_size().to_logical::<f64>(sc));
    save_file("window.txt", &format!("{} {} {} {}", p.x, p.y, z.width, z.height));
}

fn init_gfx(el: &ActiveEventLoop) -> Gfx {
    let saved = data_file("window.txt").and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| parse_window(&t));
    let [_, _, ww, wh] = saved.unwrap_or([0.0, 0.0, 960.0, 600.0]);
    let mut attrs = Window::default_attributes()
        .with_inner_size(LogicalSize::new(ww, wh)).with_title("Ónix");
    // La posición solo se restaura si cae en un monitor conectado ahora.
    if let Some([x, y, ..]) = saved {
        let visible = el.available_monitors().any(|m| {
            let (p, z) = (m.position().to_logical::<f64>(m.scale_factor()), m.size().to_logical::<f64>(m.scale_factor()));
            x >= p.x && x < p.x + z.width - 100.0 && y >= p.y && y < p.y + z.height - 100.0
        });
        if visible { attrs = attrs.with_position(winit::dpi::LogicalPosition::new(x, y)); }
    }
    let window = Arc::new(el.create_window(attrs).unwrap());
    let sz = window.inner_size();
    let screen = surface::Screen::new(&window);

    let mut db = fontdb::Database::new();
    // Respaldo para emoji, CJK, símbolos e íconos Nerd Font: solo se carga lo que existe (fontdb las mapea, no las copia).
    let home = std::env::var("HOME").unwrap_or_default();
    let nerd = [format!("{home}/AppData/Local/Microsoft/Windows/Fonts"), "C:/Windows/Fonts".into()].into_iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains("Nerd")));
    let sys = ["segoeui.ttf", "segoeuib.ttf", "consola.ttf", "consolab.ttf", "consolai.ttf", "seguisym.ttf", "seguiemj.ttf", "msyh.ttc", "malgun.ttf", "cambria.ttc", "arial.ttf"]
        .map(|f| std::path::Path::new("C:/Windows/Fonts").join(f));
    for p in sys.into_iter().chain(nerd) { let _ = db.load_font_file(p); }
    let fs = FontSystem::new_with_locale_and_db("en-US".into(), db);
    let mut g = Gfx {
        window, screen, w: sz.width.max(1), h: sz.height.max(1), fs, sc: SwashCache::new(), frame: Default::default(), busy: false,
        chip: Buffer::new_empty(Metrics::new(1.0, 1.0)), icons: vec![], side: vec![], ov: vec![], scale: 1.0, cell: (8.0, 16.0), find_bar: None, pal: None,
        font: data_file("font.txt").and_then(|f| std::fs::read_to_string(f).ok()).map_or(FONT_SIZE, |t| parse_font(&t)) };
    g.screen.resize(g.w as usize, g.h as usize, g.window.scale_factor());
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

/// La ventana ya ocupa la pantalla (menos barra de menús y Dock), aunque macOS no la dé por maximizada.
fn fills(win: (u32, u32), monitor: (u32, u32), scale: f64) -> bool {
    let near = |a: u32, b: u32, slack: f64| a as f64 + slack * scale >= b as f64;
    near(win.0, monitor.0, 90.0) && near(win.1, monitor.1, 150.0)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum TogAct { ExitFullscreen, Restore, Maximize }
/// Decide el doble clic de "maximizar": is_maximized() de winit falla en ventanas sin decoración, por eso hay estado propio (`restore`).
fn toggle_action(fullscreen: bool, restore_some: bool, is_max: bool) -> TogAct {
    if fullscreen { TogAct::ExitFullscreen } else if restore_some || is_max { TogAct::Restore } else { TogAct::Maximize }
}

struct App {
    g: Option<Gfx>, tabs: Vec<Tab>, active_tab: usize, next_id: u64, proxy: EventLoopProxy<Ev>,
    mods: ModifiersState, mouse: (f32, f32), scroll_acc: f32, drag_sep: bool, mouse_fwd: bool, hidden: bool, last_draw: Instant, last_input: Instant, next_frame: Option<Instant>, peek: Peek,
    picking: bool, focused: bool, find: Option<String>, pal: Option<palette::Palette>, mq: (Option<usize>, Instant),
    side: sb::Side, last_bar_click: Option<(Instant, (f32, f32))>, last_sep_click: Option<(Instant, (f32, f32))>, ticks: u32,
    restore: Option<(winit::dpi::PhysicalPosition<i32>, winit::dpi::PhysicalSize<u32>)>,
    uv: usage_view::View,
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
                DcAction::Maximize => match toggle_action(w.fullscreen().is_some(), self.restore.is_some(), w.is_maximized() || w.current_monitor().is_some_and(|m| fills(w.inner_size().into(), m.size().into(), w.scale_factor()))) {
                    TogAct::ExitFullscreen => { w.set_fullscreen(None); self.restore = None; }
                    TogAct::Restore => {
                        w.set_maximized(false);
                        // Si abrió ya grande (tamaño recordado) no hay a cuál volver: el de fábrica, centrado.
                        let small = self.restore.take().or_else(|| w.current_monitor().map(|m| {
                            let (sz, o, mz) = (LogicalSize::new(960.0, 600.0).to_physical::<u32>(w.scale_factor()), m.position(), m.size());
                            (winit::dpi::PhysicalPosition::new(o.x + (mz.width.saturating_sub(sz.width) / 2) as i32, o.y + (mz.height.saturating_sub(sz.height) / 2) as i32), sz)
                        }));
                        if let Some((p, sz)) = small { let _ = w.request_inner_size(sz); w.set_outer_position(p); }
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
        let right = if self.side.open_panels().contains(&true) { sb::RIGHT_W * g.scale } else { 0.0 };
        (left, top, (g.w as f32 - left - right).max(1.0), (g.h as f32 - top).max(1.0))
    }
    /// ⌘+ / ⌘- cambian la letra de a un punto; ⌘0 (`None`) la normal.
    fn set_font(&mut self, step: Option<f32>) {
        let Some(g) = self.g.as_mut() else { return };
        g.font = step.map_or(FONT_SIZE, |d| (g.font + d).clamp(FONT_RANGE.0, FONT_RANGE.1));
        save_file("font.txt", &g.font.to_string());
        self.sync_size(true);
        self.redraw();
    }
    /// Busca desde la coincidencia actual, la selecciona, la trae a la vista y actualiza la barra.
    fn search(&mut self, again: bool, up: bool) {
        let query = self.find.clone().unwrap_or_default();
        let p = self.pane();
        let id = p.id;
        let found = {
            let mut t = p.session.term.lock().unwrap();
            let from = t.selection.as_ref().and_then(|s| s.to_range(&t)).filter(|_| again).map(|r| r.start);
            let m = find_match(&t, &query, from, up);
            t.selection = m.map(|(a, b)| { let mut s = Selection::new(SelectionType::Simple, a, Side::Left); s.update(b, Side::Right); s });
            if let Some((a, _)) = m { t.scroll_to_point(a); }
            m.is_some()
        };
        let note = if query.is_empty() || found { "⏎ anterior  ⇧⏎ siguiente  esc" } else { "sin resultados  esc" };
        let Some(g) = self.g.as_mut() else { return };
        let m = g.bar_metrics();
        let mut b = Buffer::new(&mut g.fs, m);
        b.set_size(None, Some(m.line_height));
        b.set_text(&format!("Buscar: {query}▏   {note}"), &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
        b.shape_until_scroll(&mut g.fs, false);
        g.find_bar = Some((id, b));
        self.redraw();
    }
    fn close_find(&mut self) {
        if self.find.take().is_none() { return; }
        if let Some(g) = self.g.as_mut() { g.find_bar = None; }
        let mut t = self.pane().session.term.lock().unwrap();
        t.selection = None;
        t.scroll_display(Scroll::Bottom);
        drop(t);
        self.redraw();
    }
    fn find_key(&mut self, ev: &winit::event::KeyEvent) {
        let Some(q) = self.find.as_mut() else { return };
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => return self.close_find(),
            Key::Named(NamedKey::Enter) => { let up = !self.mods.shift_key(); return self.search(true, up); }
            Key::Named(NamedKey::Backspace) => { q.pop(); }
            _ => if let Some(t) = &ev.text { q.extend(t.chars().filter(|c| !c.is_control())); } else { return },
        }
        self.search(false, true);
    }
    fn redraw(&self) { if let Some(g) = &self.g { g.window.request_redraw(); } }
    /// ⌘D: segundo panel en la pestaña activa (máximo dos).
    fn split(&mut self) {
        if self.tabs[self.active_tab].panes.len() >= 2 { return; }
        let p = self.new_pane(None, None);
        let t = &mut self.tabs[self.active_tab];
        t.panes.push(p);
        t.active = 1;
        t.split = 0.5;
        self.sync_size(false);
        self.redraw();
    }
    /// ⌘J: salta al siguiente agente que pide respuesta o que terminó sin que lo vieras.
    fn go_attention(&mut self) {
        let at: Vec<(usize, usize)> = self.tabs.iter().enumerate().flat_map(|(ti, t)| (0..t.panes.len()).map(move |pi| (ti, pi))).collect();
        let flags: Vec<(bool, bool)> = at.iter().map(|&(ti, pi)| { let p = &self.tabs[ti].panes[pi]; (p.asking, p.done) }).collect();
        let now = at.iter().position(|&x| x == (self.active_tab, self.tabs[self.active_tab].active)).unwrap_or(0);
        let Some(i) = sb::next_attention(&flags, now) else { return };
        let (ti, pi) = at[i];
        (self.side.focus, self.side.col_focus, self.uv.open) = (false, false, false);
        self.active_tab = ti;
        self.tabs[ti].active = pi;
        self.redraw();
    }
    fn open_palette(&mut self) {
        self.close_find();
        let item = |label: String, hint, cmd| palette::Item { label, hint, cmd };
        let mut items = vec![];
        if self.tabs.iter().flat_map(|t| &t.panes).any(|p| p.asking || p.done) { items.push(item("Ir al agente que te necesita".into(), "^⇧J", palette::Cmd::Attention)); }
        for (i, t) in self.tabs.iter().enumerate().filter(|(i, _)| *i != self.active_tab) {
            let mark = if t.panes.iter().any(|p| p.asking) { "  ● pide respuesta" } else if t.panes.iter().any(|p| p.done) { "  ● terminó" } else { "" };
            items.push(item(format!("Pestaña {}: {}{mark}", i + 1, t.title()), "", palette::Cmd::Tab(i)));
        }
        items.extend(palette::ACTIONS.iter().map(|&(l, h, c)| item(l.into(), h, c)));
        items.extend(sb::LAUNCH.iter().enumerate().filter(|(i, _)| self.side.installed[*i]).map(|(i, (_, bin))| item(format!("Abrir {bin}"), "", palette::Cmd::Launch(i))));
        items.extend(self.side.archived.iter().rev().enumerate().map(|(k, a)| item(format!("Retomar: {} · {}", a.title, a.project), "", palette::Cmd::Restore(k))));
        self.pal = Some(palette::Palette { items, ..Default::default() });
        self.show_palette();
    }
    /// Rehace el texto de la paleta tras abrirla, escribir o mover la selección.
    fn show_palette(&mut self) {
        let Some(g) = self.g.as_mut() else { return };
        g.pal = self.pal.as_ref().map(|p| {
            let m = g.bar_metrics();
            let (_, _, w, _) = palette::geom(g.w as f32, g.scale, m.line_height);
            let cols = ((w - 24.0 * g.scale) / (m.font_size * 0.602)) as usize;
            let found = p.found();
            let mut text = vec![format!("› {}▏", p.query)];
            text.extend(found.iter().skip(p.first()).take(palette::ROWS).map(|i| palette::row_text(&i.label, i.hint, cols)));
            if found.is_empty() { text.push("sin resultados".into()); }
            let hi = if found.is_empty() { 0 } else { p.sel - p.first() + 1 };
            (text.iter().map(|t| {
                let mut b = Buffer::new(&mut g.fs, m);
                b.set_size(None, Some(m.line_height));
                b.set_text(t, &Attrs::new().family(Family::Name("Consolas")), Shaping::Advanced, None);
                b.shape_until_scroll(&mut g.fs, false);
                b
            }).collect(), hi)
        });
        self.redraw();
    }
    fn close_palette(&mut self) {
        self.pal = None;
        self.show_palette();
    }
    fn palette_key(&mut self, el: &ActiveEventLoop, ev: &winit::event::KeyEvent) {
        let Some(p) = self.pal.as_mut() else { return };
        let n = p.found().len();
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => return self.close_palette(),
            Key::Named(NamedKey::Enter) => { let c = p.picked(); self.close_palette(); if let Some(c) = c { self.run_cmd(el, c); } return; }
            Key::Named(NamedKey::ArrowUp) => p.sel = sb::move_sel(p.sel, -1, n),
            Key::Named(NamedKey::ArrowDown) => p.sel = sb::move_sel(p.sel, 1, n),
            Key::Named(NamedKey::Backspace) => { p.query.pop(); p.sel = 0; }
            _ => match &ev.text { Some(t) => { p.query.extend(t.chars().filter(|c| !c.is_control())); p.sel = 0; } None => return },
        }
        self.show_palette();
    }
    /// Clic con la paleta abierta: en una fila la ejecuta; fuera, la cierra.
    fn palette_click(&mut self, el: &ActiveEventLoop) {
        let (Some(g), Some(p)) = (self.g.as_ref(), self.pal.as_ref()) else { return };
        let (x, y, w, rh) = palette::geom(g.w as f32, g.scale, g.bar_metrics().line_height);
        let rows = g.pal.as_ref().map_or(0, |r| r.0.len());
        if !inside((x, y, w, rh * rows as f32), self.mouse) { return self.close_palette(); }
        let row = ((self.mouse.1 - y) / rh) as usize;
        if row == 0 { return; }
        let Some(c) = p.found().get(p.first() + row - 1).map(|i| i.cmd) else { return };
        self.close_palette();
        self.run_cmd(el, c);
    }
    fn run_cmd(&mut self, el: &ActiveEventLoop, c: palette::Cmd) {
        use palette::Cmd;
        (self.side.focus, self.side.col_focus, self.side.searching) = (false, false, false);
        // Lo que actúa sobre la terminal cierra antes Agent usage, que la tapa.
        if !matches!(c, Cmd::Usage) { self.uv.open = false; }
        match c {
            Cmd::Attention => self.go_attention(),
            Cmd::Tab(i) => if i < self.tabs.len() { self.active_tab = i; },
            Cmd::NewTab => self.new_tab(),
            Cmd::OpenFolder => self.pick_folder(),
            Cmd::Split => self.split(),
            Cmd::ClosePane => { let (ti, pi) = (self.active_tab, self.tabs[self.active_tab].active); self.close_pane(el, ti, pi); }
            Cmd::Search => { self.find = Some(String::new()); self.search(false, true); }
            Cmd::Sidebar => self.toggle_side(),
            Cmd::Agents => self.toggle_agents(),
            Cmd::Processes => self.toggle_list(0),
            Cmd::Ports => self.toggle_list(1),
            Cmd::Usage => self.toggle_usage(),
            Cmd::FontUp => self.set_font(Some(1.0)),
            Cmd::FontDown => self.set_font(Some(-1.0)),
            Cmd::FontReset => self.set_font(None),
            Cmd::Launch(i) => self.launch(i),
            Cmd::Restore(k) => self.restore_archived(k),
        }
        self.redraw();
    }
    fn new_pane(&mut self, cmd: Option<Vec<String>>, cwd: Option<std::path::PathBuf>) -> Pane {
        let id = self.next_id;
        self.next_id += 1;
        let dir = cwd.as_ref().map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        let (p1, p2) = (self.proxy.clone(), self.proxy.clone());
        let session = Session::spawn(cmd, cwd, move || { let _ = p1.send_event(Ev::Out(id)); }, move || { let _ = p2.send_event(Ev::Exit(id)); });
        let mut p = Pane { id, session, anchor: None, bufs: vec![], done_rows: vec![], chip_rect: None, cols: 0, rows: 0, rect: (0.0, 0.0, 1.0, 1.0), dy: 0.0, merged: None, gap: 0, cleared: false, resized: false, working: None, asking: false, ctx: None, done: false, drawn: None, picked: None, asleep: false, last_busy: Instant::now(),
            born: Instant::now(), last_out: Instant::now(), unread: false, agent: sb::Agent::Shell, had_agent: false, project: "~".into(), token: None, dir, cwd: "~".into() };
        p.refresh();
        p
    }
    fn new_tab(&mut self) { self.new_tab_with(None, None); }
    /// Abre el selector de carpeta en un hilo (uno a la vez); el resultado vuelve como Ev::Folder.
    fn pick_folder(&mut self) {
        if self.picking || self.tabs.is_empty() { return; }
        self.picking = true;
        let p = self.pane();
        let pid = win::fg_pid(&p.session).or(p.session.child_pid.map(|p| p as i32));
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
    /// Archiva en disco una sesión de agente que se cierra (los shells sin agente no se guardan). Tope 200.
    fn archive(&mut self, ti: usize, pi: usize) {
        let p = &mut self.tabs[ti].panes[pi];
        p.refresh();
        let Some(token) = p.token.clone() else { return };
        let t = sb::clean_title(&p.session.title.lock().unwrap(), TITLE_CH);
        let title = if t.is_empty() { p.cwd.clone() } else { t };
        self.side.archived.push(sb::Archived { title, project: p.project.clone(), cwd: p.dir.clone(), token, at: SystemTime::now() });
        if self.side.archived.len() > 200 { self.side.archived.remove(0); }
        self.save_archived();
    }
    fn save_archived(&self) { save_file("archived.json", &sb::archived_to_json(&self.side.archived)); }
    /// Guarda carpeta y sesión del panel activo de cada pestaña.
    fn save_tabs(&self) {
        let tabs: Vec<(String, Option<String>)> = self.tabs.iter().map(|t| { let p = &t.panes[t.active]; (p.dir.clone(), p.token.clone()) }).collect();
        save_file("tabs.txt", &tabs_to_text(&tabs));
    }
    /// Reabre la fila `k` de Archived (0 = la más reciente) y la saca de la lista.
    fn restore_archived(&mut self, k: usize) {
        let Some(i) = self.side.archived.len().checked_sub(k + 1) else { return };
        let a = &self.side.archived[i];
        // Carpeta borrada, o sesión de Claude aún viva en otro sitio: se queda archivada.
        if std::fs::read_dir(&a.cwd).is_err() || alive_now(&a.token) { return; }
        let a = self.side.archived.remove(i);
        let taken = self.tabs.iter().flat_map(|t| &t.panes).any(|p| p.token.as_deref() == Some(a.token.as_str()) && p.dir == a.cwd);
        // Codex: selector, porque "la última" puede no ser la archivada. agy y opencode continúan la última.
        self.open_saved(&a.token, a.cwd.into(), a.token != "codex" && !taken);
        self.save_archived();
        self.save_tabs();
    }
    fn toggle_side(&mut self) {
        self.side.visible = !self.side.visible;
        self.side.searching = false;
        if self.side.visible { for t in &mut self.tabs { for p in &mut t.panes { p.refresh(); } } }
        self.sync_size(false);
        self.redraw();
    }
    /// Abre una pestaña retomando la sesión de `token` (ver `resume_cmd`).
    fn open_saved(&mut self, token: &str, cwd: std::path::PathBuf, last: bool) {
        if let Some(cmd) = resume_cmd(token, last) {
            self.new_tab_with(Some(cmd), Some(cwd));
            self.tabs.last_mut().unwrap().panes[0].token = Some(token.to_string());
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
        s.col_focus = s.open_panels().contains(&true);
        if s.agents_open { s.kf = 0; }
        self.sync_size(false);
        self.redraw();
    }
    /// Abre o cierra Processes (`k` = 0) o Ports (`k` = 1); se apilan con Agents.
    fn toggle_list(&mut self, k: usize) {
        let s = &mut self.side;
        let open = !s.lists[k].open;
        s.lists[k] = sb::List { open, ..Default::default() };
        s.searching = false;
        if open { s.kf = k + 1; }
        s.mon.set(s.lists.iter().any(|l| l.open));
        s.col_focus = s.open_panels().contains(&true);
        self.sync_size(false);
        self.redraw();
    }
    /// Termina el proceso confirmado, solo si sigue siendo el mismo y es nuestro.
    fn stop_proc(&mut self, k: usize, (pid, name, ppid, uid): (i32, String, i32, u32)) {
        let mut snap = self.side.mon.snap.lock().unwrap();
        if !procs::same_proc(&snap.procs, pid, &name, ppid, uid) { self.side.lists[k].changed = true; return; }
        let me = std::process::id();
        if procs::can_stop(uid, win::OWN_UID, pid, me as i32, win::parent_of(me)) && kill(pid, 15) == 0 {
            // Sale ya de la lista; si ignora la señal, vuelve en la siguiente lectura.
            snap.listen.retain(|l| l.1 != pid);
        } else { self.side.lists[k].nope = true; }
    }
    /// Pide confirmación para terminar el proceso de la fila.
    fn arm_stop(&mut self, k: usize, r: &procs::Row) {
        let me = std::process::id();
        if procs::can_stop(r.uid, win::OWN_UID, r.pid, me as i32, win::parent_of(me)) { self.side.lists[k].stop = Some((r.pid, r.name.clone(), r.ppid, r.uid)); } else { self.side.lists[k].nope = true; }
    }
    /// Abre o cierra Agent usage; al abrir relee el historial en segundo plano (si tiene más de 60 s).
    fn toggle_usage(&mut self) {
        self.uv.open = !self.uv.open;
        if self.uv.open { let px = self.proxy.clone(); self.uv.load(move || { let _ = px.send_event(Ev::Usage); }); }
        self.redraw();
    }
    fn close_agents(&mut self) { self.side.agents_open = false; self.sync_size(false); }
    fn close_list(&mut self, k: usize) { self.side.lists[k].open = false; self.side.mon.set(self.side.lists.iter().any(|l| l.open)); self.sync_size(false); }
    /// Rects (px) de Agents y Processes en la columna derecha.
    fn column(&self) -> [Option<R>; 3] {
        let g = self.g.as_ref().unwrap();
        sb::column((g.w as f32, g.h as f32), g.scale, TABBAR_H * g.scale, self.side.open_panels())
    }
    /// Enfoca el panel de la app cuyo pty es el proceso (o un ancestro suyo); si no hay, nada.
    fn go_proc(&mut self, pid: i32) {
        let ch = procs::chain(&self.side.mon.snap.lock().unwrap().procs, pid);
        let hit = self.tabs.iter().enumerate().find_map(|(ti, t)| t.panes.iter().position(|p| {
            [win::fg_pid(&p.session), p.session.child_pid.map(|c| c as i32)].into_iter().flatten().any(|x| ch.contains(&x))
        }).map(|pi| (ti, pi)));
        if let Some((ti, pi)) = hit { self.side.focus = false; self.active_tab = ti; self.tabs[ti].active = pi; }
    }
    /// Teclas del panel Processes (abierto y visible): consume todo para que nada llegue al PTY.
    fn procs_key(&mut self, k: usize, ev: &winit::event::KeyEvent) {
        let rows = proc_rows(&self.side, k);
        let sel = self.side.lists[k].sel.min(rows.len().saturating_sub(1));
        let pending = self.side.lists[k].stop.take();
        (self.side.lists[k].nope, self.side.lists[k].changed) = (false, false);
        match ev.logical_key.as_ref() {
            Key::Named(NamedKey::ArrowUp) => self.side.lists[k].sel = sb::move_sel(sel, -1, rows.len()),
            Key::Named(NamedKey::ArrowDown) => self.side.lists[k].sel = sb::move_sel(sel, 1, rows.len()),
            Key::Named(NamedKey::Tab) if k == 1 => if let Some(r) = rows.get(sel) { self.go_proc(r.pid); },
            Key::Named(NamedKey::Tab) => self.side.psort = if self.side.psort == procs::Sort::Cpu { procs::Sort::Mem } else { procs::Sort::Cpu },
            Key::Named(NamedKey::Escape) => if pending.is_none() { self.close_list(k) },
            Key::Named(NamedKey::Enter) => match pending {
                Some(p) => self.stop_proc(k, p),
                None => if let Some(r) = rows.get(sel) {
                    if k == 1 { win::browse(&local_url(r)); } else { self.go_proc(r.pid); }
                },
            },
            Key::Named(NamedKey::Backspace) if self.mods.control_key() => if let Some(r) = rows.get(sel) { self.arm_stop(k, r); },
            Key::Character("c") if self.mods.control_key() => if let Some(r) = rows.get(sel) { pbcopy(&local_url(r)); },
            Key::Named(NamedKey::Backspace) => { self.side.lists[k].query.pop(); self.side.lists[k].sel = 0; }
            _ => if let Some(t) = &ev.text { self.side.lists[k].query.extend(t.chars().filter(|c| !c.is_control())); self.side.lists[k].sel = 0; },
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
        let cols = self.column();
        if let Some(k) = cols.iter().position(|r| r.is_some_and(|r| inside(r, m))) { self.side.kf = k; }
        if let Some(r) = cols[0].filter(|r| inside(*r, m)) {
            let rows = sb::agent_rows(entries(&self.tabs, self.active_tab));
            if inside(usage_btn(r, sc), m) { self.toggle_usage(); return; }
            match sb::ahit_at(&sb::agents_layout(r, sc, rows.len()), m) {
                Some(sb::AHit::Close) => self.close_agents(),
                Some(sb::AHit::Row(i)) => { self.side.agents_sel = i; self.go_agent(&rows[i]); }
                None => {}
            }
        } else if let Some((k, r)) = (0..2).find_map(|k| Some((k, cols[k + 1].filter(|r| inside(*r, m))?))) {
            let n = proc_rows(&self.side, k);
            let pending = self.side.lists[k].stop.take();
            (self.side.lists[k].nope, self.side.lists[k].changed) = (false, false);
            match sb::ahit_at(&sb::procs_layout(r, sc, n.len()), m) {
                Some(sb::AHit::Close) => self.close_list(k),
                Some(sb::AHit::Row(i)) => {
                    self.side.lists[k].sel = i;
                    if k == 0 { self.go_proc(n[i].pid); }
                    // En Ports, la × de la fila pide confirmación y un segundo clic termina; el resto abre en el navegador.
                    else if m.0 >= r.0 + r.2 - 48.0 * sc {
                        match pending.filter(|p| p.0 == n[i].pid) { Some(p) => self.stop_proc(k, p), None => self.arm_stop(k, &n[i]) }
                    } else { win::browse(&local_url(&n[i])); }
                }
                None => {}
            }
        }
        self.redraw();
    }
    /// Abre el agente `LAUNCH[i]` en la carpeta del panel activo: si ese panel es un shell libre
    /// (sin nada corriendo), se escribe ahí mismo; si no, en una pestaña nueva con esa carpeta.
    fn launch(&mut self, i: usize) {
        let bin = sb::LAUNCH[i].1;
        let p = self.pane();
        let shell = p.session.child_pid.map(|c| c as i32);
        let idle = p.agent == sb::Agent::Shell && shell.is_some() && win::fg_pid(&p.session) == shell;
        if idle {
            // Ctrl+U borra lo que hubiera escrito a medias; luego el comando y Enter.
            self.send_input(format!("\x1b{bin}\r").as_bytes());
            self.redraw();
            return;
        }
        let pid = win::fg_pid(&p.session).or(shell);
        let cwd = pid.map(|pid| sb::proc_info(pid).2).filter(|c| std::fs::read_dir(c).is_ok()).map(std::path::PathBuf::from);
        self.new_tab_with(Some(shell_cmd(bin.to_string())), cwd);
    }
    fn side_click(&mut self) {
        let (g, m) = (self.g.as_ref().unwrap(), self.mouse);
        let sc = g.scale;
        if m.1 < TABBAR_H * sc { self.bar_press(); return; }
        let ents = sb::filter(entries(&self.tabs, self.active_tab), self.side.filter, &self.side.query);
        let lay = sb::layout((g.w as f32, g.h as f32), g.scale, &sb::group(&ents), if self.side.open_archived { self.side.archived.len().min(sb::ARCH_MAX) } else { 0 });
        self.side.focus = true;
        self.side.searching = false;
        match sb::hit_at(&lay, m) {
            Some(sb::Hit::New) => { self.side.focus = false; self.new_tab(); }
            Some(sb::Hit::Folder) => { self.side.focus = false; self.pick_folder(); }
            Some(sb::Hit::Search) => self.side.searching = true,
            Some(sb::Hit::Archived) => self.side.open_archived = !self.side.open_archived,
            Some(sb::Hit::ArchRow(k)) => { self.side.focus = false; self.restore_archived(k); }
            Some(sb::Hit::ArchDel(k)) => if let Some(i) = self.side.archived.len().checked_sub(k + 1) { self.side.archived.remove(i); self.save_archived(); },
            Some(sb::Hit::Filter(f)) => self.side.filter = f,
            Some(sb::Hit::Row(j)) => { let e = &ents[j]; self.side.focus = false; self.active_tab = e.tab; self.tabs[e.tab].active = e.pane; }
            Some(sb::Hit::Launch(i)) if self.side.installed[i] => { self.side.focus = false; self.launch(i); }
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
    /// Ruta o imagen bajo el ratón en el panel activo (une las filas partidas por ancho).
    fn path_under_mouse(&mut self) -> Option<(std::path::PathBuf, Link)> {
        let cell = self.cell();
        let off = self.pane().session.term.lock().unwrap().grid().display_offset();
        let row = self.point(off).line;
        let p = self.pane();
        let t = p.session.term.lock().unwrap();
        let g = t.grid();
        let cols = t.columns();
        let wraps = |l: Line| g[l][Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        let (mut first, mut last) = (row, row);
        while first > t.topmost_line() && wraps(first - 1) { first = first - 1; }
        while last < t.bottommost_line() && wraps(last) { last = last + 1; }
        let line: Vec<char> = (first.0..=last.0).flat_map(|l| (0..cols).map(move |c| (l, c))).map(|(l, c)| g[Line(l)][Column(c)].c).collect();
        let col = (row.0 - first.0) as usize * cols + cell.0;
        drop(t);
        let home = std::env::var("HOME").unwrap_or_default();
        let (f, a, z) = p.token.as_deref().and_then(|s| claude_image_at(&line, col, s, &home)).or_else(|| path_at(&line, col, &p.dir, &home))?;
        let start = Point::new(Line(first.0 + (a / cols) as i32), Column(a % cols));
        Some((f, Link { pane: p.id, start, text: line[a..z].to_vec() }))
    }
    /// Abre o cierra la vista previa según lo que haya bajo el ratón; solo mira al cambiar de celda.
    fn hover_peek(&mut self) {
        let m = self.mouse;
        let over = !self.uv.open && self.pal.is_none() && self.pane().anchor.is_none() && !self.mouse_fwd && self.pane().has(m.0, m.1);
        if !over { self.peek.close(); return; }
        let at = (self.pane().id, self.cell());
        if self.peek.at == Some(at) { return; }
        self.peek.at = Some(at);
        let found = self.path_under_mouse();
        let changed = found.is_some() || self.peek.link.is_some();
        let (file, link) = found.unzip();
        self.peek.link = link;
        // shortcut: solo imágenes; Windows no trae miniaturas de documentos desde consola.
        match file.filter(|f| is_image(f)) { Some(f) => self.peek.show(f), None => self.peek.hide() }
        if changed { self.redraw(); }
    }
    /// Semáforo: quién pide respuesta y quién terminó. Avisa de los que no estás mirando.
    fn update_lights(&mut self) {
        let now = Instant::now();
        let mut changed = false;
        for (ti, t) in self.tabs.iter_mut().enumerate() {
            for (pi, p) in t.panes.iter_mut().enumerate() {
                let title = p.session.title.lock().unwrap().clone();
                let rows = if p.agent == sb::Agent::Shell { vec![] } else { live_rows(&p.session.term.lock().unwrap()) };
                let ctx = if p.agent == sb::Agent::Claude { rows.iter().rev().find_map(|r| ctx_of(r)).map(|c| c.0).or(p.ctx) } else { None };
                changed |= ctx != p.ctx;
                p.ctx = ctx;
                // Claude lo dice él mismo; para los demás (o si no lo dice) se deduce de la pantalla y el título.
                let said = (p.agent == sb::Agent::Claude).then(|| win::fg_pid(&p.session).and_then(claude_status)).flatten();
                let (asking, working) = said.unwrap_or_else(|| {
                    let asking = sb::asking(&rows);
                    (asking, !asking && (sb::interruptible(&rows) || sb::status(p.agent, p.had_agent, now - p.last_out, &title) == sb::Status::Working))
                });
                let watching = self.focused && ti == self.active_tab && pi == t.active;
                let was = (p.asking, p.done, p.working.is_some());
                let who = p.agent.name().unwrap_or("El agente");
                if asking {
                    // Preguntar no es terminar: se olvida desde cuándo trabajaba.
                    p.working = None;
                    if !p.asking && !watching { notify("Ónix", &format!("{who} necesita tu respuesta en {}", p.project)); }
                // shortcut: el estado de los agentes sin título animado sale de si escriben; una pausa larga también avisa.
                } else if finished(&mut p.working, working, now) && !watching {
                    p.done = true;
                    notify("Ónix", &format!("{who} terminó en {}", p.project));
                }
                p.asking = asking;
                p.done &= !watching && !working && !asking;
                changed |= was != (p.asking, p.done, p.working.is_some());
            }
        }
        if changed { self.redraw(); }
    }
    /// Cierra los Claude de pestañas ocultas que llevan rato esperando; la pestaña queda lista para retomarlos.
    fn sleep_idle(&mut self) {
        let after = std::env::var("MI_TERMINAL_SLEEP_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(SLEEP_AFTER);
        let now = Instant::now();
        for ti in 0..self.tabs.len() {
            for pi in 0..self.tabs[ti].panes.len() {
                let p = &mut self.tabs[ti].panes[pi];
                if p.asleep || p.asking || p.agent != sb::Agent::Claude { continue; }
                let title = p.session.title.lock().unwrap().clone();
                if ti == self.active_tab || p.working.is_some() || sb::status(p.agent, p.had_agent, now - p.last_out, &title) == sb::Status::Working { p.last_busy = now; continue; }
                if after == 0 || (now - p.last_busy).as_secs() < after { continue; }
                p.refresh();
                let (Some(pid), Some(id)) = (win::fg_pid(&p.session), p.token.clone()) else { continue };
                if claude_session(pid).as_deref() != Some(id.as_str()) || !has_transcript(&id) || !idle_prompt(&screen_rows(&p.session)) { continue; }
                let ps = win::ps_list();
                if busy_children(&ps, pid) { continue; }
                let Some(resume) = resume_cmd(&id, false) else { continue };
                let cmd = shell_cmd(format!("{SLEEP_PS}{}", resume.last().cloned().unwrap_or_default()));
                // Id nuevo: el aviso de salida de la sesión vieja ya no cierra este panel.
                let new = self.next_id;
                self.next_id += 1;
                let (p1, p2) = (self.proxy.clone(), self.proxy.clone());
                let session = Session::spawn(Some(cmd), Some(self.tabs[ti].panes[pi].dir.clone().into()),
                    move || { let _ = p1.send_event(Ev::Out(new)); }, move || { let _ = p2.send_event(Ev::Exit(new)); });
                let p = &mut self.tabs[ti].panes[pi];
                // SAFETY: SIGTERM (15) a un pid cuya sesión se acaba de comprobar.
                unsafe { kill(pid, 15) };
                session.resize(p.cols.max(2), p.rows.max(2));
                p.session = session;
                (p.id, p.asleep, p.agent, p.had_agent, p.anchor) = (new, true, sb::Agent::Shell, false, None);
                p.done_rows.iter_mut().for_each(|r| *r = None);
                p.drawn = None;
                self.redraw();
            }
        }
    }
    fn wake_visible(&mut self) {
        let Some(t) = self.tabs.get_mut(self.active_tab) else { return };
        for p in t.panes.iter_mut().filter(|p| p.asleep) {
            p.session.send(b"\r");
            (p.asleep, p.last_busy) = (false, Instant::now());
        }
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
        self.last_input = Instant::now();
        self.redraw();
    }
    /// Celda (col, fila) de pantalla bajo el ratón en el panel activo.
    fn cell(&mut self) -> (usize, usize) {
        let (m, g) = (self.mouse, self.g.as_ref().unwrap());
        let (cell, pad) = (g.cell, PAD * g.scale);
        let p = self.pane();
        let (c, r) = mouse_cell((m.0, m.1 - p.dy), p.rect, cell, pad, p.cols, p.rows);
        (c, unmerge(r, p.merged).saturating_sub(p.gap))
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
    /// Al salir (cierre de ventana, Cmd+Q o última pestaña) guarda las pestañas.
    fn exiting(&mut self, _: &ActiveEventLoop) {
        for t in &mut self.tabs { for p in &mut t.panes { p.refresh(); } }
        self.save_tabs();
        if let Some(g) = &self.g { save_window(&g.window); }
    }
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.next_frame.is_some_and(|t| Instant::now() >= t) { self.next_frame = None; self.redraw(); }
        el.set_control_flow(self.next_frame.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
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
            Ev::Usage => self.redraw(),
            Ev::Out(id) => {
                let at = self.active_tab;
                for (ti, t) in self.tabs.iter_mut().enumerate() {
                    let act = t.active;
                    for (pi, p) in t.panes.iter_mut().enumerate().filter(|(_, p)| p.id == id) {
                        p.last_out = Instant::now();
                        p.unread |= !(ti == at && pi == act);
                    }
                }
                // Un chorro de salida redibuja ~30 veces por segundo (60 justo tras escribir; 5 si es de una pestaña oculta).
                let seen = self.tabs.get(at).is_some_and(|t| t.panes.iter().any(|p| p.id == id));
                let due = self.last_draw + std::time::Duration::from_millis(match (seen, self.last_input.elapsed().as_millis() < 150) { (true, true) => 16, (true, false) => 33, _ => 200 });
                if Instant::now() >= due { self.redraw(); } else { self.next_frame = Some(self.next_frame.map_or(due, |t| t.min(due))); }
            }
            Ev::Tick => {
                // Oculta la barra, se refresca cada 2 ticks y solo se redibuja si cambió agente o proyecto.
                self.ticks = self.ticks.wrapping_add(1);
                // Sin repintados hace rato: se suelta la superficie que no se ve (~23 MB a pantalla completa).
                if let Some(g) = &mut self.g { g.screen.release_idle(); }
                if self.ticks % 30 == 0 { self.sleep_idle(); }
                self.update_lights();
                // Autoguardado: si la app se cae, las pestañas siguen ahí.
                if self.ticks % 5 == 0 { self.save_tabs(); if let Some(g) = &self.g { save_window(&g.window); } }
                // Agentes instalados después de abrir la app: se vuelve a mirar cada 10 ticks.
                if self.ticks % 10 == 0 {
                    let now = sb::LAUNCH.map(|(_, b)| sb::on_path(b));
                    if now != self.side.installed { self.side.installed = now; self.redraw(); }
                }
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
                g.w = z.width.max(1);
                g.h = z.height.max(1);
                g.screen.resize(g.w as usize, g.h as usize, g.window.scale_factor());
                self.sync_size(true);
                if self.pal.is_some() { self.show_palette(); }
                self.redraw();
            }
            // Ventana tapada o minimizada: se sueltan las superficies y se rehacen al volver.
            WindowEvent::Occluded(hidden) => {
                self.hidden = hidden;
                self.side.mon.set(self.side.lists.iter().any(|l| l.open) && !hidden);
                if hidden { self.peek.close(); }
                if hidden { self.g.as_mut().unwrap().screen.release_all(); } else { self.redraw(); }
            }
            WindowEvent::RedrawRequested => {
                if self.hidden { self.next_frame = None; return; }
                self.wake_visible();
                self.last_draw = Instant::now();
                let t = &mut self.tabs[self.active_tab];
                let a = t.active;
                t.panes[a].unread = false;
                if self.focused { t.panes[a].done = false; }
                let (hi, bh) = (bar_hit(self.mouse, self.tabs.len(), g_w(self.g.as_ref().unwrap()), self.g.as_ref().unwrap().scale, bar_left(self.side.visible, self.g.as_ref().unwrap().scale)), self.mq);
                // Reinicia el reloj al cambiar de pestaña bajo el ratón.
                let hi = match hi { Some(BarHit::Tab(i) | BarHit::Close(i)) => Some(i), _ => None };
                if hi != bh.0 { self.mq = (hi, Instant::now()); }
                let mt = self.mq.1.elapsed().as_secs_f32();
                // Redibujo continuo solo mientras el carrusel está activo (hover sobre título largo).
                let ovl = if self.uv.open {
                    let (a, sc, m, range) = (self.area(), self.g.as_ref().unwrap().scale, self.mouse, self.uv.range);
                    let (rs, tx, lo) = usage_view::build(a, sc, self.uv.stats(), range, m);
                    Some((a, rs, tx, lo))
                } else { None };
                let t0 = Instant::now();
                let animating = draw(self.g.as_mut().unwrap(), &mut self.tabs, self.active_tab, self.mouse, &self.side, mt, ovl, self.peek.link.as_ref());
                if debug_on() { eprintln!("[mt] cuadro {:.3} ms", t0.elapsed().as_secs_f64() * 1000.0); }
                // Sin esto se redibujaba a la frecuencia de la pantalla. Carrusel y onda de puntos: ~30 fps.
                self.next_frame = animating.then(|| {
                    Instant::now() + std::time::Duration::from_millis(33)
                });
                if self.g.as_ref().unwrap().busy { self.next_frame = Some(Instant::now() + std::time::Duration::from_millis(4)); }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.peek.close();
                let (ctrl, shift, alt) = (self.mods.control_key(), self.mods.shift_key(), self.mods.alt_key());
                let letter = match event.logical_key.as_ref() { Key::Character(s) => s.to_lowercase(), _ => String::new() };
                let selected = self.pane().session.term.lock().unwrap().selection.is_some();
                // Atajos de la app: Ctrl+Shift; además Ctrl+V, Ctrl+número y Ctrl+C si hay algo seleccionado.
                let ports = self.side.col_focus && self.side.kf == 2 && self.side.lists[1].open;
                let cmd = ctrl && !alt && (shift || letter == "v" || (letter == "c" && (selected || ports)) || matches!(letter.as_str(), "+" | "=" | "-" | "0") || matches!(letter.as_str(), "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
                if cmd && letter == "u" { self.toggle_usage(); return; }
                // Con Agent usage abierta, esc la cierra y el resto de teclas sin ⌘ no llega a la terminal tapada.
                if self.uv.open && !cmd {
                    if event.logical_key == Key::Named(NamedKey::Escape) { self.toggle_usage(); }
                    return;
                }
                if ctrl && event.logical_key == Key::Named(NamedKey::Tab) {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    self.redraw();
                    return;
                }
                if cmd && letter == "k" {
                    if self.pal.is_some() { self.close_palette(); } else { self.open_palette(); }
                    return;
                }
                // AltGr llega como Ctrl+Alt y escribe texto: no cuenta como atajo.
                let typing = !cmd && (!ctrl || alt);
                let modifier = matches!(event.logical_key, Key::Named(NamedKey::Control | NamedKey::Shift | NamedKey::Alt));
                if self.pal.is_some() {
                    if typing { self.palette_key(el, &event); return; }
                    // Cualquier otro atajo la cierra y sigue su camino.
                    if !modifier { self.close_palette(); }
                }
                if cmd && letter == "f" {
                    if self.find.is_some() { self.close_find(); } else { self.find = Some(String::new()); self.search(false, true); }
                    return;
                }
                if self.find.is_some() {
                    if typing { self.find_key(&event); return; }
                    // Cualquier otro atajo (salvo copiar lo hallado) cierra la búsqueda.
                    if cmd && letter != "c" && !modifier { self.close_find(); }
                }
                // El teclado va al panel de la columna con foco; con Ctrl solo se queda Ctrl+⌫ (y Ctrl+C en Ports).
                if let Some(k) = (0..2).find(|&k| self.side.col_focus && self.side.kf == k + 1 && self.side.lists[k].open) {
                    let own = (!cmd && !ctrl) || event.logical_key == Key::Named(NamedKey::Backspace) || (k == 1 && cmd && letter == "c");
                    if own { self.procs_key(k, &event); return; }
                }
                if !cmd && !ctrl && self.side.col_focus && self.side.kf == 0 && self.side.agents_open && !self.side.searching { self.agents_key(&event); return; }
                if !cmd && !ctrl && self.side_key(&event) { return; }
                if cmd {
                    let key = if letter.is_empty() { event.logical_key.clone() } else { Key::Character(letter.as_str().into()) };
                    match key.as_ref() {
                        Key::Character("b" | "\\" | "|") => self.toggle_side(),
                        Key::Character("g") => self.toggle_agents(),
                        Key::Character("p") => self.toggle_list(0),
                        Key::Character("l") => self.toggle_list(1),
                        Key::Character("j") => self.go_attention(),
                        Key::Character("=" | "+") => self.set_font(Some(1.0)),
                        Key::Character("-" | "_") => self.set_font(Some(-1.0)),
                        Key::Character("0") => self.set_font(None),
                        Key::Character("c") => {
                            if let Some(s) = selected_text(&self.pane().session.term.lock().unwrap()) { pbcopy(&s); }
                            self.pane().session.term.lock().unwrap().selection = None;
                            self.redraw();
                        }
                        Key::Character("a") => {
                            let p = self.pane();
                            let mut t = p.session.term.lock().unwrap();
                            select_input(&mut t);
                            p.picked = span(&t);
                            drop(t);
                            self.redraw();
                        }
                        // Ctrl+_ es "deshacer" en zsh y en Claude Code: quita de una vez lo recién pegado.
                        Key::Character("z") => self.send_input(&[0x1f]),
                        Key::Character("v") => {
                            // Portapapeles sin texto: si es una imagen, se guarda y se pega su ruta.
                            let mut t = pbpaste();
                            if t.is_empty() { t = clipboard_image().map(|f| term_core::drop_path_text(&f)).unwrap_or_default(); }
                            if !t.is_empty() {
                                let br = self.pane().session.term.lock().unwrap().mode().contains(TermMode::BRACKETED_PASTE);
                                self.send_input(&paste_bytes(&t, br));
                            }
                        }
                        Key::Named(NamedKey::Home) => { self.pane().session.term.lock().unwrap().scroll_display(Scroll::Top); self.redraw(); }
                        Key::Named(NamedKey::End) => { self.pane().session.term.lock().unwrap().scroll_display(Scroll::Bottom); self.redraw(); }
                        Key::Character("t") => self.new_tab(),
                        Key::Character("o") => self.pick_folder(),
                        Key::Character("w") => { let (ti, pi) = (self.active_tab, self.tabs[self.active_tab].active); self.close_pane(el, ti, pi); }
                        Key::Character("d") => self.split(),
                        Key::Character("[" | "]" | "{" | "}") => {
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
                // Borrar justo tras ⌘A quita todo lo escrito, no solo un carácter.
                if matches!(event.logical_key, Key::Named(NamedKey::Backspace | NamedKey::Delete)) {
                    let p = self.pane();
                    let picked = p.picked.take();
                    let all = {
                        let t = p.session.term.lock().unwrap();
                        selected_text(&t).filter(|_| picked.is_some() && span(&t) == picked)
                            .map(|s| erase_bytes(s.chars().count(), t.mode().contains(TermMode::APP_CURSOR)))
                    };
                    if let Some(b) = all { self.send_input(&b); return; }
                }
                // AltGr llega como Ctrl+Alt: en teclados en español escribe @, \, etc.
                if ctrl && alt { if let Some(t) = event.text.as_ref().filter(|t| !t.chars().any(char::is_control)) { self.send_input(t.as_bytes()); return; } }
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
                self.peek.close();
                if self.pal.is_some() { return; }
                let ch = self.g.as_ref().unwrap().cell.1;
                let lines = take_lines(&mut self.scroll_acc, match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / ch,
                });
                let m = self.mouse;
                if let (true, Some(i)) = (lines != 0, self.tabs[self.active_tab].panes.iter().position(|p| p.has(m.0, m.1))) {
                    let (g, shift) = (self.g.as_ref().unwrap(), self.mods.shift_key());
                    let p = &self.tabs[self.active_tab].panes[i];
                    let cell = mouse_cell((m.0, m.1 - p.dy), p.rect, g.cell, PAD * g.scale, p.cols, p.rows);
                    let mode = *p.session.term.lock().unwrap().mode();
                    match wheel_bytes(lines, cell, mode).filter(|_| !shift) {
                        Some(b) => p.session.send(&b),
                        None => p.session.term.lock().unwrap().scroll_display(Scroll::Delta(lines)),
                    }
                    self.redraw();
                }
            }
            // Sin esto el carrusel seguiría girando con el ratón fuera de la ventana.
            WindowEvent::CursorLeft { .. } => { self.mouse = (-1.0, -1.0); self.peek.close(); self.redraw(); }
            WindowEvent::Focused(false) => { self.focused = false; self.peek.close(); }
            WindowEvent::Focused(true) => self.focused = true,
            WindowEvent::CursorMoved { position, .. } => {
                let bar_h = TABBAR_H * self.g.as_ref().unwrap().scale;
                let was_bar = self.mouse.1 < bar_h;
                self.mouse = (position.x as f32, position.y as f32);
                if was_bar || self.mouse.1 < bar_h || self.uv.open { self.redraw(); }
                if self.drag_sep {
                    let (a, sep) = (self.area(), self.sep());
                    let t = &mut self.tabs[self.active_tab];
                    t.split = ((self.mouse.0 - a.0 - sep / 2.0) / (a.2 - sep)).clamp(0.1, 0.9);
                    self.sync_size(false);
                    self.redraw();
                    return;
                }
                self.hover_peek();
                let icon = if self.on_sep() { CursorIcon::ColResize } else if self.peek.link.is_some() { CursorIcon::Pointer } else { CursorIcon::Default };
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
                self.peek.close();
                if state == ElementState::Released {
                    self.drag_sep = false;
                    // El soltar del clic que usó la paleta no es de la terminal.
                    if self.pal.is_some() { return; }
                    let on_side = self.side.visible && self.mouse.0 < sb::SIDE_W * self.g.as_ref().unwrap().scale;
                    if on_side || self.column().iter().any(|r| r.is_some_and(|r| inside(r, self.mouse))) { return; }
                    if self.uv.open && inside(self.area(), self.mouse) { return; }
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
                    let ok = off == 0 && a.line.0 >= 0 && !sel && hit && !mode.intersects(TermMode::ALT_SCREEN | TermMode::MOUSE_MODE)
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
                if self.pal.is_some() { self.palette_click(el); return; }
                let sc = self.g.as_ref().unwrap().scale;
                if inside((BAR_LEFT * sc, 0.0, 28.0 * sc, TABBAR_H * sc), self.mouse) { self.toggle_side(); return; }
                let wpx = self.g.as_ref().unwrap().w as f32;
                if inside(bar_btn(wpx, sc, BTN_PULSE), self.mouse) { self.toggle_list(0); return; }
                if inside(bar_btn(wpx, sc, BTN_ROBOT), self.mouse) { self.toggle_agents(); return; }
                if inside(bar_btn(wpx, sc, BTN_GLOBE), self.mouse) { self.toggle_list(1); return; }
                { if self.column().iter().any(|r| r.is_some_and(|r| inside(r, self.mouse))) { self.col_click(); return; } }
                if self.uv.open && inside(self.area(), self.mouse) {
                    match usage_view::hit_at(self.area(), sc, self.mouse) {
                        Some(usage_view::Hit::Close) => self.toggle_usage(),
                        Some(usage_view::Hit::Range(i)) => { self.uv.range = i; self.redraw(); }
                        None => {}
                    }
                    return;
                }
                if self.side.visible && self.mouse.0 < sb::SIDE_W * sc { self.side.col_focus = false; self.side_click(); return; }
                self.side.focus = false;
                self.side.col_focus = false;
                self.side.searching = false;
                if self.mouse.1 < TABBAR_H * sc {
                    let w = self.g.as_ref().unwrap().w as f32;
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
                // Clic en una ruta o [Image #N]: la muestra en su carpeta (Finder). ⌘+clic abre imagen o documento. Shift = seleccionar texto.
                if !shift {
                    if let Some((f, _)) = self.path_under_mouse() {
                        win::open(&f, self.mods.control_key() && previewable(&f));
                        return;
                    }
                }
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

fn pbcopy(s: &str) { win::copy(s) }

const IMG_EXT: [&str; 8] = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".heic", ".bmp", ".tiff"];
/// Documentos con vista previa. Lista cerrada: el clic nunca abre algo ejecutable.
const DOC_EXT: [&str; 12] = [".pdf", ".docx", ".xlsx", ".pptx", ".pages", ".numbers", ".key", ".txt", ".md", ".csv", ".mp4", ".mov"];

/// Con vista previa (y que ⌘+clic puede abrir).
fn previewable(p: &std::path::Path) -> bool {
    let n = p.to_string_lossy().to_lowercase();
    p.is_file() && IMG_EXT.iter().chain(&DOC_EXT).any(|x| n.ends_with(x))
}

fn is_image(p: &std::path::Path) -> bool {
    let n = p.to_string_lossy().to_lowercase();
    IMG_EXT.iter().any(|x| n.ends_with(x))
}

/// Vista previa abierta: celda mirada, archivo y el proceso que dibuja el panel.
#[derive(Default)]
struct Peek { at: Option<(u64, (usize, usize))>, link: Option<Link>, shown: Option<(std::path::PathBuf, std::process::Child)> }

impl Peek {
    fn show(&mut self, file: std::path::PathBuf) {
        if self.shown.as_ref().is_some_and(|(f, _)| *f == file) { return; }
        self.hide();
        self.shown = win::powershell(win::PEEK).env("ONIX_PEEK", &file).stdout(Stdio::null()).spawn().ok().map(|c| (file, c));
    }
    fn hide(&mut self) {
        if let Some((_, mut c)) = self.shown.take() { let _ = c.kill(); let _ = c.wait(); }
    }
    /// Cierra y olvida la celda: el siguiente movimiento vuelve a mirar.
    fn close(&mut self) { self.hide(); self.at = None; self.link = None; }
}

impl Drop for Peek { fn drop(&mut self) { self.hide(); } }

/// Archivo o carpeta existente cuya ruta cubre la columna `col` de `line`, con su tramo [inicio, fin): absoluta, `~/`, `file://` o
/// relativa a `dir`, con espacios (o `\ `). Debe llevar `/` o extensión, para no enlazar palabras sueltas.
fn path_at(line: &[char], col: usize, dir: &str, home: &str) -> Option<(std::path::PathBuf, usize, usize)> {
    // Del final más lejano al más cercano y de izquierda a derecha: gana la ruta existente más larga.
    for end in (col + 1..=line.len().min(col + 256)).rev() {
        if line.get(end).is_some_and(|c| c.is_alphanumeric()) || line[end - 1].is_whitespace() { continue; }
        for start in (end.saturating_sub(256)..=col).filter(|&i| i == 0 || line[i - 1].is_whitespace() || "'\"`([{<=:|,│@".contains(line[i - 1])) {
            let raw: String = line[start..end].iter().collect();
            let t = match raw.strip_prefix("file://") { Some(u) => u.replace("%20", " "), None => raw.replace("\\ ", " ").replace('\\', "/") };
            if t.is_empty() || t.starts_with(' ') || !(t.contains('/') || std::path::Path::new(&t).extension().is_some()) { continue; }
            let path = match t.strip_prefix("~/") { Some(r) => std::path::Path::new(home).join(r), None => std::path::Path::new(dir).join(&t) };
            if path.is_absolute() && path.file_name().is_some() && path.exists() { return Some((path, start, end)); }
        }
    }
    None
}

const IMG_MARK: Rgb = [0xd7, 0x77, 0x57];

/// Pinta las marcas `[Image #N]` para distinguirlas del texto. Sobre fondo claro (cursor encima) no se toca.
fn paint_images(row: &mut [(usize, usize, char, Rgb, Rgb, bool)]) {
    const HEAD: &str = "[Image #";
    let mut i = 0;
    while i + HEAD.len() < row.len() {
        if !HEAD.chars().zip(&row[i..]).all(|(h, c)| c.2 == h) { i += 1; continue; }
        let digits = row[i + HEAD.len()..].iter().take_while(|c| c.2.is_ascii_digit()).count();
        let end = i + HEAD.len() + digits;
        if digits > 0 && row.get(end).is_some_and(|c| c.2 == ']') {
            for c in row[i..=end].iter_mut().filter(|c| c.4.iter().all(|&v| v < 0x80)) { c.3 = IMG_MARK; }
        }
        i = end;
    }
}

/// Imagen pegada en Claude Code que corresponde a la marca `[Image #N]` bajo la columna `col`.
fn claude_image_at(line: &[char], col: usize, session: &str, home: &str) -> Option<(std::path::PathBuf, usize, usize)> {
    if !sb::is_uuid(session) { return None; }
    let start = (0..=col.min(line.len().checked_sub(1)?)).rev().find(|&i| line[i] == '[')?;
    let end = start + line[start..].iter().position(|&c| c == ']')?;
    if col > end { return None; }
    let mark: String = line[start + 1..end].iter().collect();
    let n: u32 = mark.strip_prefix("Image #")?.parse().ok()?;
    // Claude Code las guarda en la carpeta temporal (`claude…/<proyecto>/<sesión>/images`); versiones anteriores, en `image-cache`.
    let temp = std::fs::read_dir(std::env::temp_dir()).into_iter().flatten().flatten()
        .filter(|d| d.file_name().to_string_lossy().starts_with("claude"))
        .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten());
    let old = std::path::Path::new(home).join(".claude/image-cache").join(session);
    temp.map(|d| d.path().join(session).join("images")).chain([old])
        .flat_map(|d| IMG_EXT.iter().map(move |x| d.join(format!("{n}{x}"))))
        .find(|p| p.is_file()).map(|p| (p, start, end + 1))
}

/// Si el portapapeles es una imagen, la guarda como PNG en la caché de la app y devuelve su ruta.
fn clipboard_image() -> Option<String> {
    let dir = std::path::Path::new(&std::env::var_os("HOME")?).join("AppData/Local/Onix/cache");
    std::fs::create_dir_all(&dir).ok()?;
    let ms = SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
    let file = dir.join(format!("pegada-{ms}.png"));
    let ok = win::powershell("Add-Type -AssemblyName System.Windows.Forms, System.Drawing; $i = [System.Windows.Forms.Clipboard]::GetImage(); if (-not $i) { exit 1 }; $i.Save($env:ONIX_IMG, [System.Drawing.Imaging.ImageFormat]::Png)")
        .env("ONIX_IMG", &file).status().ok()?.success();
    ok.then(|| file.to_string_lossy().into_owned())
}

fn pbpaste() -> String { win::paste() }

fn main() {
    // El resto de la app usa HOME y rutas con `/`; se fija antes de crear hilos.
    if let Ok(h) = std::env::var("USERPROFILE") { std::env::set_var("HOME", win::slashes(&h)); }
    let el = EventLoop::<Ev>::with_user_event().build().unwrap();
    el.set_control_flow(ControlFlow::Wait);
    let tick = el.create_proxy();
    std::thread::spawn(move || while tick.send_event(Ev::Tick).is_ok() { std::thread::sleep(std::time::Duration::from_secs(1)); });
    let side = sb::Side { col_focus: false, lists: Default::default(), kf: 0, psort: procs::Sort::Cpu, mon: procs::Mon::start(), agents_open: false, agents_sel: 0, stop_at: None, visible: true, focus: false, searching: false, query: String::new(), filter: sb::Filter::All,
        archived: data_file("archived.json").and_then(|f| std::fs::read_to_string(f).ok()).map(|t| sb::archived_from_json(&t)).unwrap_or_default(), open_archived: false, installed: sb::LAUNCH.map(|(_, b)| sb::on_path(b)) };
    let mut app = App { uv: usage_view::View::new(), g: None, tabs: vec![], active_tab: 0, next_id: 0, proxy: el.create_proxy(), last_bar_click: None, last_sep_click: None, ticks: 0, restore: None, picking: false, focused: true, find: None, pal: None, mq: (None, Instant::now()),
        mods: ModifiersState::empty(), mouse: (0.0, 0.0), scroll_acc: 0.0, drag_sep: false, mouse_fwd: false, hidden: false, last_draw: Instant::now(), last_input: Instant::now(), next_frame: None, peek: Peek::default(), side };
    // Reabre las pestañas guardadas (cwd existente); sin archivo, o con MI_TERMINAL_CMD, una pestaña en HOME.
    let saved = data_file("tabs.txt")
        .and_then(|f| std::fs::read_to_string(f).ok()).map(|t| parse_tabs(&t)).unwrap_or_default();
    let dirs: Vec<_> = saved.into_iter().map(|(d, id)| (std::path::PathBuf::from(d), id)).filter(|(d, _)| std::fs::read_dir(d).is_ok()).collect();
    // Solo la primera pestaña de cada (agente, carpeta) continúa "la última" sesión.
    let mut seen = std::collections::HashSet::new();
    if dirs.is_empty() { app.new_tab(); } else { for (d, id) in dirs {
        // Viva fuera de la app (otra terminal): solo el cwd, sin --resume.
        match id.filter(|i| !alive_now(i)) {
            Some(i) => { let first = seen.insert((i.clone(), d.clone())); app.open_saved(&i, d, first) }
            None => app.new_tab_with(None, Some(d)),
        }
    } }
    // Solo para pruebas: escribe este comando al PTY al inicio.
    if let Ok(c) = std::env::var("MI_TERMINAL_CMD") { app.pane().session.send(format!("{c}\n").as_bytes()); }
    el.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    #[test]
    fn el_punto_late() {
        assert_eq!(super::pulse([200, 100, 0], 0.0), [200, 100, 0]);
        assert_eq!(super::pulse([200, 100, 0], 0.6), [50, 25, 0]);
    }
    #[test]
    fn estado_que_dice_claude() {
        assert_eq!(super::claude_state(r#"{"status":"busy"}"#), Some((false, true)));
        assert_eq!(super::claude_state(r#"{"status":"waiting","waitingFor":"input needed"}"#), Some((true, false)));
        assert_eq!(super::claude_state(r#"{"status":"idle"}"#), Some((false, false)));
        assert_eq!(super::claude_state(r#"{"pid":1}"#), None);
    }
    #[test]
    fn pie_cuadro_y_esquinas() {
        let mk = |t: &str| -> Vec<(usize, usize, char, u8, u8, bool)> { format!("{t:<30}").chars().enumerate().map(|(i, c)| (i, 1, c, 0, 0, false)).collect() };
        let txt = |r: &[(usize, usize, char, u8, u8, bool)]| r.iter().map(|c| c.2).collect::<String>().trim_end().to_string();
        let col = [1, 2, 3, 4, 5];
        let mut g = vec![mk("  ctx 29% 5h 8%")];
        super::ctx_bar(&mut g, col);
        assert_eq!(txt(&g[0]), "  contexto ━━━───── 29% 5h 8%");
        // Sin sitio para la barra queda la cifra.
        let mut n = vec![mk("  ctx 85%   5h 8% sem 3%")];
        super::ctx_bar(&mut n, col);
        assert!(txt(&n[0]).starts_with("  contexto 85%   5h"));
        let line = "─".repeat(30);
        let b = vec![mk(&line), mk("❯ hola"), mk(&line), mk("  ▸▸ auto mode on")];
        assert_eq!(super::input_box(&b, 30), Some((0, 2)));
        assert_eq!(super::input_box(&b[1..], 30), None);
        assert_eq!(super::footer_row(&txt(&b[3])), Some(false));
        assert_eq!(super::footer_row("  contexto 3%"), Some(true));
        assert_eq!(super::footer_row("el modo mode on"), None);
        let mut out = vec![];
        super::round(&mut out, (0.0, 0.0, 20.0, 20.0), 6.0, [9, 9, 9], [0, 0, 0], true);
        assert!(out.iter().all(|r| r.0 >= 0.0 && r.1 >= 0.0 && r.0 + r.2 <= 20.0 && r.1 + r.3 <= 20.0));
    }
    #[test]
    fn lee_la_ventana_guardada() {
        assert_eq!(super::parse_window("10 38 1512 865\n"), Some([10.0, 38.0, 1512.0, 865.0]));
        for malo in ["", "1 2 3", "0 0 10 10", "0 0 NaN 600", "a b c d", "0 0 960 600 1"] { assert_eq!(super::parse_window(malo), None, "{malo}"); }
    }
    #[test]
    fn rellena_el_hueco_de_abajo_con_historial() {
        use term_core::alacritty_terminal::{event::VoidListener, term::Config, vte::ansi::Processor};
        let mut t = super::Term::new(Config::default(), &term_core::Size(10, 5), VoidListener);
        let mut p: Processor = Processor::new();
        // Sin historial no hay con qué rellenar.
        p.advance(&mut t, b"a\r\nb");
        assert_eq!(super::bottom_gap(&t), 0);
        p.advance(&mut t, b"\r\nc\r\nd\r\ne\r\nf\r\ng\r\nh");
        assert_eq!(super::bottom_gap(&t), 0);
        // Sube dos filas y borra hacia abajo, como un diálogo que se cierra.
        p.advance(&mut t, b"\x1b[2A\r\x1b[J");
        assert_eq!(super::bottom_gap(&t), 2);
        // Al subir con la rueda se mantiene; en lo más alto no queda historial que sumar.
        t.scroll_display(super::Scroll::Delta(1));
        assert_eq!(super::view_gap(&t), 2);
        t.scroll_display(super::Scroll::Top);
        assert_eq!(super::view_gap(&t), 0);
        // Tras limpiar la pantalla no vuelve el chat viejo, ni mientras crece el nuevo.
        t.scroll_display(super::Scroll::Bottom);
        let mut cleared = false;
        p.advance(&mut t, b"\x1b[H\x1b[Jn");
        assert_eq!(super::fill_gap(&t, &mut cleared), 0);
        p.advance(&mut t, b"\r\n1\r\n2\r\n3");
        assert_eq!(super::fill_gap(&t, &mut cleared), 0);
        p.advance(&mut t, b"\r\n4\r\n5\x1b[1A\r\x1b[J");
        assert_eq!(super::fill_gap(&t, &mut cleared), 0);
        p.advance(&mut t, b"\r\n6\r\n7");
        assert_eq!(super::fill_gap(&t, &mut cleared), 0);
        assert!(!cleared);
    }
    #[test]
    fn clear_de_claude_borra_el_historial() {
        use term_core::alacritty_terminal::{event::VoidListener, grid::Dimensions, term::Config, vte::ansi::Processor};
        let mut t = super::Term::new(Config::default(), &term_core::Size(30, 5), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, "1\r\n2\r\n3\r\n4\r\n5\r\n❯ /clear\r\n7".as_bytes());
        super::forget_on_clear(&mut t);
        assert!(t.grid().history_size() > 0); // sin la cabecera no se toca
        p.advance(&mut t, "\x1b[H\x1b[J$ claude\r\n ▐ Claude Code v2\r\n\r\n❯ /clear\r\nfin".as_bytes());
        super::forget_on_clear(&mut t);
        assert_eq!(t.grid().history_size(), 0);
        // La cabecera queda arriba y el cursor sigue sobre su texto.
        assert_eq!(t.grid()[super::Line(0)][super::Column(3)].c, 'C');
        assert_eq!(t.grid().cursor.point.line.0, 3);
    }
    #[test]
    fn al_cambiar_el_ancho_no_queda_copia_vieja() {
        use term_core::alacritty_terminal::{event::VoidListener, grid::Dimensions, term::Config, vte::ansi::Processor};
        let mut t = super::Term::new(Config::default(), &term_core::Size(30, 5), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, " ▐ Claude Code v2\r\n\r\n❯ una pregunta bastante larga\r\n\r\nrespuesta".as_bytes());
        t.resize(term_core::Size(20, 5));
        assert!(t.grid().history_size() > 0);
        let mut resized = true;
        // Charla larga: sin cabecera arriba, el historial se conserva.
        p.advance(&mut t, b"\x1b[H\x1b[2Kfinal");
        super::forget_after_resize(&mut t, &mut resized);
        assert!(t.grid().history_size() > 0 && resized);
        p.advance(&mut t, "\x1b[H\x1b[2K ▐ Claude Code v2".as_bytes());
        super::forget_after_resize(&mut t, &mut resized);
        assert!(t.grid().history_size() == 0 && !resized);
    }
    #[test]
    fn cmd_a_selecciona_solo_lo_escrito() {
        use term_core::alacritty_terminal::{event::VoidListener, term::Config, vte::ansi::Processor};
        let mut t = super::Term::new(Config::default(), &term_core::Size(20, 8), VoidListener);
        let mut p: Processor = Processor::new();
        let mut pick = |t: &mut super::Term<VoidListener>, bytes: &str| { p.advance(t, bytes.as_bytes()); super::select_input(t); super::selected_text(t) };
        // Cuadro de Claude Code con dos líneas: ni el chat ni el pie.
        let claude = "chat\r\n──────\r\n❯ hola\r\n  mundo\r\n──────\r\npie\x1b[4;8H";
        assert_eq!(pick(&mut t, claude).as_deref(), Some("hola\nmundo"));
        assert!(super::span(&t).is_some());
        let b = super::erase_bytes(2, false);
        assert!(b.starts_with(b"\x1b[C") && b.iter().filter(|&&c| c == 0x7f).count() == 20);
        // Línea del shell partida por el ancho.
        assert_eq!(pick(&mut t, "\x1b[H\x1b[2J~\r\n❯ echo abcdefghijklmnopqrst").as_deref(), Some("echo abcdefghijklmnopqrst"));
        // Sugerencia atenuada, o salida sin prompt junto al cursor: nada.
        assert_eq!(pick(&mut t, "\x1b[H\x1b[2J❯ \x1b[2mTry\x1b[0m"), None);
        assert_eq!(pick(&mut t, "\x1b[H\x1b[2J❯ ls\r\nsalida"), None);
    }
    #[test]
    fn avisa_solo_tras_trabajar_un_rato() {
        let (t0, mut s) = (std::time::Instant::now(), None);
        let at = |n| t0 + std::time::Duration::from_secs(n);
        assert!(!super::finished(&mut s, false, at(0)));
        assert!(!super::finished(&mut s, true, at(1)) && !super::finished(&mut s, false, at(5))); // muy corto
        assert!(!super::finished(&mut s, true, at(10)) && !super::finished(&mut s, true, at(30)));
        assert!(super::finished(&mut s, false, at(31)) && !super::finished(&mut s, false, at(32))); // una sola vez
    }
    #[test]
    fn busca_en_el_historial() {
        use term_core::alacritty_terminal::{event::VoidListener, term::Config, vte::ansi::Processor};
        let mut t = super::Term::new(Config::default(), &term_core::Size(20, 3), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, b"Error uno\r\nbien\r\notro error\r\nfin");
        let last = super::find_match(&t, "ERROR", None, true).unwrap();
        assert_eq!((last.0.line.0, last.0.column.0, last.1.column.0), (1, 5, 9));
        let prev = super::find_match(&t, "error", Some(last.0), true).unwrap();
        assert_eq!(prev.0.line.0, -1); // en el historial
        assert_eq!(super::find_match(&t, "error", Some(prev.0), true), Some(last)); // da la vuelta
        assert_eq!(super::find_match(&t, "nada", None, true), None);
        assert_eq!((super::parse_font("16\n"), super::parse_font("999"), super::parse_font("x")), (16.0, 13.0, 13.0));
    }
    #[test]
    fn colorea_la_linea_de_uso() {
        let mk = |t: &str| -> Vec<(usize, usize, char, u8, u8, bool)> { t.chars().enumerate().map(|(i, c)| (i, 1, c, 9, 0, false)).collect() };
        let mut g = vec![mk("mode on  5h ━─ 50% ↻  semana ━─ 85%")];
        super::paint_usage(&mut g, [1, 2, 3, 4, 5]);
        let c = |i: usize| g[0][i].3;
        // El texto del modo no se toca; etiqueta, lleno, vacío, cifra, reloj, y aviso desde 80 %.
        assert_eq!((c(0), c(9), c(12), c(13), c(15), c(17), c(19), c(29), c(32)), (9, 1, 3, 2, 3, 3, 1, 5, 3));
        // Aunque debajo queden filas vacías.
        let mut alto = vec![mk("5h ━─ 50%")];
        alto.extend((0..10).map(|_| mk("          ")));
        super::paint_usage(&mut alto, [1, 2, 3, 4, 5]);
        assert_eq!(alto[0][0].3, 1);
        let mut n = vec![mk("5h sin barra 50%")];
        super::paint_usage(&mut n, [1, 2, 3, 4, 5]);
        assert!(n[0].iter().all(|c| c.3 == 9));
    }
    #[test]
    fn tapa_nombre_de_sesion_en_el_borde() {
        let mk = |t: &str| -> Vec<(usize, usize, char, u8, u8, bool)> { t.chars().enumerate().map(|(i, c)| (i, 1, c, 0, 0, false)).collect() };
        let txt = |r: &[(usize, usize, char, u8, u8, bool)]| r.iter().map(|c| c.2).collect::<String>();
        let mut r = mk("─────── ahorro-tokens-x ─");
        hide_label(&mut r);
        assert_eq!(txt(&r), "─".repeat(r.len()));
        let mut r = mk("───────────────────");
        hide_label(&mut r);
        assert_eq!(txt(&r), "───────────────────");
        let mut r = mk("──── Hola mundo ──── fin");
        hide_label(&mut r);
        assert_eq!(txt(&r), "──── Hola mundo ──── fin");
    }
    #[test]
    fn vista_previa_de_documentos() {
        let f = std::env::temp_dir().join("mt-peek.pdf");
        std::fs::write(&f, "x").unwrap();
        let line: Vec<char> = format!("ver {} ya", f.display()).chars().collect();
        assert_eq!(path_at(&line, 8, "/", "/").map(|h| h.0), Some(f.clone()));
        assert!(!is_image(&f) && is_image(std::path::Path::new("a.JPG")));
        let _ = std::fs::remove_file(f);
    }
    #[test]
    fn solo_duerme_en_reposo() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(idle_prompt(&v(&["───", "❯\u{a0}   ", "───", "  ▸▸ auto mode on"])));
        assert!(!idle_prompt(&v(&["───", "❯ borrador", "───", "  ▸▸ auto mode on"])));
        assert!(!idle_prompt(&v(&["───", "❯ ", "  segunda línea", "───", "  ▸▸ auto mode on"])));
        assert!(!idle_prompt(&v(&["Do you want to proceed?", "❯ 1. Yes", "Esc to cancel"])));
        assert!(!busy_children("  7 caffeinate\n  8 /bin/zsh", 7));
        assert!(busy_children("  7 /bin/zsh", 7));
    }
    #[test]
    fn barra_junto_al_modo() {
        let mk = |t: &str, n: usize| { let mut v: Vec<_> = t.chars().map(|c| (0usize, 1usize, c, 0u8, 0u8, false)).collect(); v.resize(n, (0, 1, ' ', 0, 0, false)); v };
        let mut g = vec![mk("  5h ━── 8% · week ━── 23%", 80), mk("  ▸▸ auto mode on", 80)];
        assert_eq!(merge_status(&mut g), Some(0));
        let t = |r: &Vec<(usize, usize, char, u8, u8, bool)>| r.iter().map(|c| c.2).collect::<String>().trim_end().to_string();
        assert_eq!(g.len(), 2);
        assert_eq!(t(&g[1]), format!("  ▸▸ auto mode on{}5h ━── 8% · week ━── 23%", " ".repeat(80 - 2 - "5h ━── 8% · week ━── 23%".chars().count() - 17)));
        assert_eq!(t(&g[0]), "");
        let mut m = vec![mk("hola", 80), mk("  5h ━── 8%", 80), mk("  ▸▸ auto mode on", 80)];
        assert_eq!(merge_status(&mut m), Some(1));
        assert_eq!((t(&m[0]), t(&m[1])), ("".into(), "hola".into()));
        assert_eq!([unmerge(1, Some(1)), unmerge(2, Some(1)), unmerge(0, Some(1)), unmerge(1, None)], [0, 2, 0, 1]);
        let mut n = vec![mk("  5h ━── 8% · week ━── 23%", 40), mk("  ▸▸ auto mode on", 40)];
        merge_status(&mut n);
        assert!(t(&n[0]).starts_with("  5h"));
        let mut o = vec![mk("5h x", 40), mk("> hola", 40)];
        merge_status(&mut o);
        assert_eq!(t(&o[1]), "> hola");
    }
    #[test]
    fn oculta_aviso_de_modo() {
        let mk = |t: &str| t.chars().enumerate().map(|(i, c)| (i, 1usize, c, 0u8, 0u8, false)).collect::<Vec<_>>();
        let mut r = mk("▸▸ auto mode on (shift+tab to cycle)");
        hide_hint(&mut r);
        assert_eq!(r.iter().map(|c| c.2).collect::<String>().trim_end(), "▸▸ auto mode on");
        let mut r3 = mk("▸▸ auto mode on (shift+tab to cycle) · esc to interrupt");
        hide_hint(&mut r3);
        assert_eq!(r3.iter().map(|c| c.2).collect::<String>().trim_end(), "▸▸ auto mode on");
        let mut r2 = mk("echo shift+tab");
        hide_hint(&mut r2);
        assert_eq!(r2.iter().map(|c| c.2).collect::<String>(), "echo shift+tab");
    }
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
        assert!(super::fills((3024, 1744), (3024, 1964), 2.0));
        assert!(!super::fills((1920, 1200), (3024, 1964), 2.0));
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
        // Una carpeta por prueba (hilo): compartida, una la reescribía mientras otra la leía.
        let d = std::env::temp_dir().join(format!("mt-sess-{}-{:?}-{pid}-{name_pid}", std::process::id(), std::thread::current().id()));
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
    fn colorea_las_marcas_de_imagen() {
        let mut r: Vec<_> = "a [Image #12] [Image #x] b".chars().enumerate().map(|(i, c)| (i, 1, c, [9u8; 3], [0u8; 3], false)).collect();
        super::paint_images(&mut r);
        let painted: String = r.iter().filter(|c| c.3 == super::IMG_MARK).map(|c| c.2).collect();
        assert_eq!(painted, "[Image #12]");
    }
    #[test]
    fn imagen_bajo_el_clic() {
        let d = std::env::temp_dir().join(format!("mt-img-{}", std::process::id()));
        let id = "0a1b2c3d-1111-4222-8333-444455556666";
        std::fs::create_dir_all(d.join(".claude/image-cache").join(id)).unwrap();
        std::fs::write(d.join("mi foto.JPG"), "x").unwrap();
        std::fs::write(d.join(".claude/image-cache").join(id).join("2.png"), "x").unwrap();
        let ds = d.to_str().unwrap();
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        let hit = |s: &str, col| path_at(&c(s), col, ds, ds).map(|h| h.0);
        let want = Some(d.join("mi foto.JPG"));
        assert_eq!(hit("ver mi foto.JPG.", 8), want);
        assert_eq!(hit("(~/mi\\ foto.JPG)", 5), want);
        assert_eq!(hit(&format!("file://{ds}/mi%20foto.JPG"), 3), want);
        assert_eq!(hit("ver mi foto.JPG y mas", 18), None); // clic fuera de la ruta
        assert_eq!(hit("no hay.png", 4), None);
        std::fs::create_dir_all(d.join("src/ui")).unwrap();
        std::fs::write(d.join("src/ui/a.rs"), "x").unwrap();
        assert_eq!(path_at(&c("ver src/ui/a.rs:12 ya"), 5, ds, ds), Some((d.join("src/ui/a.rs"), 4, 15)));
        assert_eq!(hit("en src/ui hay", 4), Some(d.join("src/ui")));
        assert_eq!(hit("la carpeta src sola", 12), None); // palabra suelta: no es enlace
        assert!(claude_image_at(&c("a [Image #2] b"), 5, id, ds).is_some());
        assert!(claude_image_at(&c("a [Image #2] b"), 13, id, ds).is_none());
        assert!(claude_image_at(&c("[Image #2]"), 3, "codex", ds).is_none());
        // Ubicación actual, en la carpeta temporal.
        let tmp = std::env::temp_dir().join("claude-prueba").join(format!("mt-test-{}", std::process::id()));
        let new_id = &format!("0a1b2c3d-1111-4222-8333-{:012}", std::process::id());
        std::fs::create_dir_all(tmp.join(new_id).join("images")).unwrap();
        std::fs::write(tmp.join(new_id).join("images/7.png"), "x").unwrap();
        assert_eq!(claude_image_at(&c("[Image #7]"), 3, new_id, ds).map(|h| h.0), Some(tmp.join(new_id).join("images/7.png")));
        let _ = std::fs::remove_dir_all(tmp);
        let _ = std::fs::remove_dir_all(d);
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
        assert_eq!(resume_cmd(id, true).unwrap()[3], format!("claude --resume {id}"));
        assert_eq!(parse_tabs("C:/Users/a/x\nD:malo\n"), [n("C:/Users/a/x")]);
        assert!(resume_cmd("x;rm", true).is_none());
        assert_eq!(resume_cmd("codex", true).unwrap()[3], "codex resume --last");
        assert_eq!(resume_cmd("codex", false).unwrap()[3], "codex resume");
        assert_eq!(resume_cmd("agy", true).unwrap()[3], "agy --continue");
        assert_eq!(resume_cmd("opencode", true).unwrap()[3], "opencode --continue");
        for (a, b) in sb::LAUNCH { assert_eq!(a != sb::Agent::Claude, resume_cmd(b, true).is_some() && sb::valid_token(b)); }
        assert_eq!(parse_tabs("/j\tcodex\n"), [("/j".to_string(), Some("codex".to_string()))]);
        let a = |token: &str, cwd: &str| sb::Archived { title: "t".into(), project: "p".into(), cwd: cwd.into(), token: token.into(), at: std::time::UNIX_EPOCH };
        let json = sb::archived_to_json(&[a("agy", "/a"), a("x;rm", "/b"), a("codex", "rel")]);
        assert_eq!(sb::archived_from_json(&json), [a("agy", "/a")]);
        assert!(sb::archived_from_json("basura").is_empty());
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
        // 300 - 14*20 - 8 = 12: la última fila termina en el borde.
        assert_eq!(bottom_dy(300.0, 14, 20.0, 8.0), 12.0);
        assert_eq!(bottom_dy(10.0, 2, 20.0, 8.0), 0.0);
    }
}
