//! Paleta de comandos (Ctrl+Shift+K): lista, filtro y geometría.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Attention, Tab(usize), NewTab, OpenFolder, Split, ClosePane, Search, Sidebar, Agents, Processes, Ports, Usage,
    FontUp, FontDown, FontReset, Launch(usize), Restore(usize),
}

/// Acciones fijas: (nombre, atajo, orden).
pub const ACTIONS: [(&str, &str, Cmd); 13] = [
    ("Nueva pestaña", "^⇧T", Cmd::NewTab), ("Abrir carpeta", "^⇧O", Cmd::OpenFolder), ("Dividir panel", "^⇧D", Cmd::Split),
    ("Cerrar panel", "^⇧W", Cmd::ClosePane), ("Buscar en el panel", "^⇧F", Cmd::Search), ("Barra lateral", "^⇧B", Cmd::Sidebar),
    ("Panel de agentes", "^⇧G", Cmd::Agents), ("Panel de procesos", "^⇧P", Cmd::Processes), ("Panel de puertos", "^⇧L", Cmd::Ports),
    ("Consumo de agentes (usage)", "^⇧U", Cmd::Usage), ("Letra más grande", "^+", Cmd::FontUp), ("Letra más pequeña", "^-", Cmd::FontDown),
    ("Letra normal", "^0", Cmd::FontReset),
];

/// Filas visibles a la vez.
pub const ROWS: usize = 10;

pub struct Item { pub label: String, pub hint: &'static str, pub cmd: Cmd }

#[derive(Default)]
pub struct Palette { pub query: String, pub sel: usize, pub items: Vec<Item> }

/// Minúsculas y sin tildes, para escribir "pestana" y hallar "pestaña".
fn fold(s: &str) -> String {
    s.to_lowercase().chars().map(|c| match c { 'á' => 'a', 'é' => 'e', 'í' => 'i', 'ó' => 'o', 'ú' | 'ü' => 'u', 'ñ' => 'n', c => c }).collect()
}

impl Palette {
    /// Los que contienen todas las palabras escritas, en cualquier orden.
    pub fn found(&self) -> Vec<&Item> {
        let q = fold(&self.query);
        self.items.iter().filter(|i| { let l = fold(&i.label); q.split_whitespace().all(|w| l.contains(w)) }).collect()
    }
    /// Primera fila visible: la lista se corre para que la selección no se salga.
    pub fn first(&self) -> usize { self.sel.saturating_sub(ROWS - 1) }
    pub fn picked(&self) -> Option<Cmd> { self.found().get(self.sel).map(|i| i.cmd) }
}

/// Caja de la paleta (x, y, ancho, alto de fila) para una ventana de `width` px; la fila 0 es lo escrito.
pub fn geom(width: f32, sc: f32, line: f32) -> (f32, f32, f32, f32) {
    let w = (560.0 * sc).min(width - 32.0 * sc);
    ((width - w) / 2.0, 80.0 * sc, w, line + 12.0 * sc)
}

/// Texto de una fila a `cols` caracteres: nombre a la izquierda y atajo a la derecha.
pub fn row_text(label: &str, hint: &str, cols: usize) -> String {
    let room = cols.saturating_sub(hint.chars().count() + 2);
    let label = if label.chars().count() > room { format!("{}…", label.chars().take(room.saturating_sub(1)).collect::<String>()) } else { label.to_string() };
    format!("{label:<room$}  {hint}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filtra_y_elige() {
        let items = ACTIONS.iter().map(|&(l, hint, cmd)| Item { label: l.into(), hint, cmd }).collect();
        let mut p = Palette { items, ..Default::default() };
        p.query = "PESTANA nueva".into();
        assert_eq!(p.picked(), Some(Cmd::NewTab));
        p.query = "zzz".into();
        assert_eq!(p.picked(), None);
        p.query.clear();
        p.sel = 12;
        assert_eq!((p.first(), p.picked()), (3, Some(Cmd::FontReset)));
    }
    #[test]
    fn fila_cabe() {
        assert_eq!(row_text("abc", "⌘T", 10), "abc     ⌘T");
        assert_eq!(row_text("abcdefghij", "⌘T", 10).chars().count(), 10);
    }
}
