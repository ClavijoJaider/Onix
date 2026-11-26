//! Skills: cuáles hay, cuáles cargó la sesión y el panel de interruptores junto al cuadro de escritura.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::Mutex;

/// Estado del panel. `skills`: (nombre, cargada). Fijadas y usos se guardan en disco.
#[derive(Default, Clone, PartialEq)]
pub struct Ui {
    pub skills: Vec<(String, bool)>, pub open: bool, pub scroll: usize, pub query: String, pub note: Option<&'static str>,
    pub fav: BTreeSet<String>, pub uses: BTreeMap<String, u32>,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Line { Title(&'static str), Skill(String, bool) }

/// Desde cuántas skills se puede filtrar escribiendo.
pub const SEARCH_FROM: usize = 9;

impl Ui {
    /// Activas, fijadas, más usadas y el resto; cada una sale una sola vez.
    pub fn lines(&self) -> Vec<Line> {
        let q = self.query.to_lowercase();
        let mut used: Vec<&(String, bool)> = self.skills.iter().filter(|s| self.uses.contains_key(&s.0)).collect();
        used.sort_by_key(|s| std::cmp::Reverse(self.uses[&s.0]));
        let groups = [
            ("ACTIVAS", self.skills.iter().filter(|s| s.1).collect()),
            ("FIJADAS", self.skills.iter().filter(|s| self.fav.contains(&s.0)).collect()),
            ("MÁS USADAS", used),
            ("TODAS", self.skills.iter().collect::<Vec<_>>()),
        ];
        let (mut out, mut seen) = (vec![], BTreeSet::new());
        for (title, list) in groups {
            let new: Vec<_> = list.into_iter().filter(|s| s.0.to_lowercase().contains(&q) && seen.insert(s.0.as_str())).collect();
            if !new.is_empty() { out.push(Line::Title(title)); out.extend(new.into_iter().map(|s| Line::Skill(s.0.clone(), s.1))); }
        }
        out
    }
    pub fn save(&self) -> String { serde_json::json!({ "fav": self.fav, "uses": self.uses }).to_string() }
    pub fn restore(&mut self, text: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
        self.fav = serde_json::from_value(v["fav"].clone()).unwrap_or_default();
        self.uses = serde_json::from_value(v["uses"].clone()).unwrap_or_default();
    }
}

/// Color fijo de cada skill: el elegido para las tres propias y uno por nombre para las nuevas.
pub fn color(name: &str) -> [u8; 3] {
    const MORE: [[u8; 3]; 6] = [[0x6a, 0xa6, 0xff], [0xf0, 0x8a, 0x8a], [0x7a, 0xd0, 0xe0], [0xf0, 0xa8, 0x60], [0xe0, 0x90, 0xd0], [0xb0, 0xd0, 0x6a]];
    match name {
        "ponytail" => [0xe8, 0xc0, 0x4a], "humanizer" => [0xc9, 0xa0, 0xff], "find-skills" => [0x6a, 0xd0, 0xa8],
        n => MORE[n.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize)) % MORE.len()],
    }
}

/// Carpetas de `~/.claude/skills` con SKILL.md.
// shortcut: no mira skills de plugins ni de la carpeta del proyecto; añadirlas si se usan.
pub fn installed() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else { return vec![] };
    let dir = std::path::Path::new(&home).join(".claude/skills");
    let mut v: Vec<String> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter(|d| d.path().join("SKILL.md").is_file()).filter_map(|d| d.file_name().into_string().ok()).collect();
    v.sort();
    v
}

/// Frase con la que se le pide a Claude soltar una skill.
pub const OFF: &str = "deja de usar la skill ";

/// Aplica una línea de la transcripción a las skills cargadas.
// shortcut: tras compactar la charla se dan por descargadas, aunque Claude pueda conservarlas.
fn apply(line: &str, on: &mut BTreeSet<String>) {
    if !["Base directory for this skill", "<command-name>", "\"Skill\"", "isCompactSummary", "stop ponytail", "normal mode", OFF].iter().any(|k| line.contains(k)) { return; }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { return };
    if v["isCompactSummary"] == true { on.clear(); return; }
    let content = &v["message"]["content"];
    if v["type"] == "assistant" {
        for c in content.as_array().into_iter().flatten().filter(|c| c["name"] == "Skill") {
            if let Some(s) = c["input"]["skill"].as_str() { on.insert(s.into()); }
        }
    } else if v["type"] == "user" {
        let Some(text) = content.as_str().or_else(|| content[0]["text"].as_str()) else { return };
        if matches!(text.trim(), "stop ponytail" | "normal mode") { on.remove("ponytail"); }
        if let Some(name) = text.trim().strip_prefix(OFF) { on.remove(name); }
        if v["isMeta"] == true {
            if let Some(path) = text.strip_prefix("Base directory for this skill: ").and_then(|t| t.lines().next()) {
                on.extend(path.rsplit('/').next().map(String::from));
            }
        }
        if text.starts_with("<command-") {
            if let Some(name) = text.split("<command-name>/").nth(1).and_then(|t| t.split('<').next()) { on.insert(name.into()); }
        }
    }
}

/// Skills cargadas en la sesión `id`; solo lee lo nuevo de la transcripción.
pub fn loaded(id: &str) -> BTreeSet<String> {
    static SEEN: Mutex<Option<HashMap<String, (u64, BTreeSet<String>)>>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap();
    let (at, on) = seen.get_or_insert_with(HashMap::new).entry(id.into()).or_default();
    if let Some(file) = crate::activity::path(id).and_then(|p| std::fs::File::open(p).ok()) {
        let mut r = BufReader::new(file);
        if r.seek(SeekFrom::Start(*at)).is_ok() {
            let mut line = vec![];
            // Una línea a medio escribir se deja para la próxima.
            while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) && line.ends_with(b"\n") {
                *at += line.len() as u64;
                apply(&String::from_utf8_lossy(&line), on);
                line.clear();
            }
        }
    }
    on.clone()
}

type R = (f32, f32, f32, f32);

/// Panel sobre los puntos `spot`: su caja, el primer renglón visible y los rects de los visibles.
/// No sube de `top`; None si no caben ni tres renglones.
pub fn layout(ui: &Ui, spot: R, top: f32, sc: f32) -> Option<(R, usize, Vec<R>)> {
    let (row, head, w) = (24.0 * sc, 30.0 * sc, 290.0 * sc);
    let total = ui.lines().len();
    let fit = ((spot.1 - 8.0 * sc - top - head - 8.0 * sc) / row).floor();
    if fit < 3.0 { return None; }
    let n = total.min(fit as usize).min(12);
    let h = head + row * n.max(1) as f32 + 8.0 * sc;
    let (x, y) = ((spot.0 + spot.2 - w).max(4.0 * sc), spot.1 - 6.0 * sc - h);
    let first = ui.scroll.min(total - n);
    Some(((x, y, w, h), first, (0..n).map(|i| (x + 4.0 * sc, y + head + i as f32 * row, w - 8.0 * sc, row)).collect()))
}

/// Un renglón de la tarjeta: (nombre completo, para el color; texto que cabe).
pub type CardLine = Vec<(String, String)>;

/// Dos skills por renglón, siempre visibles las dos (recortando si hace falta), y la etiqueta final:
/// "+N" cerrada, "▴" abierta, nada si ya caben todas.
pub fn card(list: &[String], chars: usize, open: bool) -> (Vec<CardLine>, Option<String>) {
    // Se reservan 3 letras para la etiqueta y 3 por cada punto y su separación.
    let room = chars.saturating_sub(9);
    let fit = |pair: &[String]| -> CardLine {
        let len: Vec<usize> = pair.iter().map(|n| n.chars().count()).collect();
        let cap = |i: usize| {
            if len.iter().sum::<usize>() <= room { return len[i]; }
            // La segunda se queda con la mitad de abajo, así con hueco impar no se pasan.
            let half = if i == 0 { room / 2 } else { room - room / 2 };
            room - len.get(1 - i).copied().unwrap_or(0).min(half)
        };
        pair.iter().enumerate().map(|(i, n)| {
            let c = cap(i);
            let shown = if len[i] <= c { n.clone() } else { format!("{}…", n.chars().take(c.saturating_sub(1)).collect::<String>()) };
            (n.clone(), shown)
        }).collect()
    };
    let take = if open { list.len() } else { 2 };
    let lines = list.chunks(2).take(take.div_ceil(2)).map(fit).collect();
    let tail = (list.len() > 2).then(|| if open { "▴".into() } else { format!("+{}", list.len() - 2) });
    (lines, tail)
}

/// Cápsula de los puntos: pegada a la derecha de `spot` y justo del ancho de lo que muestra.
pub fn pill(ui: &Ui, spot: R, sc: f32) -> R {
    let text = |chars: usize| chars as f32 * 11.0 * 0.602 * sc;
    let on = ui.skills.iter().filter(|s| s.1).count();
    let inner = match on {
        0 => 12.0 * sc + text("skills".len()),
        n => (n.min(3) as f32 * 13.0 - 6.0) * sc + if n > 3 { 6.0 * sc + text(format!("+{}", n - 3).len()) } else { 0.0 },
    };
    let w = (inner + 12.0 * sc).min(spot.2);
    (spot.0 + spot.2 - w, spot.1, w, spot.3)
}

/// Zonas de un renglón: (estrella, resto).
pub fn star(r: R, sc: f32) -> R { (r.0 + r.2 - 78.0 * sc, r.1, 28.0 * sc, r.3) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estado_desde_la_transcripcion() {
        let mut on = BTreeSet::new();
        apply(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"humanizer"}}]}}"#, &mut on);
        apply(r#"{"type":"user","isMeta":true,"message":{"content":[{"type":"text","text":"Base directory for this skill: /u/.claude/skills/ponytail\n\nx"}]}}"#, &mut on);
        apply(r#"{"type":"user","message":{"content":"<command-message>find-skills</command-message>\n<command-name>/find-skills</command-name>"}}"#, &mut on);
        // Citar la huella en un mensaje normal no activa nada.
        apply(r#"{"type":"user","message":{"content":"Base directory for this skill: /x/falsa"}}"#, &mut on);
        assert_eq!(on.iter().collect::<Vec<_>>(), ["find-skills", "humanizer", "ponytail"]);
        apply(r#"{"type":"user","message":{"content":"stop ponytail"}}"#, &mut on);
        apply(r#"{"type":"user","message":{"content":"deja de usar la skill humanizer"}}"#, &mut on);
        assert_eq!(on.iter().collect::<Vec<_>>(), ["find-skills"]);
        apply(r#"{"type":"user","isCompactSummary":true,"message":{"content":"resumen"}}"#, &mut on);
        assert!(on.is_empty());
    }
    #[test]
    fn resumen_de_la_tarjeta() {
        let l: Vec<String> = ["find-skills", "humanizer", "ponytail"].map(String::from).to_vec();
        let text = |c: &[CardLine]| c.iter().map(|l| l.iter().map(|n| n.1.clone()).collect::<Vec<_>>()).collect::<Vec<_>>();
        let (closed, tail) = card(&l, 29, false);
        assert_eq!((text(&closed), tail.as_deref()), (vec![vec!["find-skills".to_string(), "humanizer".to_string()]], Some("+1")));
        let (open, tail) = card(&l, 29, true);
        assert!(open.len() == 2 && text(&open)[1] == ["ponytail"] && tail.as_deref() == Some("▴"));
        // Estrecha: siguen saliendo las dos, recortadas y sin pasarse.
        let narrow = card(&l, 22, false).0;
        assert!(narrow[0].len() == 2 && narrow[0].iter().map(|n| n.1.chars().count()).sum::<usize>() <= 13);
        assert_eq!(card(&l[..2], 29, false).1, None);
        assert!(card(&[], 29, false).0.is_empty());
    }
    #[test]
    fn la_capsula_se_ajusta() {
        let spot = (100.0, 0.0, 62.0, 18.0);
        let w = |n: usize| pill(&Ui { skills: (0..8).map(|i| (format!("s{i}"), i < n)).collect(), ..Default::default() }, spot, 1.0);
        assert_eq!(w(1), (143.0, 0.0, 19.0, 18.0));
        assert!(w(1).2 < w(2).2 && w(2).2 < w(3).2 && w(3).2 < w(8).2 && w(8).2 <= spot.2 && w(0).2 <= spot.2);
    }
    #[test]
    fn orden_filtro_y_caja() {
        let mut ui = Ui { skills: (0..20).map(|i| (format!("s{i:02}"), i == 7)).collect(), ..Default::default() };
        ui.fav.insert("s09".into());
        ui.uses.insert("s03".into(), 2);
        let l = ui.lines();
        assert_eq!(l[..6], [Line::Title("ACTIVAS"), Line::Skill("s07".into(), true), Line::Title("FIJADAS"), Line::Skill("s09".into(), false), Line::Title("MÁS USADAS"), Line::Skill("s03".into(), false)]);
        assert_eq!(l.len(), 24);
        ui.scroll = 999;
        let (b, first, rows) = layout(&ui, (800.0, 700.0, 60.0, 20.0), 40.0, 1.0).unwrap();
        assert!(rows.len() == 12 && first == 12 && b.1 >= 40.0 && b.1 + b.3 <= 700.0);
        ui.query = "S07".into();
        assert_eq!(ui.lines().len(), 2);
        assert!(layout(&ui, (800.0, 100.0, 60.0, 20.0), 40.0, 1.0).is_none());
        let mut again = Ui::default();
        again.restore(&ui.save());
        assert!(again.fav == ui.fav && again.uses == ui.uses);
    }
}
