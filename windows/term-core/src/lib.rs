//! Núcleo sin gráficos: PTY, grilla (alacritty_terminal), teclas y colores.
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

pub use alacritty_terminal;
use alacritty_terminal::event::{Event as TermEvent, EventListener};
use std::sync::atomic::{AtomicU64, Ordering};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

pub type Rgb = [u8; 3];
pub const BG: Rgb = [0x0b, 0x0b, 0x0d];
pub const FG: Rgb = [0xd4, 0xd4, 0xd4];
pub const SCROLLBACK: usize = 3_000;
const PALETTE: [Rgb; 16] = [
    [0x1c, 0x1c, 0x1e], [0xe0, 0x5a, 0x5a], [0x5a, 0xc0, 0x6a], [0xd9, 0xb0, 0x5a],
    [0x5a, 0x8a, 0xe0], [0xb0, 0x70, 0xd0], [0x4a, 0xc0, 0xc0], [0xd4, 0xd4, 0xd4],
    [0x5a, 0x5a, 0x60], [0xff, 0x7a, 0x7a], [0x7a, 0xe0, 0x8a], [0xf0, 0xd0, 0x7a],
    [0x7a, 0xaa, 0xff], [0xd0, 0x90, 0xf0], [0x6a, 0xe0, 0xe0], [0xff, 0xff, 0xff],
];

pub fn dim(c: Rgb) -> Rgb { c.map(|v| (v as f32 * 0.7) as u8) }

/// Color de alacritty -> RGB (256 colores, truecolor y nombrados).
pub fn to_color(c: Color, is_fg: bool) -> Rgb {
    match c {
        Color::Spec(r) => [r.r, r.g, r.b],
        Color::Indexed(i) => match i {
            0..=15 => PALETTE[i as usize],
            16..=231 => {
                let n = i - 16;
                let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                [lv(n / 36), lv(n / 6 % 6), lv(n % 6)]
            }
            _ => {
                let g = 8 + (i - 232) * 10;
                [g, g, g]
            }
        },
        Color::Named(n) => match n as usize {
            i @ 0..=15 => PALETTE[i],
            i @ 259..=266 => dim(PALETTE[i - 259]),
            _ if n == NamedColor::Background => BG,
            _ if is_fg => FG,
            _ => BG,
        },
    }
}

/// Tecla lógica, independiente de la ventana.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    Enter, Backspace, Tab, Escape, ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
    Home, End, Delete, PageUp, PageDown, Char(char),
}

/// Modificadores activos (Option = alt, se envía como Meta).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods { pub shift: bool, pub alt: bool, pub ctrl: bool }

/// Tecla + modificadores -> bytes que espera el PTY.
pub fn key_bytes(key: Key, m: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let n = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8; // parámetro xterm
    let arrow = |c: u8| {
        if m.alt && !m.shift && !m.ctrl && (c == b'C' || c == b'D') {
            // Option+←/→: salto de palabra (ESC b / ESC f)
            Some(vec![0x1b, if c == b'D' { b'b' } else { b'f' }])
        } else if n > 1 {
            Some(format!("\x1b[1;{n}{}", c as char).into_bytes())
        } else {
            Some(vec![0x1b, if app_cursor { b'O' } else { b'[' }, c])
        }
    };
    let meta = |mut v: Vec<u8>| { if m.alt { v.insert(0, 0x1b); } Some(v) };
    match key {
        Key::Enter => meta(b"\r".to_vec()),
        Key::Backspace => meta(vec![0x7f]),
        Key::Tab if m.shift => Some(b"\x1b[Z".to_vec()),
        Key::Tab => Some(b"\t".to_vec()),
        Key::Escape => Some(vec![0x1b]),
        Key::ArrowUp => arrow(b'A'),
        Key::ArrowDown => arrow(b'B'),
        Key::ArrowRight => arrow(b'C'),
        Key::ArrowLeft => arrow(b'D'),
        Key::Home => Some(b"\x1b[H".to_vec()),
        Key::End => Some(b"\x1b[F".to_vec()),
        Key::Delete => Some(b"\x1b[3~".to_vec()),
        Key::PageUp => Some(b"\x1b[5~".to_vec()),
        Key::PageDown => Some(b"\x1b[6~".to_vec()),
        Key::Char(c) if m.ctrl && c.is_ascii_alphabetic() => meta(vec![c.to_ascii_lowercase() as u8 - b'a' + 1]),
        Key::Char(c) if m.alt && !m.ctrl => meta(c.to_string().into_bytes()),
        Key::Char(_) => None,
    }
}

/// Texto pegado -> bytes: sin ESC ni controles (salvo \t y \n). Con bracketed paste se envuelve;
/// sin él, \n pasa a \r.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let t: String = text.chars().filter(|c| !c.is_control() || *c == '\t' || *c == '\n').collect();
    if bracketed { format!("\x1b[200~{t}\x1b[201~").into_bytes() } else { t.replace('\n', "\r").into_bytes() }
}

/// Ruta soltada en la ventana -> texto para el shell: escapa con `\` los ASCII que no sean [A-Za-z0-9_./~+@%:,=-]
/// (los no ASCII van sin escapar), quita controles y termina en un espacio.
pub fn drop_path_text(path: &str) -> String {
    let mut o = String::new();
    for c in path.chars().filter(|c| !c.is_control()) {
        if c.is_ascii() && !c.is_ascii_alphanumeric() && !"_./~+@%:,=-".contains(c) { o.push('\\'); }
        o.push(c);
    }
    o.push(' ');
    o
}

/// Clic en (fila,col) con el cursor en otra celda de la misma línea envuelta -> flechas ←/→ para llegar.
/// Con APP_CURSOR activo las flechas son `ESC O x`, si no `ESC [ x` (igual que `key_bytes`).
pub fn click_move_bytes(cursor: (usize, usize), click: (usize, usize), cols: usize, app_cursor: bool) -> Vec<u8> {
    let d = (click.0 * cols + click.1) as isize - (cursor.0 * cols + cursor.1) as isize;
    let arrow: &[u8] = match (d < 0, app_cursor) {
        (true, false) => b"\x1b[D", (false, false) => b"\x1b[C",
        (true, true) => b"\x1bOD", (false, true) => b"\x1bOC",
    };
    arrow.repeat(d.unsigned_abs())
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct Listener(Writer, Arc<Mutex<String>>);
impl EventListener for Listener {
    // Respuestas del terminal al programa (p. ej. posición del cursor que pide vim) y título (OSC 0/2).
    fn send_event(&self, e: TermEvent) {
        match e {
            TermEvent::PtyWrite(s) => { let _ = self.0.lock().unwrap().write_all(s.as_bytes()); }
            TermEvent::Title(t) => *self.1.lock().unwrap() = t,
            TermEvent::ResetTitle => self.1.lock().unwrap().clear(),
            _ => {}
        }
    }
}

pub struct Size(pub usize, pub usize);
impl Dimensions for Size {
    fn total_lines(&self) -> usize { self.1 }
    fn screen_lines(&self) -> usize { self.1 }
    fn columns(&self) -> usize { self.0 }
}

pub struct Session {
    pub term: Arc<Mutex<Term<Listener>>>,
    /// Título puesto por el programa (vacío si no hay).
    pub title: Arc<Mutex<String>>,
    writer: Writer,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Sube con cada lectura del PTY (con `term` tomado): si no cambió, la rejilla tampoco por parte del programa.
    pub version: Arc<AtomicU64>,
    /// PID del hijo (zsh o el CLI lanzado).
    pub child_pid: Option<u32>,
}

impl Session {
    /// Lanza `zsh -l` (o `cmd` si se da) en un PTY. `on_output` se llama tras cada lectura;
    /// `on_exit` cuando el PTY se cierra.
    pub fn spawn(
        cmd: Option<Vec<String>>,
        cwd: Option<std::path::PathBuf>,
        on_output: impl Fn() + Send + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        // Si el cwd pedido falla (sin permiso, borrado), se reintenta en HOME en vez de entrar en pánico.
        let zdot = if cmd.is_none() { chip_zdotdir() } else { None };
        let build = |dir: Option<std::ffi::OsString>| {
            let mut c = match &cmd {
                Some(v) => { let mut c = CommandBuilder::new(&v[0]); c.args(&v[1..]); c }
                None if cfg!(windows) => { let mut c = CommandBuilder::new("powershell.exe"); c.args(["-NoLogo", "-NoExit", "-Command", "$global:onix = $function:prompt; function global:prompt { [Environment]::CurrentDirectory = $PWD.ProviderPath; & $global:onix }"]); c }
                None => { let mut c = CommandBuilder::new("zsh"); c.arg("-l"); c }
            };
            // No heredar el estado de una sesión de Claude Code que lanzó la terminal
            // (CLAUDE_CODE_CHILD_SESSION, CLAUDECODE...): hacía que `claude` arrancara como sesión hija.
            let heredadas: Vec<_> = std::env::vars_os()
                .filter(|(k, _)| k.to_string_lossy().starts_with("CLAUDE"))
                .map(|(k, _)| k)
                .collect();
            for k in heredadas { c.env_remove(k); }
            c.env("TERM", "xterm-256color");
            c.env("COLORTERM", "truecolor");
            if cmd.is_none() {
                if let Some(z) = &zdot {
                    if let Some(o) = std::env::var_os("ZDOTDIR") { c.env("MI_ORIG_ZDOTDIR", o); }
                    if let Some(h) = std::env::var_os("HISTFILE") { c.env("HISTFILE_ORIG", h); }
                    c.env("ZDOTDIR", z);
                }
            }
            let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
            if let Some(h) = dir.or_else(|| std::env::var_os(home)) { c.cwd(h); }
            c
        };
        let mut child = match cwd.clone().and_then(|d| pair.slave.spawn_command(build(Some(d.into()))).ok()) {
            Some(ch) => ch,
            None => pair.slave.spawn_command(build(None)).expect("spawn"),
        };
        let killer = child.clone_killer();
        let child_pid = child.process_id();
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let title = Arc::new(Mutex::new(String::new()));
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let term = Arc::new(Mutex::new(Term::new(config, &Size(80, 24), Listener(writer.clone(), title.clone()))));
        let version = Arc::new(AtomicU64::new(0));
        let (t, v) = (term.clone(), version.clone());
        std::thread::spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 { break; }
                {
                    let mut term = t.lock().unwrap();
                    parser.advance(&mut *term, &buf[..n]);
                    v.fetch_add(1, Ordering::Relaxed);
                }
                on_output();
            }
        });
        // En Windows (ConPTY) la lectura no termina cuando el shell sale: el aviso lo da la espera del proceso.
        std::thread::spawn(move || { let _ = child.wait(); on_exit(); });
        Self { term, title, version, writer, master: pair.master, killer, child_pid }
    }

    /// PID del proceso en primer plano del PTY (el líder de su grupo), si se conoce.
    #[cfg(unix)]
    pub fn fg_pid(&self) -> Option<i32> { self.master.process_group_leader() }

    pub fn send(&self, b: &[u8]) { let _ = self.writer.lock().unwrap().write_all(b); }

    pub fn resize(&self, cols: usize, rows: usize) {
        self.term.lock().unwrap().resize(Size(cols, rows));
        let _ = self.master.resize(PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 });
    }
}

/// Al soltar la sesión (cerrar panel/pestaña) se mata el hijo: el wait() y el lector terminan solos (EOF).
impl Drop for Session {
    fn drop(&mut self) { let _ = self.killer.kill(); }
}

/// Prompt con "chip" de cwd (línea con la carpeta y cursor en la siguiente) para zsh.
/// Se hace desde el shell y no desde la terminal: solo el shell sabe dónde empieza el prompt;
/// la terminal vería celdas sueltas y no podría dibujar un fondo redondeado fiable.
const ZSHRC_CHIP: &str = "\
# Generado por mi-terminal (se sobrescribe). Desactivar: MI_TERMINAL_NO_PROMPT=1
# /etc/zshrc (entre .zshenv y .zshrc) fija HISTFILE con ZDOTDIR: se restaura antes de cargar el .zshrc del usuario.
ZDOTDIR=${MI_ORIG_ZDOTDIR:-$HOME}
HISTFILE=${HISTFILE_ORIG:-${MI_ORIG_ZDOTDIR:-$HOME}/.zsh_history}
[[ -r \"${MI_ORIG_ZDOTDIR:-$HOME}/.zshrc\" ]] && source \"${MI_ORIG_ZDOTDIR:-$HOME}/.zshrc\"
PROMPT=$'%F{244}%~%f\\n%F{4}❯%f '
ZDOTDIR=${MI_ORIG_ZDOTDIR:-$HOME}
unset MI_ORIG_ZDOTDIR
";

/// Reenvío de los demás archivos de inicio, porque con ZDOTDIR propio zsh ya no lee los del usuario.
fn zsh_forward(name: &str) -> String {
    format!("[[ -r \"${{MI_ORIG_ZDOTDIR:-$HOME}}/{name}\" ]] && source \"${{MI_ORIG_ZDOTDIR:-$HOME}}/{name}\"\n")
}

/// ¿Aplicar el prompt? No si está desactivado, si hay STARSHIP_SHELL o si el .zshrc (líneas no comentadas)
/// usa un tema conocido o fija PROMPT/PS1 o ejecuta `prompt <tema>`.
pub fn chip_enabled(no_prompt: Option<&str>, starship_shell: bool, zshrc: &str) -> bool {
    const KW: [&str; 9] = ["powerlevel10k", "p10k", "starship", "oh-my-posh", "spaceship", "pure-prompt", "pure.zsh", "promptinit", "pure/"];
    let propio = |l: &str| {
        let l = l.trim();
        !l.starts_with('#') && (KW.iter().any(|k| l.contains(k)) || (l.contains("prompt_") && l.contains("_setup"))
            || ["PROMPT=", "PS1=", "export PROMPT=", "export PS1=", "prompt "].iter().any(|p| l.starts_with(p)))
    };
    no_prompt != Some("1") && !starship_shell && !zshrc.lines().any(propio)
}

/// Escribe `contenido` solo si cambió.
fn write_if_changed(path: &std::path::Path, contenido: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|a| a == contenido) { return Ok(()); }
    std::fs::write(path, contenido)
}

/// Prepara ~/Library/Application Support/mi-terminal/zsh y devuelve el directorio si hay que usarlo como ZDOTDIR.
fn chip_zdotdir() -> Option<std::path::PathBuf> {
    if cfg!(windows) { return None; }
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    let orig = std::env::var_os("ZDOTDIR").map(std::path::PathBuf::from).unwrap_or(home.clone());
    let zshrc = std::fs::read_to_string(orig.join(".zshrc")).unwrap_or_default();
    let no = std::env::var("MI_TERMINAL_NO_PROMPT").ok();
    if !chip_enabled(no.as_deref(), std::env::var_os("STARSHIP_SHELL").is_some(), &zshrc) { return None; }
    let dir = home.join("Library/Application Support/mi-terminal/zsh");
    std::fs::create_dir_all(&dir).ok()?;
    write_if_changed(&dir.join(".zshrc"), ZSHRC_CHIP).ok()?;
    for n in [".zshenv", ".zprofile", ".zlogin"] { write_if_changed(&dir.join(n), &zsh_forward(n)).ok()?; }
    Some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chip_activo_por_defecto() { assert!(chip_enabled(None, false, "export A=1\n")); }
    #[test]
    fn chip_desactivado_por_variable() { assert!(!chip_enabled(Some("1"), false, "")); assert!(chip_enabled(Some("0"), false, "")); }
    #[test]
    fn chip_respeta_temas() {
        assert!(!chip_enabled(None, true, ""));
        for t in ["source ~/powerlevel10k/powerlevel10k.zsh-theme", "[[ -f ~/.p10k.zsh ]]", "eval \"$(starship init zsh)\"", "eval \"$(oh-my-posh init zsh)\"", "source spaceship.zsh", "fpath+=$HOME/.zsh/pure-prompt", "prompt_pure_setup", "autoload promptinit; promptinit", "prompt pure", "PS1='%n> '", "export PROMPT='> '"] {
            assert!(!chip_enabled(None, false, t), "{t}");
        }
    }
    #[test]
    fn chip_ignora_comentarios() { assert!(chip_enabled(None, false, "# eval starship\n  # PS1=x\nexport A=1\n")); }
    #[test]
    fn zshrc_generado() {
        assert!(ZSHRC_CHIP.find("HISTFILE=${HISTFILE_ORIG:-${MI_ORIG_ZDOTDIR:-$HOME}/.zsh_history}").unwrap() < ZSHRC_CHIP.find("source").unwrap());
        assert!(ZSHRC_CHIP.find("ZDOTDIR=${MI_ORIG_ZDOTDIR:-$HOME}").unwrap() < ZSHRC_CHIP.find("source").unwrap());
        assert!(ZSHRC_CHIP.contains("source \"${MI_ORIG_ZDOTDIR:-$HOME}/.zshrc\""));
        assert!(ZSHRC_CHIP.contains("PROMPT=$'%F{244}%~%f\\n%F{4}❯%f '"));
        assert!(ZSHRC_CHIP.contains("ZDOTDIR=${MI_ORIG_ZDOTDIR:-$HOME}"));
        assert!(ZSHRC_CHIP.find("source").unwrap() < ZSHRC_CHIP.find("\nPROMPT=").unwrap());
        assert_eq!(zsh_forward(".zshenv"), "[[ -r \"${MI_ORIG_ZDOTDIR:-$HOME}/.zshenv\" ]] && source \"${MI_ORIG_ZDOTDIR:-$HOME}/.zshenv\"\n");
    }
    #[test]
    fn drop_mata_al_hijo() {
        let marca = "sleep 98765";
        let s = Session::spawn(Some(vec!["sh".into(), "-c".into(), format!("exec {marca}")]), None, || {}, || {});
        let vivo = || std::process::Command::new("pgrep").args(["-f", marca]).output().unwrap().status.success();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(vivo());
        drop(s);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!vivo());
    }
    #[test]
    fn cwd_sin_permiso_no_hace_panic() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join("mi-terminal-sinperm");
        let _ = std::fs::create_dir_all(&d);
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o000)).unwrap();
        let s = Session::spawn(Some(vec!["sleep".into(), "98766".into()]), Some(d.clone()), || {}, || {});
        assert!(s.child_pid.is_some());
        drop(s);
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir(&d);
    }
    #[test]
    fn colores() {
        assert_eq!(to_color(Color::Indexed(196), true), [255, 0, 0]);
        assert_eq!(to_color(Color::Indexed(232), true), [8, 8, 8]);
        assert_eq!(to_color(Color::Named(NamedColor::Foreground), true), FG);
        assert_eq!(to_color(Color::Named(NamedColor::Red), true), [0xe0, 0x5a, 0x5a]);
    }
    #[test]
    fn teclas() {
        let n = Mods::default();
        let m = |shift, alt, ctrl| Mods { shift, alt, ctrl };
        assert_eq!(key_bytes(Key::Char('c'), m(false, false, true), false), Some(vec![3]));
        assert_eq!(key_bytes(Key::ArrowUp, n, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(Key::Enter, n, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::Backspace, n, false), Some(vec![0x7f]));
        assert_eq!(key_bytes(Key::Delete, n, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key_bytes(Key::Tab, m(true, false, false), false), Some(b"\x1b[Z".to_vec()));
        assert_eq!(key_bytes(Key::Char('b'), m(false, true, false), false), Some(b"\x1bb".to_vec()));
        assert_eq!(key_bytes(Key::Char('a'), n, false), None);
        assert_eq!(key_bytes(Key::ArrowUp, m(true, false, true), false), Some(b"\x1b[1;6A".to_vec()));
        assert_eq!(key_bytes(Key::ArrowLeft, m(false, true, false), false), Some(b"\x1bb".to_vec()));
        assert_eq!(key_bytes(Key::ArrowRight, m(false, true, false), false), Some(b"\x1bf".to_vec()));
    }
    #[test]
    fn clic_mueve_cursor() {
        assert_eq!(click_move_bytes((0, 5), (0, 3), 10, false), b"\x1b[D\x1b[D".to_vec());
        assert_eq!(click_move_bytes((0, 5), (0, 3), 10, true), b"\x1bOD\x1bOD".to_vec());
        assert_eq!(click_move_bytes((0, 5), (0, 7), 10, false), b"\x1b[C\x1b[C".to_vec());
        assert_eq!(click_move_bytes((0, 8), (1, 2), 10, false), b"\x1b[C".repeat(4));
        assert_eq!(click_move_bytes((1, 2), (0, 8), 10, false), b"\x1b[D".repeat(4));
        assert!(click_move_bytes((0, 5), (0, 5), 10, false).is_empty());
    }
    #[test]
    fn ctrl_v_llega_al_shell() {
        assert_eq!(key_bytes(Key::Char('v'), Mods { shift: false, alt: false, ctrl: true }, false), Some(vec![0x16]));
    }
    #[test]
    fn ruta_soltada() {
        assert_eq!(drop_path_text("/a/b c/d (1).png"), "/a/b\\ c/d\\ \\(1\\).png ");
        assert_eq!(drop_path_text("/tmp/it's;$x\n"), "/tmp/it\\'s\\;\\$x ");
        assert_eq!(drop_path_text("/Users/ñu/a-b_c.txt"), "/Users/ñu/a-b_c.txt ");
    }
    #[test]
    fn historial_no_depende_de_total_lines() {
        // Size(80,24) tiene total_lines == screen_lines: aun así el historial se llena (Term usa scrolling_history).
        use alacritty_terminal::event::VoidListener;
        let mut t = Term::new(Config { scrolling_history: SCROLLBACK, ..Config::default() }, &Size(80, 24), VoidListener);
        let mut p: Processor = Processor::new();
        for i in 1..=200 { p.advance(&mut t, format!("linea {i}\r\n").as_bytes()); }
        assert!(t.history_size() > 100, "history_size={}", t.history_size());
        t.resize(Size(100, 30));
        assert!(t.history_size() > 100);
        t.scroll_display(alacritty_terminal::grid::Scroll::Delta(5));
        assert_eq!(t.grid().display_offset(), 5);
    }
    #[test]
    fn pegado() {
        assert_eq!(paste_bytes("a\x1b[201~b\r\nc\td", true), b"\x1b[200~a[201~b\nc\td\x1b[201~".to_vec());
        assert_eq!(paste_bytes("a\nb\x07", false), b"a\rb".to_vec());
    }
}
