//! Núcleo sin gráficos: PTY, grilla (alacritty_terminal), teclas y colores.
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

pub use alacritty_terminal;
use alacritty_terminal::event::{Event as TermEvent, EventListener};
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
    /// PID del hijo (zsh o el CLI lanzado).
    pub child_pid: Option<u32>,
}

impl Session {
    /// Lanza `zsh -l` (o `cmd` si se da) en un PTY. `on_output` se llama tras cada lectura;
    /// `on_exit` cuando el PTY se cierra.
    pub fn spawn(
        cmd: Option<Vec<String>>,
        on_output: impl Fn() + Send + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut c = match cmd {
            Some(v) => { let mut c = CommandBuilder::new(&v[0]); c.args(&v[1..]); c }
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
        if let Some(h) = std::env::var_os("HOME") { c.cwd(h); }
        let mut child = pair.slave.spawn_command(c).expect("spawn");
        let killer = child.clone_killer();
        let child_pid = child.process_id();
        let writer: Writer = Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let title = Arc::new(Mutex::new(String::new()));
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let term = Arc::new(Mutex::new(Term::new(config, &Size(80, 24), Listener(writer.clone(), title.clone()))));
        let t = term.clone();
        std::thread::spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 { break; }
                parser.advance(&mut *t.lock().unwrap(), &buf[..n]);
                on_output();
            }
            on_exit();
        });
        std::thread::spawn(move || { let _ = child.wait(); });
        Self { term, title, writer, master: pair.master, killer, child_pid }
    }

    /// PID del proceso en primer plano del PTY (el líder de su grupo), si se conoce.
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drop_mata_al_hijo() {
        let marca = "sleep 98765";
        let s = Session::spawn(Some(vec!["sh".into(), "-c".into(), format!("exec {marca}")]), || {}, || {});
        let vivo = || std::process::Command::new("pgrep").args(["-f", marca]).output().unwrap().status.success();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(vivo());
        drop(s);
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(!vivo());
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
    fn pegado() {
        assert_eq!(paste_bytes("a\x1b[201~b\r\nc\td", true), b"\x1b[200~a[201~b\nc\td\x1b[201~".to_vec());
        assert_eq!(paste_bytes("a\nb\x07", false), b"a\rb".to_vec());
    }
}
