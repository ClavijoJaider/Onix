//! Barra lateral de sesiones: lógica pura (agrupación, estado, filtro, tiempo), geometría y libproc.
use std::time::Duration;

pub type R = (f32, f32, f32, f32);
/// Ancho fijo de la barra, en px lógicos.
pub const SIDE_W: f32 = 250.0;
pub const ROW_H: f32 = 46.0;
/// Renglón extra de una fila con skills cargadas.
const SKILLS_H: f32 = 16.0;
const GROUP_H: f32 = 26.0;
pub const ARCH_H: f32 = 20.0;
pub const ARCH_MAX: usize = 6;

/// Filas de la lista Archived abierta: (título a 22 car., "hace 3m"), más recientes primero.
pub fn archived_rows(a: &[Archived], now: std::time::SystemTime) -> Vec<(String, String)> {
    a.iter().rev().take(ARCH_MAX).map(|x| {
        let secs = now.duration_since(x.at).map_or(0, |d| d.as_secs());
        (clean_title(&x.title, 22), fmt_elapsed(secs))
    }).collect()
}
/// y lógica de la fila `k` de `n` filas de Archived abierta (se apilan justo sobre los íconos de lanzamiento).
pub fn arch_y(h: f32, n: usize, k: usize) -> f32 { h - 84.0 - (n - k) as f32 * ARCH_H }

/// Agente según el argv de un proceso `node`: el primer argumento tras el ejecutable que nombre un binario de LAUNCH.
pub fn agent_from_args(args: &[String]) -> Agent {
    args.iter().skip(1).find_map(|a| {
        let f = a.rsplit('/').next().unwrap_or(a).to_lowercase();
        LAUNCH.iter().find(|(_, b)| f == *b || a.to_lowercase().contains(&format!("/{b}"))).map(|(g, _)| *g)
    }).unwrap_or(Agent::Shell)
}

pub fn proc_args(pid: i32) -> Vec<String> { crate::win::proc_args(pid) }

/// Formato KERN_PROCARGS2: argc (i32) + ruta del ejecutable + NULs + argv separados por NUL.
fn parse_procargs(b: &[u8]) -> Vec<String> {
    let argc = i32::from_ne_bytes([b[0], b[1], b[2], b[3]]).max(0) as usize;
    let mut it = b[4..].split(|&c| c == 0).skip_while(|s| !s.is_empty()).skip_while(|s| s.is_empty()); // salta ruta y relleno
    let first = it.next().map(|s| String::from_utf8_lossy(s).into_owned());
    first.into_iter().chain(it.map(|s| String::from_utf8_lossy(s).into_owned())).take(argc).collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Agent { Claude, Codex, Antigravity, Grok, Opencode, Junie, Shell }

/// Íconos de lanzamiento (orden de la fila inferior) y su binario.
pub const LAUNCH: [(Agent, &str); 6] = [
    (Agent::Claude, "claude"), (Agent::Codex, "codex"), (Agent::Grok, "grok"),
    (Agent::Antigravity, "agy"), (Agent::Junie, "junie"), (Agent::Opencode, "opencode"),
];

/// Página oficial de instalación de cada CLI, en el orden de LAUNCH: se abre al tocar uno que no está.
pub const INSTALL: [&str; 6] = [
    "https://code.claude.com/docs/en/quickstart", "https://developers.openai.com/codex/cli", "https://docs.x.ai/build/overview",
    "https://antigravity.google/docs/cli/install", "https://junie.jetbrains.com/docs/junie-cli.html", "https://opencode.ai/docs",
];

impl Agent {
    /// Glifo de texto (solo Shell; los agentes llevan logo, ver `icon`).
    pub fn glyph(self) -> &'static str { "❯" }
    /// Nombre que se muestra en la pestaña ("claude", "codex", "antigravity"…); Shell no tiene.
    pub fn name(self) -> Option<&'static str> {
        if self == Agent::Antigravity { return Some("antigravity"); }
        LAUNCH.iter().find(|(g, _)| *g == self).map(|(_, b)| *b)
    }
    /// (id, máscara alfa 64x64) del logo de marca; Shell no tiene.
    pub fn icon(self) -> Option<(u16, &'static [u8])> {
        macro_rules! m { ($i:expr, $f:literal) => { Some(($i, &include_bytes!(concat!("../assets/icons/", $f, ".a8"))[..])) } }
        match self {
            Agent::Claude => m!(0, "claude"), Agent::Codex => m!(1, "openai"), Agent::Antigravity => m!(2, "googlegemini"),
            Agent::Grok => m!(3, "grok"), Agent::Opencode => m!(4, "opencode"), Agent::Junie => m!(5, "jetbrains"), Agent::Shell => None,
        }
    }
    pub fn color(self) -> [u8; 3] {
        match self {
            Agent::Claude => [0xd9, 0x77, 0x57], Agent::Antigravity => [0x8e, 0x75, 0xb2], Agent::Shell => [0x8a, 0x8a, 0x94],
            _ => [0xee, 0xee, 0xee],
        }
    }
}

/// Íconos de línea del cromo (ids de `ICON_*`), dibujados con antialias en una máscara de `n` px.
pub const ICON_NEW: u16 = 10;
pub const ICON_SEARCH: u16 = 11;
pub const ICON_ARCHIVE: u16 = 12;
pub const ICON_PANEL: u16 = 13;
pub const ICON_PULSE: u16 = 14;
pub const ICON_GLOBE: u16 = 15;
pub const ICON_FOLDER: u16 = 16;
pub const ICON_ROBOT: u16 = 17;
/// Disco relleno (insignia de estado).
pub const ICON_DOT: u16 = 18;
/// Cometa en órbita (trabajando): ids `ICON_SPIN..ICON_SPIN + SPIN_FRAMES`, una vuelta por segundo.
pub const ICON_SPIN: u16 = 20;
pub const SPIN_FRAMES: u16 = 24;

/// Trazos del ícono en una cuadrícula de 24: (segmentos [x0, y0, x1, y1], elipses [cx, cy, rx, ry]).
fn icon_shapes(id: u16) -> Option<(Vec<[f32; 4]>, Vec<[f32; 4]>)> {
    let poly = |p: &[(f32, f32)]| (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); [a.0, a.1, b.0, b.1] }).collect::<Vec<_>>();
    let open = |p: &[(f32, f32)]| p.windows(2).map(|w| [w[0].0, w[0].1, w[1].0, w[1].1]).collect::<Vec<_>>();
    Some(match id {
        ICON_NEW => ([open(&[(11.0, 5.0), (4.0, 5.0), (4.0, 20.0), (19.0, 20.0), (19.0, 13.0)]), poly(&[(16.0, 4.0), (20.0, 8.0), (12.0, 16.0), (8.0, 17.0), (8.0, 12.0)])].concat(), vec![]),
        ICON_SEARCH => (vec![[15.5, 15.5, 20.5, 20.5]], vec![[10.5, 10.5, 6.5, 6.5]]),
        ICON_ARCHIVE => ([poly(&[(3.0, 5.0), (21.0, 5.0), (21.0, 9.0), (3.0, 9.0)]), open(&[(5.0, 9.0), (5.0, 20.0), (19.0, 20.0), (19.0, 9.0)]), vec![[10.0, 13.0, 14.0, 13.0]]].concat(), vec![]),
        ICON_PANEL => ([poly(&[(3.0, 5.0), (21.0, 5.0), (21.0, 19.0), (3.0, 19.0)]), vec![[9.0, 5.0, 9.0, 19.0]]].concat(), vec![]),
        ICON_PULSE => (open(&[(2.0, 12.0), (7.0, 12.0), (10.0, 5.0), (14.0, 19.0), (17.0, 12.0), (22.0, 12.0)]), vec![]),
        ICON_GLOBE => (vec![[3.0, 12.0, 21.0, 12.0]], vec![[12.0, 12.0, 9.0, 9.0], [12.0, 12.0, 4.0, 9.0]]),
        ICON_FOLDER => (poly(&[(3.0, 6.0), (9.0, 6.0), (11.5, 8.5), (21.0, 8.5), (21.0, 19.0), (3.0, 19.0)]), vec![]),
        ICON_ROBOT => ([poly(&[(5.0, 8.0), (19.0, 8.0), (19.0, 19.0), (5.0, 19.0)]), vec![[12.0, 8.0, 12.0, 4.0], [2.5, 12.0, 2.5, 15.0], [21.5, 12.0, 21.5, 15.0], [9.0, 16.0, 15.0, 16.0]]].concat(), vec![[9.0, 12.0, 1.0, 1.0], [15.0, 12.0, 1.0, 1.0]]),
        _ => return None,
    })
}

/// Máscara alfa `n`x`n` del ícono de cromo `id`: trazo de 2 sobre 24 con extremos redondos.
fn chrome_icon(id: u16, n: usize) -> Option<Vec<u8>> {
    if (ICON_SPIN..ICON_SPIN + SPIN_FRAMES).contains(&id) {
        use std::f32::consts::TAU;
        let head = (id - ICON_SPIN) as f32 / SPIN_FRAMES as f32 * TAU;
        return Some((0..n * n).map(|p| {
            // Cometa: cinco puntos en órbita (caja de 10), cada vez más chicos y tenues.
            let (x, y) = (((p % n) as f32 + 0.5) * 10.0 / n as f32, ((p / n) as f32 + 0.5) * 10.0 / n as f32);
            let a = (0..5).map(|k| {
                let k = k as f32;
                let (s, c) = (head - k * 0.55).sin_cos();
                let d = (x - 5.0 - 3.7 * c).hypot(y - 5.0 - 3.7 * s);
                ((1.4 - 0.2 * k - d) * n as f32 / 10.0 + 0.5).clamp(0.0, 1.0) * (1.0 - 0.18 * k)
            }).fold(0.0, f32::max);
            (a * 255.0).round() as u8
        }).collect());
    }
    if id == ICON_DOT {
        let r = n as f32 / 2.0;
        return Some((0..n * n).map(|p| { let d = ((p % n) as f32 + 0.5 - r).hypot((p / n) as f32 + 0.5 - r); ((r - d + 0.5).clamp(0.0, 1.0) * 255.0) as u8 }).collect());
    }
    let (segs, rings) = icon_shapes(id)?;
    let (k, hw) = (24.0 / n as f32, 1.0);
    let mut out = Vec::with_capacity(n * n);
    for j in 0..n { for i in 0..n {
        let (px, py) = ((i as f32 + 0.5) * k, (j as f32 + 0.5) * k);
        let seg = segs.iter().map(|&[x0, y0, x1, y1]| {
            let (dx, dy) = (x1 - x0, y1 - y0);
            let t = (((px - x0) * dx + (py - y0) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            ((px - x0 - t * dx).powi(2) + (py - y0 - t * dy).powi(2)).sqrt()
        });
        // Distancia aproximada a una elipse: (|r_norm - 1|) escalado por el radio menor.
        let ring = rings.iter().map(|&[cx, cy, rx, ry]| (((px - cx) / rx).hypot((py - cy) / ry) - 1.0).abs() * rx.min(ry));
        let d = seg.chain(ring).fold(f32::MAX, f32::min);
        out.push((((hw - d) / k + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8);
    } }
    Some(out)
}

/// Máscara del ícono `id` a `w`x`h` px: de cromo (id >= 10, cuadrada) o logo de marca remuestreado (bilineal).
pub fn raster_icon(id: u16, w: u16, h: u16) -> Option<Vec<u8>> {
    if id >= 10 { return chrome_icon(id, w as usize); }
    let (_, src) = LAUNCH.iter().find_map(|(a, _)| a.icon().filter(|i| i.0 == id))?;
    let at = |x: i32, y: i32| src[(y.clamp(0, 63) * 64 + x.clamp(0, 63)) as usize] as f32;
    let mut out = Vec::with_capacity(w as usize * h as usize);
    for j in 0..h { for i in 0..w {
        let (fx, fy) = ((i as f32 + 0.5) * 64.0 / w as f32 - 0.5, (j as f32 + 0.5) * 64.0 / h as f32 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
        let bot = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
        out.push((top * (1.0 - ty) + bot * ty).round() as u8);
    } }
    Some(out)
}

/// Agente según el nombre y la ruta del ejecutable en primer plano (Shell si no coincide).
/// Un CLI de node ("node …/claude") aparece como `node`: Pane::refresh usa `agent_from_args` con su argv.
pub fn agent_from(name: &str, path: &str) -> Agent {
    let s = format!("{name} {path}").to_lowercase();
    // Nombres cortos (agy) solo por coincidencia exacta del binario, para no confundirlos con otras palabras.
    let hit = |b: &str| name == b || path.ends_with(&format!("/{b}")) || (b.len() > 3 && s.contains(b));
    LAUNCH.iter().find(|(_, b)| hit(b)).map_or(Agent::Shell, |(a, _)| *a)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status { Asking, Working, Waiting, Done }

/// Título con spinner al inicio = el agente está trabajando. Claude Code alterna ◐/◑ mientras piensa
/// y pone ✳ en reposo (visto en su binario 2.1.331); otros CLIs usan braille.
pub fn spinner(title: &str) -> bool { title.chars().next().is_some_and(|c| c == '◐' || c == '◑' || ('\u{2800}'..='\u{28ff}').contains(&c)) }

/// Done: hubo un agente y ya no está. Working: título con spinner; para agentes que no lo ponen
/// (todos menos Claude), salida en los últimos 2 s. Waiting: el resto.
pub fn status(agent: Agent, had_agent: bool, since_out: Duration, title: &str) -> Status {
    if agent == Agent::Shell { return if had_agent { Status::Done } else { Status::Waiting }; }
    let busy = spinner(title) || (agent != Agent::Claude && since_out < Duration::from_secs(2));
    if busy { Status::Working } else { Status::Waiting }
}

/// El pie del agente ofrece interrumpir: está trabajando, aunque algo haya pisado el spinner del título.
/// Solo las últimas líneas, para no confundirlo con esa frase dentro de la conversación.
pub fn interruptible(rows: &[String]) -> bool {
    rows.iter().rev().filter(|r| !r.trim().is_empty()).take(5).any(|r| r.contains("esc to interrupt"))
}

/// El agente espera que respondas: un menú numerado con el cursor en una opción, como el permiso de Claude o Codex.
/// Solo mira lo que está en pantalla; no guarda nada.
// shortcut: reconoce esos menús y "(y/n)"; si un agente pregunta de otra forma, añadir aquí su señal.
pub fn asking(rows: &[String]) -> bool {
    let clean: Vec<&str> = rows.iter().map(|r| r.trim_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{257f}').contains(&c))).collect();
    let option = |r: &str| r.strip_prefix("❯ ").or_else(|| r.strip_prefix("› ")).is_some_and(|o| o.starts_with(|c: char| c.is_ascii_digit()) && o.get(1..3) == Some(". "));
    let menu = clean.iter().any(|r| option(r))
        && clean.iter().any(|r| r.contains("1. Yes") || r.contains("Esc to cancel") || r.contains("Enter to select") || r.contains("Press enter to confirm"));
    menu || clean.iter().rev().find(|r| !r.is_empty()).is_some_and(|r| r.ends_with("(y/n)") || r.ends_with("[y/N]") || r.ends_with("[Y/n]"))
}

/// Siguiente panel que te necesita después de `at`, dando la vuelta: primero los que preguntan, luego los que terminaron.
/// Cada elemento es (pregunta, terminó sin que lo vieras).
pub fn next_attention(flags: &[(bool, bool)], at: usize) -> Option<usize> {
    let n = flags.len();
    let after = |f: fn(&(bool, bool)) -> bool| (1..=n).map(|d| (at + d) % n).find(|&i| f(&flags[i]));
    after(|f| f.0).or_else(|| after(|f| f.1))
}

/// Ruta del cwd con la home abreviada como `~` (vacío = `~`).
pub fn tilde(cwd: &str, home: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    if cwd.is_empty() || (!home.is_empty() && cwd == home.trim_end_matches('/')) { return "~".into(); }
    match cwd.strip_prefix(home.trim_end_matches('/')) { Some(r) if !home.is_empty() && r.starts_with('/') => format!("~{r}"), _ => cwd.to_string() }
}

impl Status {
    pub fn label(self) -> &'static str { match self { Status::Working => "Working", Status::Waiting => "Waiting", Status::Asking => "Needs you", Status::Done => "Done" } }
}

/// 27s, 3m, 2h, 1d.
pub fn fmt_elapsed(secs: u64) -> String {
    match secs { 0..=59 => format!("{secs}s"), 60..=3599 => format!("{}m", secs / 60), 3600..=86399 => format!("{}h", secs / 3600), _ => format!("{}d", secs / 86400) }
}

/// Quita glifos/spinner iniciales del título OSC y lo acorta a `n` caracteres.
pub fn clean_title(t: &str, n: usize) -> String {
    let t = t.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '[' && c != '~' && c != '/').trim();
    if t.chars().count() <= n { t.to_string() } else { format!("{}…", t.chars().take(n - 1).collect::<String>()) }
}

/// Nombre de carpeta del cwd (`~` para la home o vacío).
pub fn project_of(cwd: &str, home: &str) -> String {
    if cwd.is_empty() || cwd == home { return "~".into(); }
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("~").to_string()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Filter { All, Waiting, Working }

#[derive(Clone, Debug)]
pub struct Entry {
    pub tab: usize, pub pane: usize, pub project: String, pub title: String, pub agent: Agent,
    pub status: Status, pub done: bool, pub secs: u64, pub unread: bool, pub focused: bool, pub skills: Vec<String>, pub skills_open: bool,
}

/// Renglones de skills de cada fila: uno cerrada, uno por cada dos si está abierta.
pub fn tall(es: &[Entry]) -> Vec<usize> {
    es.iter().enumerate().flat_map(|(i, e)| {
        let n = if e.skills_open { e.skills.len().div_ceil(2) } else { e.skills.len().min(1) };
        std::iter::repeat_n(i, n)
    }).collect()
}

/// (All, Waiting, Working)
pub fn counts(es: &[Entry]) -> (usize, usize, usize) {
    let n = |s| es.iter().filter(|e| e.status == s).count();
    (es.len(), n(Status::Waiting) + n(Status::Asking), n(Status::Working))
}

/// Filtra por estado y por texto (título o proyecto, sin distinguir mayúsculas).
pub fn filter(es: Vec<Entry>, f: Filter, q: &str) -> Vec<Entry> {
    let q = q.to_lowercase();
    es.into_iter().filter(|e| {
        let ok = match f { Filter::All => true, Filter::Waiting => matches!(e.status, Status::Waiting | Status::Asking), Filter::Working => e.status == Status::Working };
        ok && (q.is_empty() || e.title.to_lowercase().contains(&q) || e.project.to_lowercase().contains(&q))
    }).collect()
}

pub enum Item { Group(String), Row(usize) }

/// Agrupa por proyecto (orden de primera aparición); `Row(i)` indexa `es`.
pub fn group(es: &[Entry]) -> Vec<Item> {
    let mut projects: Vec<&str> = vec![];
    for e in es { if !projects.contains(&e.project.as_str()) { projects.push(&e.project); } }
    projects.into_iter().flat_map(|p| {
        std::iter::once(Item::Group(p.to_string())).chain(es.iter().enumerate().filter(move |(_, e)| e.project == p).map(|(i, _)| Item::Row(i)))
    }).collect()
}

/// UUID 8-4-4-4-12 hexadecimal.
pub fn is_uuid(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    p.len() == 5 && p.iter().zip([8, 4, 4, 4, 12]).all(|(x, n)| x.len() == n && x.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Hit { New, Folder, Search, Filter(Filter), Group(usize), Row(usize), Launch(usize), Archived, ArchRow(usize), ArchDel(usize) }

/// Rects (px) de todo lo interactivo/dibujable de la barra para una ventana de `win` px y escala `sc`.
/// `Group(i)`/`Row(j)`: `i` indexa `items`, `j` indexa las entradas; solo lo que cabe en la lista.
/// `tall`: índice de entrada repetido una vez por cada renglón de skills que lleva.
pub fn layout(win: (f32, f32), sc: f32, items: &[Item], arch: usize, tall: &[usize]) -> Vec<(R, Hit)> {
    let h = win.1 / sc;
    let mut v = vec![
        ((8.0, 44.0, SIDE_W - 52.0, 28.0), Hit::New), ((SIDE_W - 36.0, 44.0, 28.0, 28.0), Hit::Folder),
        ((8.0, 80.0, SIDE_W - 16.0, 28.0), Hit::Search),
    ];
    let fw = (SIDE_W - 16.0) / 3.0;
    for (i, f) in [Filter::All, Filter::Waiting, Filter::Working].into_iter().enumerate() {
        v.push(((8.0 + i as f32 * fw, 116.0, fw, 24.0), Hit::Filter(f)));
    }
    let (mut y, bottom) = (152.0, h - 84.0 - arch as f32 * ARCH_H);
    for (i, it) in items.iter().enumerate() {
        let (hh, hit) = match it { Item::Group(_) => (GROUP_H, Hit::Group(i)), Item::Row(j) => (ROW_H + tall.iter().filter(|t| *t == j).count() as f32 * SKILLS_H, Hit::Row(*j)) };
        if y + hh > bottom { break; }
        v.push(((0.0, y, SIDE_W, hh), hit));
        y += hh;
    }
    for i in 0..LAUNCH.len() { v.push(((10.0 + i as f32 * 36.0, h - 76.0, 30.0, 28.0), Hit::Launch(i))); }
    v.push(((8.0, h - 40.0, SIDE_W - 16.0, 28.0), Hit::Archived));
    // `k` cuenta desde la fila de arriba (la archivada más reciente): clic = reabrir, × = borrar.
    for k in 0..arch {
        let y = arch_y(h, arch, k);
        v.push(((8.0, y, SIDE_W - 42.0, ARCH_H), Hit::ArchRow(k)));
        v.push(((SIDE_W - 32.0, y, 24.0, ARCH_H), Hit::ArchDel(k)));
    }
    v.into_iter().map(|((x, y, w, h), k)| ((x * sc, y * sc, w * sc, h * sc), k)).collect()
}

pub fn hit_at(l: &[(R, Hit)], m: (f32, f32)) -> Option<Hit> {
    l.iter().find(|(r, _)| m.0 >= r.0 && m.0 < r.0 + r.2 && m.1 >= r.1 && m.1 < r.1 + r.3).map(|(_, h)| *h)
}

/// ¿`bin` está en PATH (o en las carpetas habituales de CLIs)?
pub fn on_path(bin: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let extra = term_core::user_bins(std::path::Path::new(&home));
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).chain(extra)
        .any(|d| ["exe", "cmd", "bat", ""].iter().any(|x| d.join(bin).with_extension(x).is_file()))
}

/// Ruta absoluta con `/`: `C:/…`, red `//…` o estilo Unix.
pub fn is_abs(p: &str) -> bool { p.starts_with('/') || matches!(p.as_bytes(), [d, b':', b'/', ..] if d.is_ascii_alphabetic()) }

pub fn proc_info(pid: i32) -> (String, String, String) { crate::win::proc_info(pid) }

/// Sesión de agente cerrada. `token` es lo que permite reabrirla (UUID de Claude o el binario de otro agente).
#[derive(Debug, PartialEq)]
pub struct Archived { pub title: String, pub project: String, pub cwd: String, pub token: String, pub at: std::time::SystemTime }

/// ¿Sirve para reabrir una sesión? Es lo único que llega a un comando.
pub fn valid_token(t: &str) -> bool { is_uuid(t) || LAUNCH.iter().any(|(a, b)| *a != Agent::Claude && *b == t) }

pub fn archived_to_json(a: &[Archived]) -> String {
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    serde_json::Value::Array(a.iter().map(|x| serde_json::json!({"title": x.title, "project": x.project, "cwd": x.cwd, "token": x.token, "at": secs(x.at)})).collect()).to_string()
}

/// Descarta entradas incompletas, con ruta no absoluta o token inválido.
pub fn archived_from_json(t: &str) -> Vec<Archived> {
    let Ok(serde_json::Value::Array(v)) = serde_json::from_str(t) else { return vec![] };
    v.iter().filter_map(|x| {
        let s = |k: &str| x.get(k)?.as_str().map(String::from);
        let at = std::time::UNIX_EPOCH.checked_add(Duration::from_secs(x.get("at")?.as_u64()?))?;
        let a = Archived { title: s("title")?, project: s("project")?, cwd: s("cwd")?, token: s("token")?, at };
        (is_abs(&a.cwd) && valid_token(&a.token)).then_some(a)
    }).collect()
}

/// Panel "Agents": filas = agentes abiertos (no Shell), los que piden respuesta, luego working y luego los más recientes.
pub fn agent_rows(es: Vec<Entry>) -> Vec<Entry> {
    let mut v: Vec<Entry> = es.into_iter().filter(|e| e.agent != Agent::Shell).collect();
    v.sort_by_key(|e| (match e.status { Status::Asking => 0, Status::Working => 1, _ => 2 }, e.secs));
    v
}
/// Selección tras mover `d` filas, con tope en 0..n-1. shortcut: se indexa por posición; si la lista se reordena, la selección sigue la posición.
pub fn move_sel(sel: usize, d: i32, n: usize) -> usize { (sel as i64 + d as i64).clamp(0, n.saturating_sub(1) as i64) as usize }
/// Segundos que dura el aviso "press s again to stop".
pub const STOP_SECS: u64 = 3;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum AHit { Close, Row(usize) }
/// Rects (px) del panel Agents: X de cierre y hasta `n` filas (las que caben sobre el pie).
/// Ancho (px lógicos) de la columna derecha de Agents/Processes.
pub const RIGHT_W: f32 = 330.0;
/// Rects (px) de los paneles de la columna derecha bajo la barra de pestañas (`top` px):
/// Agents arriba y Processes abajo si están los dos; uno solo ocupa toda la columna.
pub fn column(win: (f32, f32), sc: f32, top: f32, open: [bool; 3]) -> [Option<R>; 3] {
    let (x, w, h) = (win.0 - RIGHT_W * sc, RIGHT_W * sc, (win.1 - top).max(1.0));
    // Alto repartido entre los abiertos: Agents (filas simples) pide algo menos que Processes y Ports.
    let weight = |k: usize| if !open[k] { 0.0 } else if k == 0 { 0.45 } else { 0.55 };
    let total: f32 = (0..3).map(weight).sum();
    let mut acc = 0.0;
    std::array::from_fn(|k| open[k].then(|| {
        let y = top + (h * acc / total).round();
        acc += weight(k);
        (x, y, w, top + (h * acc / total).round() - y)
    }))
}
/// Panel de la columna en `r` (px): cerrar arriba a la derecha y filas desde `rows_top` (px lógicos), dejando `foot` abajo.
fn panel_layout(r: R, sc: f32, n: usize, rows_top: f32, foot: f32) -> Vec<(R, AHit)> {
    let mut v = vec![((r.0 + r.2 - 34.0 * sc, r.1 + 6.0 * sc, 28.0 * sc, 28.0 * sc), AHit::Close)];
    for i in 0..n {
        let y = r.1 + (rows_top + i as f32 * ROW_H) * sc;
        if y + ROW_H * sc > r.1 + r.3 - foot * sc { break; }
        v.push(((r.0, y, r.2, ROW_H * sc), AHit::Row(i)));
    }
    v
}
/// Panel Agents: encabezado de 40 y pie de 32 (px lógicos).
pub fn agents_layout(r: R, sc: f32, n: usize) -> Vec<(R, AHit)> { panel_layout(r, sc, n, 40.0, 32.0) }
/// Panel Processes: encabezado + fila de búsqueda/columnas (filas desde 68) y pie de 2 líneas.
pub fn procs_layout(r: R, sc: f32, n: usize) -> Vec<(R, AHit)> { panel_layout(r, sc, n, 68.0, 48.0) }
pub fn ahit_at(l: &[(R, AHit)], m: (f32, f32)) -> Option<AHit> {
    l.iter().find(|(r, _)| m.0 >= r.0 && m.0 < r.0 + r.2 && m.1 >= r.1 && m.1 < r.1 + r.3).map(|(_, h)| *h)
}

#[derive(Default)]
pub struct List { pub open: bool, pub sel: usize, pub query: String, pub stop: Option<(i32, String, i32, u32)>, pub nope: bool, pub changed: bool }

impl Side {
    pub fn open_panels(&self) -> [bool; 3] { [self.agents_open, self.lists[0].open, self.lists[1].open] }
}

pub struct Side {
    /// La columna derecha (Agents/Processes) recibe el teclado: al abrirla o al hacer clic en ella.
    pub col_focus: bool,
    pub agents_open: bool, pub agents_sel: usize, pub stop_at: Option<std::time::Instant>,
    pub visible: bool, pub focus: bool, pub searching: bool, pub query: String, pub filter: Filter,
    /// Processes (0) y Ports (1): cada uno con su búsqueda, selección y confirmación.
    pub lists: [List; 2],
    /// Panel de la columna que recibe el teclado: 0 Agents, 1 Processes, 2 Ports.
    pub kf: usize,
    pub psort: crate::procs::Sort, pub mon: crate::procs::Mon,
    pub archived: Vec<Archived>, pub open_archived: bool, pub installed: [bool; 6],
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(project: &str, title: &str, status: Status) -> Entry {
        Entry { tab: 0, pane: 0, project: project.into(), title: title.into(), agent: Agent::Claude, status, done: false, secs: 0, unread: false, focused: false, skills: vec![], skills_open: false }
    }
    #[test]
    fn agente_desde_argv_de_node() {
        let a = |v: &[&str]| agent_from_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["node", "/x/bin/claude", "--resume"]), Agent::Claude);
        assert_eq!(a(&["node", "/opt/lib/node_modules/@openai/codex/bin/codex.js"]), Agent::Codex);
        assert_eq!(a(&["node", "server.js"]), Agent::Shell);
        assert_eq!(a(&[]), Agent::Shell);
    }
    #[test]
    fn procargs_parsea() {
        let mut b = 3i32.to_ne_bytes().to_vec();
        b.extend(b"/usr/bin/node\0\0\0node\0/x/claude\0-c\0HOME=/x\0");
        assert_eq!(parse_procargs(&b), ["node", "/x/claude", "-c"]);
    }
    #[test]
    fn archived_recientes_primero_y_recorta() {
        use std::time::{Duration as D, SystemTime as T};
        let now = T::UNIX_EPOCH + D::from_secs(1000);
        let a: Vec<Archived> = (0..8).map(|i| Archived { title: "x".repeat(40), project: format!("p{i}"), cwd: "/x".into(), token: "codex".into(), at: now - D::from_secs(60 * (8 - i)) }).collect();
        let r = archived_rows(&a, now);
        assert_eq!(r.len(), ARCH_MAX);
        assert_eq!(r[0].1, "1m");
        assert_eq!(r[0].0.chars().count(), 22);
        assert!(arch_y(1000.0, 6, 0) < arch_y(1000.0, 6, 5));
    }
    #[test]
    fn agrupa_por_proyecto() {
        let es = [e("a", "1", Status::Done), e("b", "2", Status::Done), e("a", "3", Status::Done)];
        let g = group(&es);
        let s: Vec<String> = g.iter().map(|i| match i { Item::Group(p) => p.clone(), Item::Row(j) => es[*j].title.clone() }).collect();
        assert_eq!(s, ["a", "1", "3", "b", "2"]);
    }
    #[test]
    fn estado() {
        let (s, l) = (Duration::from_secs(1), Duration::from_secs(30));
        assert_eq!(status(Agent::Claude, true, s, "x"), Status::Waiting);
        assert_eq!(status(Agent::Claude, true, l, "⠋ x"), Status::Working);
        assert_eq!(status(Agent::Claude, true, l, "✳ x"), Status::Waiting);
        assert_eq!(status(Agent::Shell, true, s, "x"), Status::Done);
        assert_eq!(status(Agent::Shell, false, s, "x"), Status::Waiting);
    }
    #[test]
    fn filtra_y_cuenta() {
        let es = vec![e("midterm", "Build", Status::Working), e("midlive", "Docs", Status::Waiting), e("x", "Fix", Status::Done)];
        assert_eq!(counts(&es), (3, 1, 1));
        assert_eq!(filter(es.clone(), Filter::All, "").len(), 3);
        assert_eq!(filter(es.clone(), Filter::Working, "")[0].title, "Build");
        assert_eq!(filter(es.clone(), Filter::Waiting, "")[0].title, "Docs");
        assert_eq!(filter(es.clone(), Filter::All, "MID").len(), 2);
        assert_eq!(filter(es, Filter::Working, "mid")[0].project, "midterm");
    }
    #[test]
    fn formatos() {
        assert_eq!([fmt_elapsed(27), fmt_elapsed(180), fmt_elapsed(7300), fmt_elapsed(200000)], ["27s", "3m", "2h", "2d"]);
        assert_eq!(project_of("/Users/a/Dev/midterm/", "/Users/a"), "midterm");
        assert_eq!(project_of("/Users/a", "/Users/a"), "~");
        assert_eq!(clean_title("✳ Claude Code", 20), "Claude Code");
        assert_eq!(clean_title("abcdef", 4), "abc…");
        assert_eq!(agent_from("2.1.5", "/x/.local/share/claude/versions/2.1.5"), Agent::Claude);
        assert_eq!(agent_from("zsh", "/bin/zsh"), Agent::Shell);
    }
    #[test]
    fn logos_embebidos() {
        for (a, _) in LAUNCH { let (id, d) = a.icon().unwrap(); assert_eq!(d.len(), 4096); assert!(d.iter().any(|&b| b > 200)); assert_eq!(raster_icon(id, 36, 36).unwrap().len(), 1296); }
        assert!(Agent::Shell.icon().is_none());
    }
    #[test]
    fn iconos_de_cromo() {
        for id in ICON_NEW..=ICON_ROBOT { let m = raster_icon(id, 32, 32).unwrap(); assert_eq!(m.len(), 1024); assert!(m.iter().any(|&b| b > 200) && m.iter().any(|&b| b == 0)); }
        assert!(raster_icon(99, 32, 32).is_none());
    }
    fn uuid_ok() -> String { "0a1b2c3d-1111-4222-8333-444455556666".into() }
    #[test]
    fn uuid() {
        assert!(is_uuid(&uuid_ok()) && is_uuid("0A1B2C3D-1111-4222-8333-444455556666"));
        for m in ["", "x", "0a1b2c3d-1111-4222-8333-44445555666", "0a1b2c3d-1111-4222-8333-44445555666g", "0a1b2c3d-1111-4222-8333-444455556666;rm", "0a1b2c3d-1111-4222-8333-444455556666\n"] { assert!(!is_uuid(m), "{m:?}"); }
    }
    #[test]
    fn estado_por_titulo_de_claude() {
        let (q, r) = (Duration::from_secs(0), Duration::from_secs(10));
        assert_eq!(status(Agent::Claude, true, q, "✳ Refactor"), Status::Waiting);
        assert_eq!(status(Agent::Claude, true, r, "◐ Refactor"), Status::Working);
        assert_eq!(status(Agent::Claude, true, r, "◑ Refactor"), Status::Working);
        assert_eq!(status(Agent::Codex, true, q, "codex"), Status::Working);
        assert_eq!(status(Agent::Codex, true, r, "codex"), Status::Waiting);
    }
    #[test]
    fn nombre_de_agente() {
        assert_eq!((Agent::Claude.name(), Agent::Codex.name(), Agent::Antigravity.name(), Agent::Shell.name()), (Some("claude"), Some("codex"), Some("antigravity"), None));
    }
    #[test]
    fn ruta_con_tilde() {
        assert_eq!(tilde("/Users/a", "/Users/a"), "~");
        assert_eq!(tilde("/Users/a/Dev/x/", "/Users/a"), "~/Dev/x");
        assert_eq!(tilde("/Users/ab/x", "/Users/a"), "/Users/ab/x");
        assert_eq!(tilde("/tmp", "/Users/a"), "/tmp");
        assert_eq!(tilde("", "/Users/a"), "~");
    }
    #[test]
    fn panel_agents() {
        let mut shell = e("z", "zsh", Status::Waiting);
        shell.agent = Agent::Shell;
        let (mut a, mut b, mut c) = (e("a", "1", Status::Waiting), e("b", "2", Status::Working), e("c", "3", Status::Working));
        (a.secs, b.secs, c.secs) = (5, 90, 30);
        let r = agent_rows(vec![shell, a, b, c]);
        assert_eq!(r.iter().map(|x| x.project.as_str()).collect::<Vec<_>>(), ["c", "b", "a"]);
        assert_eq!(counts(&r).2, 2);
        assert_eq!([move_sel(0, -1, 3), move_sel(1, 1, 3), move_sel(2, 1, 3), move_sel(0, 1, 0)], [0, 2, 2, 0]);
        let [ra, rp, _] = column((2000.0, 1200.0), 2.0, 72.0, [true, false, false]);
        let ra = ra.unwrap();
        assert!(rp.is_none() && ra.0 == 2000.0 - RIGHT_W * 2.0 && ra.1 == 72.0 && ra.3 == 1128.0);
        let l = agents_layout(ra, 2.0, 2);
        assert_eq!(ahit_at(&l, (1960.0, 90.0)), Some(AHit::Close));
        assert_eq!(ahit_at(&l, (ra.0 + 20.0, 72.0 + (40.0 + ROW_H) * 2.0 + 4.0)), Some(AHit::Row(1)));
        assert_eq!(ahit_at(&l, (ra.0 + 20.0, 72.0 + (40.0 + 2.0 * ROW_H) * 2.0 + 4.0)), None);
        assert_eq!(ahit_at(&l, (20.0, 72.0 + 44.0 * 2.0)), None);
        let three = column((2000.0, 1200.0), 2.0, 72.0, [true; 3]).map(Option::unwrap);
        assert!(three[0].1 + three[0].3 == three[1].1 && three[1].1 + three[1].3 == three[2].1 && three[2].1 + three[2].3 == 1200.0);
        let [a2, p2, _] = column((2000.0, 1200.0), 2.0, 72.0, [true, true, false]);
        let (a2, p2) = (a2.unwrap(), p2.unwrap());
        assert!(a2.1 + a2.3 == p2.1 && p2.1 + p2.3 == 1200.0);
    }
    #[test]
    fn trabajando_por_el_pie() {
        let r = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(interruptible(&r(&["✻ Pensando…", "❯ ", "───", "  ⏵⏵ auto mode on · esc to interrupt", ""])));
        assert!(!interruptible(&r(&["dice esc to interrupt", "a", "b", "c", "❯ ", "  ⏵⏵ auto mode on"])));
    }
    #[test]
    fn semaforo() {
        let r = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(asking(&r(&["│ Do you want to proceed?  │", "│ ❯ 1. Yes                 │", "│   2. No                  │"])));
        assert!(asking(&r(&["  1. Rojo", "❯ 2. Verde", "", "Enter to select · Esc to cancel"])));
        assert!(asking(&r(&["› 1. Yes, proceed", "  2. No"])));
        assert!(asking(&r(&["Overwrite file? (y/n)", ""])));
        // Una lista escrita en el cuadro de entrada no es una pregunta.
        assert!(!asking(&r(&["❯ 1. arregla esto", "  2. y esto", "⏵⏵ auto mode on"])));
        assert!(!asking(&r(&["❯ ", "hola"])));
        let f = [(false, true), (false, false), (true, false), (false, false)];
        assert_eq!(next_attention(&f, 0), Some(2));
        assert_eq!(next_attention(&f[..2], 1), Some(0));
        assert_eq!(next_attention(&[(false, false)], 0), None);
    }
    #[test]
    fn layout_y_clic() {
        let es = [e("a", "1", Status::Done)];
        let items = group(&es);
        let tall = layout((2000.0, 1200.0), 2.0, &items, 0, &[0]);
        let row = |l: &[(R, Hit)]| l.iter().find(|h| h.1 == Hit::Row(0)).unwrap().0 .3;
        assert_eq!(row(&tall) - row(&layout((2000.0, 1200.0), 2.0, &items, 0, &[])), SKILLS_H * 2.0);
        let l = layout((2000.0, 1200.0), 2.0, &items, 0, &[]);
        assert_eq!(hit_at(&l, (20.0, 100.0)), Some(Hit::New));
        assert_eq!(hit_at(&l, (20.0, 152.0 * 2.0 + GROUP_H * 2.0 + 10.0)), Some(Hit::Row(0)));
        assert_eq!(hit_at(&l, (600.0, 100.0)), None);
    }
}
