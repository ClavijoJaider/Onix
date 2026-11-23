//! Barra lateral de sesiones: lógica pura (agrupación, estado, filtro, tiempo), geometría y libproc.
use std::time::Duration;

pub type R = (f32, f32, f32, f32);
/// Ancho fijo de la barra, en px lógicos.
pub const SIDE_W: f32 = 250.0;
const ROW_H: f32 = 46.0;
const GROUP_H: f32 = 26.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Agent { Claude, Codex, Gemini, Grok, Opencode, Junie, Shell }

/// Íconos de lanzamiento (orden de la fila inferior) y su binario.
pub const LAUNCH: [(Agent, &str); 6] = [
    (Agent::Claude, "claude"), (Agent::Codex, "codex"), (Agent::Grok, "grok"),
    (Agent::Gemini, "gemini"), (Agent::Junie, "junie"), (Agent::Opencode, "opencode"),
];

impl Agent {
    /// Glifo de texto (solo Shell; los agentes llevan logo, ver `icon`).
    pub fn glyph(self) -> &'static str { "❯" }
    /// (id, máscara alfa 64x64) del logo de marca; Shell no tiene.
    pub fn icon(self) -> Option<(u16, &'static [u8])> {
        macro_rules! m { ($i:expr, $f:literal) => { Some(($i, &include_bytes!(concat!("../assets/icons/", $f, ".a8"))[..])) } }
        match self {
            Agent::Claude => m!(0, "claude"), Agent::Codex => m!(1, "openai"), Agent::Gemini => m!(2, "googlegemini"),
            Agent::Grok => m!(3, "grok"), Agent::Opencode => m!(4, "opencode"), Agent::Junie => m!(5, "jetbrains"), Agent::Shell => None,
        }
    }
    pub fn color(self) -> [u8; 3] {
        match self {
            Agent::Claude => [0xd9, 0x77, 0x57], Agent::Gemini => [0x8e, 0x75, 0xb2], Agent::Shell => [0x8a, 0x8a, 0x94],
            _ => [0xee, 0xee, 0xee],
        }
    }
}

/// Máscara del logo `id` remuestreada (bilineal) a `w`x`h` px.
pub fn raster_icon(id: u16, w: u16, h: u16) -> Option<Vec<u8>> {
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
/// shortcut: un CLI de node ("node …/claude") aparece como `node`, no se detecta; mirar argv si hace falta.
pub fn agent_from(name: &str, path: &str) -> Agent {
    let s = format!("{name} {path}").to_lowercase();
    LAUNCH.iter().find(|(_, b)| s.contains(b)).map_or(Agent::Shell, |(a, _)| *a)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status { Working, Waiting, Done }

/// Título que empieza por un spinner braille = el agente está trabajando.
pub fn spinner(title: &str) -> bool { title.chars().next().is_some_and(|c| ('\u{2800}'..='\u{28ff}').contains(&c)) }

/// Done: hubo un agente y ya no está. Working: spinner o salida en los últimos 2 s (solo agentes). Waiting: el resto.
pub fn status(agent: Agent, had_agent: bool, since_out: Duration, title: &str) -> Status {
    if agent == Agent::Shell { return if had_agent { Status::Done } else { Status::Waiting }; }
    if spinner(title) || since_out < Duration::from_secs(2) { Status::Working } else { Status::Waiting }
}

impl Status {
    pub fn label(self) -> &'static str { match self { Status::Working => "Working", Status::Waiting => "Waiting", Status::Done => "Done" } }
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
    pub status: Status, pub secs: u64, pub unread: bool, pub focused: bool,
}

/// (All, Waiting, Working)
pub fn counts(es: &[Entry]) -> (usize, usize, usize) {
    let n = |s| es.iter().filter(|e| e.status == s).count();
    (es.len(), n(Status::Waiting), n(Status::Working))
}

/// Filtra por estado y por texto (título o proyecto, sin distinguir mayúsculas).
pub fn filter(es: Vec<Entry>, f: Filter, q: &str) -> Vec<Entry> {
    let q = q.to_lowercase();
    es.into_iter().filter(|e| {
        let ok = match f { Filter::All => true, Filter::Waiting => e.status == Status::Waiting, Filter::Working => e.status == Status::Working };
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

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Hit { New, Folder, Search, Filter(Filter), Group(usize), Row(usize), Launch(usize), Archived }

/// Rects (px) de todo lo interactivo/dibujable de la barra para una ventana de `win` px y escala `sc`.
/// `Group(i)`/`Row(j)`: `i` indexa `items`, `j` indexa las entradas; solo lo que cabe en la lista.
pub fn layout(win: (f32, f32), sc: f32, items: &[Item]) -> Vec<(R, Hit)> {
    let h = win.1 / sc;
    let mut v = vec![
        ((8.0, 44.0, SIDE_W - 52.0, 28.0), Hit::New), ((SIDE_W - 36.0, 44.0, 28.0, 28.0), Hit::Folder),
        ((8.0, 80.0, SIDE_W - 16.0, 28.0), Hit::Search),
    ];
    let fw = (SIDE_W - 16.0) / 3.0;
    for (i, f) in [Filter::All, Filter::Waiting, Filter::Working].into_iter().enumerate() {
        v.push(((8.0 + i as f32 * fw, 116.0, fw, 24.0), Hit::Filter(f)));
    }
    let (mut y, bottom) = (152.0, h - 84.0);
    for (i, it) in items.iter().enumerate() {
        let (hh, hit) = match it { Item::Group(_) => (GROUP_H, Hit::Group(i)), Item::Row(j) => (ROW_H, Hit::Row(*j)) };
        if y + hh > bottom { break; }
        v.push(((0.0, y, SIDE_W, hh), hit));
        y += hh;
    }
    for i in 0..LAUNCH.len() { v.push(((10.0 + i as f32 * 36.0, h - 76.0, 30.0, 28.0), Hit::Launch(i))); }
    v.push(((8.0, h - 40.0, SIDE_W - 16.0, 28.0), Hit::Archived));
    v.into_iter().map(|((x, y, w, h), k)| ((x * sc, y * sc, w * sc, h * sc), k)).collect()
}

pub fn hit_at(l: &[(R, Hit)], m: (f32, f32)) -> Option<Hit> {
    l.iter().find(|(r, _)| m.0 >= r.0 && m.0 < r.0 + r.2 && m.1 >= r.1 && m.1 < r.1 + r.3).map(|(_, h)| *h)
}

/// ¿`bin` está en PATH (o en carpetas habituales de CLIs)?
pub fn on_path(bin: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let extra = [format!("{home}/.local/bin"), format!("{home}/.cargo/bin"), format!("{home}/.bun/bin"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()];
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':').map(str::to_string).chain(extra).any(|d| std::path::Path::new(&d).join(bin).is_file())
}

extern "C" {
    fn proc_name(pid: i32, buf: *mut u8, size: u32) -> i32;
    fn proc_pidpath(pid: i32, buf: *mut u8, size: u32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buf: *mut u8, size: i32) -> i32;
}

fn cstr(b: &[u8]) -> String { String::from_utf8_lossy(&b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())]).into_owned() }

/// (nombre, ruta del ejecutable, cwd) de un proceso vía libproc; vacíos si falla.
pub fn proc_info(pid: i32) -> (String, String, String) {
    let (mut n, mut p) = ([0u8; 256], [0u8; 4096]);
    // PROC_PIDVNODEPATHINFO = 9: dos vnode_info_path de 152 + 1024 bytes; el cwd es la ruta del primero.
    let mut c = [0u8; 2352];
    // SAFETY: buffers locales con el tamaño que se pasa; libproc solo escribe dentro de él.
    let cwd = unsafe {
        proc_name(pid, n.as_mut_ptr(), n.len() as u32);
        proc_pidpath(pid, p.as_mut_ptr(), p.len() as u32);
        if proc_pidinfo(pid, 9, 0, c.as_mut_ptr(), c.len() as i32) >= 1176 { cstr(&c[152..1176]) } else { String::new() }
    };
    (cstr(&n), cstr(&p), cwd)
}

/// Sesión cerrada, guardada en memoria.
#[allow(dead_code)] // título, proyecto y hora se guardan; la lista Archived no se muestra aún
pub struct Archived { pub title: String, pub project: String, pub at: std::time::SystemTime }

pub struct Side {
    pub visible: bool, pub focus: bool, pub searching: bool, pub query: String, pub filter: Filter,
    pub archived: Vec<Archived>, pub installed: [bool; 6],
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(project: &str, title: &str, status: Status) -> Entry {
        Entry { tab: 0, pane: 0, project: project.into(), title: title.into(), agent: Agent::Claude, status, secs: 0, unread: false, focused: false }
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
        assert_eq!(status(Agent::Claude, true, s, "x"), Status::Working);
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
    fn libproc_propio() {
        let (n, _, cwd) = proc_info(std::process::id() as i32);
        assert!(!n.is_empty());
        assert_eq!(std::fs::canonicalize(cwd).unwrap(), std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap());
    }
    #[test]
    fn layout_y_clic() {
        let es = [e("a", "1", Status::Done)];
        let items = group(&es);
        let l = layout((2000.0, 1200.0), 2.0, &items);
        assert_eq!(hit_at(&l, (20.0, 100.0)), Some(Hit::New));
        assert_eq!(hit_at(&l, (20.0, 152.0 * 2.0 + GROUP_H * 2.0 + 10.0)), Some(Hit::Row(0)));
        assert_eq!(hit_at(&l, (600.0, 100.0)), None);
    }
}
