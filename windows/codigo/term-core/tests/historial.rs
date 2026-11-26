//! El historial compacto (ver vendor/alacritty_terminal) no pierde ni una casilla.
use term_core::alacritty_terminal::{event::VoidListener, grid::Dimensions, index::{Column, Line}, term::{cell::Cell, Config, Term}, vte::ansi::Processor};

struct Size;
impl Dimensions for Size {
    fn total_lines(&self) -> usize { 5 }
    fn screen_lines(&self) -> usize { 5 }
    fn columns(&self) -> usize { 40 }
}

#[test]
fn compactar_no_pierde_nada() {
    let mut term = Term::new(Config { scrolling_history: 50, ..Config::default() }, &Size, VoidListener);
    let mut parser: Processor = Processor::new();
    let feed = "\x1b[31;44mrojo\x1b[0m 漢字 😀 e\u{301} \x1b]8;id=a;http://a.b\x07enlace\x1b]8;;\x07\r\n\r\n".repeat(60);
    parser.advance(&mut term, feed.as_bytes());
    let read = |t: &Term<VoidListener>| -> Vec<Cell> {
        (-(t.grid().history_size() as i32)..5).flat_map(|l| (0..40).map(move |c| (l, c))).map(|(l, c)| t.grid()[Line(l)][Column(c)].clone()).collect()
    };
    let before = read(&term);
    term.grid_mut().compact_history();
    assert!(read(&term) == before);
    // Reciclar renglones compactos al seguir escribiendo.
    term.grid_mut().compact_history();
    parser.advance(&mut term, feed.as_bytes());
    assert!(read(&term) == before);
}
