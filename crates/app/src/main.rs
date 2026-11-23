use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use eframe::egui::{self, Color32, FontFamily, FontId, Key, Pos2, Rect, Vec2};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

const BG: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
const FG: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd4);
const FONT_SIZE: f32 = 13.0;
const SCROLLBACK: usize = 5_000;
const PALETTE: [[u8; 3]; 16] = [
    [0x1c, 0x1c, 0x1e], [0xe0, 0x5a, 0x5a], [0x5a, 0xc0, 0x6a], [0xd9, 0xb0, 0x5a],
    [0x5a, 0x8a, 0xe0], [0xb0, 0x70, 0xd0], [0x4a, 0xc0, 0xc0], [0xd4, 0xd4, 0xd4],
    [0x5a, 0x5a, 0x60], [0xff, 0x7a, 0x7a], [0x7a, 0xe0, 0x8a], [0xf0, 0xd0, 0x7a],
    [0x7a, 0xaa, 0xff], [0xd0, 0x90, 0xf0], [0x6a, 0xe0, 0xe0], [0xff, 0xff, 0xff],
];

/// Color de alacritty -> Color32 (256 colores, truecolor y nombrados).
fn to_color(c: Color, is_fg: bool) -> Color32 {
    let pal = |i: usize| Color32::from_rgb(PALETTE[i][0], PALETTE[i][1], PALETTE[i][2]);
    match c {
        Color::Spec(r) => Color32::from_rgb(r.r, r.g, r.b),
        Color::Indexed(i) => match i {
            0..=15 => pal(i as usize),
            16..=231 => {
                let n = i - 16;
                let lv = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Color32::from_rgb(lv(n / 36), lv(n / 6 % 6), lv(n % 6))
            }
            _ => {
                let g = 8 + (i - 232) * 10;
                Color32::from_rgb(g, g, g)
            }
        },
        Color::Named(n) => match n as usize {
            i @ 0..=15 => pal(i),
            i @ 259..=266 => pal(i - 259).gamma_multiply(0.7),
            _ if n == NamedColor::Background => BG,
            _ if is_fg => FG,
            _ => BG,
        },
    }
}

struct Listener(Arc<Mutex<Box<dyn Write + Send>>>);
impl EventListener for Listener {
    // Respuestas del terminal al programa (p. ej. posición del cursor que pide vim).
    fn send_event(&self, e: TermEvent) {
        if let TermEvent::PtyWrite(s) = e {
            let _ = self.0.lock().unwrap().write_all(s.as_bytes());
        }
    }
}

struct Size(usize, usize);
impl Dimensions for Size {
    fn total_lines(&self) -> usize { self.1 }
    fn screen_lines(&self) -> usize { self.1 }
    fn columns(&self) -> usize { self.0 }
}

struct App {
    term: Arc<Mutex<Term<Listener>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    size: (usize, usize),
    scroll_acc: f32,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("zsh");
        cmd.arg("-l");
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        if let Some(h) = std::env::var_os("HOME") {
            cmd.cwd(h);
        }
        let mut child = pair.slave.spawn_command(cmd).expect("spawn zsh");
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer().expect("writer")));
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let config = Config { scrolling_history: SCROLLBACK, ..Config::default() };
        let term = Arc::new(Mutex::new(Term::new(config, &Size(80, 24), Listener(writer.clone()))));

        let (t, ctx2) = (term.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 { break; }
                parser.advance(&mut *t.lock().unwrap(), &buf[..n]);
                ctx2.request_repaint();
            }
            ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
        });
        std::thread::spawn(move || { let _ = child.wait(); });
        Self { term, writer, master: pair.master, size: (80, 24), scroll_acc: 0.0 }
    }

    fn send(&self, b: &[u8]) {
        let _ = self.writer.lock().unwrap().write_all(b);
    }

    fn handle_input(&mut self, ctx: &egui::Context, cell: Vec2, origin: Pos2, resp: &egui::Response) {
        let mode = *self.term.lock().unwrap().mode();
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            match e {
                egui::Event::Text(t) => self.send(t.as_bytes()),
                egui::Event::Paste(t) => {
                    if mode.contains(TermMode::BRACKETED_PASTE) {
                        self.send(format!("\x1b[200~{t}\x1b[201~").as_bytes());
                    } else {
                        self.send(t.as_bytes());
                    }
                }
                egui::Event::Copy => {
                    let s = self.term.lock().unwrap().selection_to_string();
                    if let Some(s) = s { ctx.copy_text(s); }
                }
                egui::Event::Key { key, pressed: true, modifiers, .. } if !modifiers.command => {
                    if let Some(b) = key_bytes(key, modifiers.ctrl, mode.contains(TermMode::APP_CURSOR)) {
                        self.send(&b);
                    }
                }
                _ => {}
            }
        }
        // Selección con el ratón.
        let to_point = |p: Pos2, off: usize| {
            let col = (((p.x - origin.x) / cell.x).max(0.0) as usize).min(self.size.0 - 1);
            let row = (((p.y - origin.y) / cell.y).max(0.0) as i32).min(self.size.1 as i32 - 1);
            Point::new(Line(row - off as i32), Column(col))
        };
        let mut term = self.term.lock().unwrap();
        let off = term.grid().display_offset();
        if let Some(p) = resp.interact_pointer_pos() {
            if resp.drag_started() {
                term.selection = Some(Selection::new(SelectionType::Simple, to_point(p, off), Side::Left));
            } else if resp.dragged() {
                if let Some(s) = term.selection.as_mut() { s.update(to_point(p, off), Side::Right); }
            }
        }
        if resp.clicked() { term.selection = None; }
    }
}

/// Tecla -> bytes que espera el PTY.
fn key_bytes(key: Key, ctrl: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let arrow = |c: u8| Some(vec![0x1b, if app_cursor { b'O' } else { b'[' }, c]);
    match key {
        Key::Enter => Some(b"\r".to_vec()),
        Key::Backspace => Some(vec![0x7f]),
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
        _ if ctrl => {
            let n = key.name();
            let c = n.as_bytes();
            (c.len() == 1 && c[0].is_ascii_alphabetic()).then(|| vec![c[0].to_ascii_lowercase() - b'a' + 1])
        }
        _ => None,
    }
}

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] { BG.to_normalized_gamma_f32() }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let font = FontId::new(FONT_SIZE, FontFamily::Monospace);
        let cell = Vec2::new(ctx.fonts_mut(|f| f.glyph_width(&font, 'M')), ctx.fonts_mut(|f| f.row_height(&font)));
        let rect = ui.max_rect().shrink(8.0);
        let (cols, rows) = (((rect.width() / cell.x) as usize).max(2), ((rect.height() / cell.y) as usize).max(2));
        if (cols, rows) != self.size {
            self.size = (cols, rows);
            self.term.lock().unwrap().resize(Size(cols, rows));
            let _ = self.master.resize(PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 });
        }

        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        resp.request_focus();
        self.handle_input(&ctx, cell, rect.min, &resp);

        // Rueda -> historial.
        self.scroll_acc += ctx.input(|i| i.smooth_scroll_delta.y) / cell.y;
        let lines = self.scroll_acc as i32;
        self.scroll_acc -= lines as f32;
        if lines != 0 { self.term.lock().unwrap().scroll_display(Scroll::Delta(lines)); }

        let painter = ui.painter_at(ui.max_rect());
        let term = self.term.lock().unwrap();
        let content = term.renderable_content();
        let off = content.display_offset;
        let cursor = (content.cursor.shape != alacritty_terminal::vte::ansi::CursorShape::Hidden && off == 0)
            .then_some(content.cursor.point);
        let sel = content.selection;

        // Por fila: fondos fusionados por tramo y texto fusionado por color.
        let mut row_cells: Vec<(usize, char, Color32, Color32, bool)> = Vec::with_capacity(cols);
        let mut cur_line = i32::MIN;
        let flush = |line: i32, cells: &mut Vec<(usize, char, Color32, Color32, bool)>| {
            let y = rect.min.y + line as f32 * cell.y;
            let x_of = |c: usize| rect.min.x + c as f32 * cell.x;
            let mut i = 0;
            while i < cells.len() {
                let bg = cells[i].3;
                let mut j = i;
                while j < cells.len() && cells[j].3 == bg { j += 1; }
                if bg != BG {
                    painter.rect_filled(Rect::from_min_max(Pos2::new(x_of(cells[i].0), y), Pos2::new(x_of(cells[j - 1].0 + 1), y + cell.y)), 0.0, bg);
                }
                i = j;
            }
            let mut i = 0;
            while i < cells.len() {
                let (fg, bold) = (cells[i].2, cells[i].4);
                let mut s = String::new();
                let mut j = i;
                while j < cells.len() && cells[j].2 == fg && cells[j].4 == bold && cells[j].0 == cells[i].0 + (j - i) {
                    s.push(cells[j].1);
                    j += 1;
                }
                if !s.trim().is_empty() {
                    painter.text(Pos2::new(x_of(cells[i].0), y), egui::Align2::LEFT_TOP, s, font.clone(), fg);
                }
                i = j;
            }
            cells.clear();
        };
        for it in content.display_iter {
            let line = it.point.line.0 + off as i32;
            if line != cur_line {
                if cur_line != i32::MIN { flush(cur_line, &mut row_cells); }
                cur_line = line;
            }
            let c = &it.cell;
            if c.flags.contains(Flags::WIDE_CHAR_SPACER) { continue; }
            let (mut fg, mut bg) = (to_color(c.fg, true), to_color(c.bg, false));
            if let (true, Color::Named(n)) = (c.flags.contains(Flags::BOLD), c.fg) {
                if (n as usize) < 8 { fg = to_color(Color::Indexed(n as u8 + 8), true); }
            }
            if c.flags.contains(Flags::DIM) { fg = fg.gamma_multiply(0.7); }
            let selected = sel.is_some_and(|s| s.contains(it.point));
            if c.flags.contains(Flags::INVERSE) ^ selected ^ (cursor == Some(it.point)) {
                std::mem::swap(&mut fg, &mut bg);
                if fg == bg { fg = BG; }
            }
            let ch = if c.flags.contains(Flags::HIDDEN) { ' ' } else { c.c };
            row_cells.push((it.point.column.0, ch, fg, bg, c.flags.contains(Flags::BOLD)));
        }
        if cur_line != i32::MIN { flush(cur_line, &mut row_cells); }
        drop(term);

        if off > 0 {
            let mut jump = false;
            egui::Area::new(egui::Id::new("jump")).fixed_pos(Pos2::new(rect.center().x - 90.0, rect.bottom() - 36.0)).show(&ctx, |ui| {
                jump = ui.button("Jump to bottom (click) ↓").clicked();
            });
            if jump { self.term.lock().unwrap().scroll_display(Scroll::Bottom); }
        }
    }
}

fn main() -> eframe::Result {
    let opts = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_title("mi-terminal").with_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native("mi-terminal", opts, Box::new(|cc| {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        Ok(Box::new(App::new(&cc.egui_ctx)))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn colores() {
        assert_eq!(to_color(Color::Indexed(196), true), Color32::from_rgb(255, 0, 0));
        assert_eq!(to_color(Color::Indexed(232), true), Color32::from_rgb(8, 8, 8));
        assert_eq!(to_color(Color::Named(NamedColor::Foreground), true), FG);
        assert_eq!(to_color(Color::Named(NamedColor::Red), true), Color32::from_rgb(0xe0, 0x5a, 0x5a));
    }
    #[test]
    fn teclas() {
        assert_eq!(key_bytes(Key::C, true, false), Some(vec![3]));
        assert_eq!(key_bytes(Key::ArrowUp, false, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(Key::Enter, false, false), Some(b"\r".to_vec()));
    }
}
