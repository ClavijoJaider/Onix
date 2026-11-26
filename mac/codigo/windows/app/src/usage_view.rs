//! Pantalla "Agent usage": cifras de Claude Code leídas de ~/.claude/projects (solo lectura).
//! Se dibuja encima del área de paneles; esc o el botón la cierran. Lo que no se mide sale con "—".
use std::sync::{Arc, Mutex};
use std::time::Instant;
use usage::stats::{self, Stats};

pub type R = (f32, f32, f32, f32);
pub type Rgb = [u8; 3];
/// (texto, x, y, color, tamaño en px lógicos, negrita, `x` es el borde derecho)
pub type Txt = (String, f32, f32, Rgb, f32, bool, bool);
/// Logo de un agente: (caja px, índice en `AGENTS`).
pub type Logo = (R, usize);
pub const BG: Rgb = [0, 0, 0];
/// Alto (px lógicos) de las barras de modelos y agentes.
pub const PILL_H: f32 = 7.0;
const PILL: Rgb = [0x14, 0x14, 0x16];
const PILL_ON: Rgb = [0x2a, 0x2a, 0x30];
const PILL_HOVER: Rgb = [0x1a, 0x1a, 0x20];
const UP: Rgb = [0x14, 0x2a, 0x1a];
const DOWN: Rgb = [0x2e, 0x16, 0x16];

/// Rects que se dibujan redondeados: (¿es uno?, fondo que tiene detrás).
pub fn rounded(c: Rgb) -> Option<Rgb> {
    match c { CARD => Some(BG), PILL_ON | PILL_HOVER => Some(PILL), UP | DOWN => Some(CARD), _ => None }
}

/// Proveedor de un modelo -> logo (índice en `AGENTS`).
fn logo_of(provider: &str) -> Option<usize> { match provider { "Anthropic" => Some(0), "OpenAI" => Some(1), "Google" => Some(2), "xAI" => Some(4), _ => None } }

pub const RANGES: [(&str, Option<i64>); 4] = [("7 days", Some(7)), ("30 days", Some(30)), ("90 days", Some(90)), ("All", None)];

const DIM: Rgb = [0x8a, 0x8a, 0x94];
const LIT: Rgb = [0xe6, 0xe6, 0xea];
const CARD: Rgb = [0x16, 0x16, 0x18];
const LINE: Rgb = [0x26, 0x26, 0x2c];
const GRAY: Rgb = [0x4a, 0x4a, 0x52];
const BLUE: Rgb = [0x4a, 0x9e, 0xff];
const GREEN: Rgb = [0x5a, 0xc0, 0x6a];
const RED: Rgb = [0xe0, 0x60, 0x60];
const ORANGE: Rgb = [0xd9, 0x77, 0x57];

/// Ancho de la columna de modelos respecto del total.
const MODELS_W: f32 = 0.485;
/// Orden de las filas de agentes (índices en `AGENTS`).
const ORDER: [usize; 6] = [0, 2, 1, 4, 5, 3];
/// Agentes de la lista (solo Claude Code y Codex tienen fuente de datos).
const AGENTS: [&str; 6] = ["Claude Code", "Codex", "Antigravity", "opencode", "Grok CLI", "Junie"];

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Hit { Range(usize), Close, Agent(usize) }

pub struct View {
    pub open: bool,
    pub range: usize,
    /// Agente elegido en la lista (None = todos).
    pub agent: Option<usize>,
    /// Registros leídos (con la hora de lectura); se relee al abrir si tienen más de 60 s.
    pub data: Arc<Mutex<Option<(Instant, Vec<usage::Record>, Vec<usage::Past>)>>>,
    pub loading: Arc<std::sync::atomic::AtomicBool>,
    cache: Option<((usize, Option<usize>), Instant, Stats)>,
}

impl View {
    pub fn new() -> Self { View { open: false, range: 1, agent: None, data: Arc::default(), loading: Arc::default(), cache: None } }

    /// Lee los jsonl en un hilo; `done` avisa al terminar (para redibujar).
    pub fn load(&self, done: impl FnOnce() + Send + 'static) {
        use std::sync::atomic::Ordering;
        let fresh = self.data.lock().unwrap().as_ref().is_some_and(|(t, ..)| t.elapsed().as_secs() < 60);
        if fresh || self.loading.swap(true, Ordering::SeqCst) { return; }
        let (data, loading) = (self.data.clone(), self.loading.clone());
        std::thread::spawn(move || {
            let mut s = usage::Scan::default();
            let mut past = vec![];
            if let Ok(h) = std::env::var("HOME") {
                let claude = std::path::Path::new(&h).join(".claude");
                s.scan_dir(&claude.join("projects"));
                s.scan_codex(&std::path::Path::new(&h).join(".codex/sessions"));
                past = usage::history(&std::fs::read_to_string(claude.join("stats-cache.json")).unwrap_or_default());
            }
            *data.lock().unwrap() = Some((Instant::now(), s.records.into_values().collect(), past));
            loading.store(false, Ordering::SeqCst);
            done();
        });
    }

    /// Estadísticas del rango elegido (se recalculan solo si cambian datos o rango).
    pub fn stats(&mut self) -> Option<&Stats> {
        let d = self.data.lock().unwrap();
        let (t, recs, past) = d.as_ref()?;
        if !self.cache.as_ref().is_some_and(|(r, ct, _)| *r == (self.range, self.agent) && ct == t) {
            let s = stats::compute(recs, past, self.agent, RANGES[self.range].1, chrono::Local::now().date_naive());
            self.cache = Some(((self.range, self.agent), *t, s));
        }
        drop(d);
        self.cache.as_ref().map(|c| &c.2)
    }
}

/// Escala de la pantalla: está medida sobre 1280x725 y crece o encoge entera con el área.
pub fn zoom(a: R, sc: f32) -> f32 { sc * (a.2 / sc / 1280.0).min(a.3 / sc / 725.0).clamp(0.55, 1.4) }

/// Zonas clicables (px) dentro del área `a`.
pub fn hits(a: R, sc: f32) -> Vec<(R, Hit)> {
    let sc = zoom(a, sc);
    let (x1, y0) = (a.0 + a.2 - 24.0 * sc, a.1 + 13.0 * sc);
    let mut v = vec![((x1 - 36.0 * sc, y0, 36.0 * sc, 27.0 * sc), Hit::Close)];
    let mut x = x1 - 60.0 * sc;
    for i in (0..RANGES.len()).rev() {
        let w = (RANGES[i].0.chars().count() as f32 * 7.6 + 22.0) * sc;
        x -= w;
        v.push(((x, y0, w, 27.0 * sc), Hit::Range(i)));
    }
    // Filas de la lista de agentes que tienen fuente de datos.
    let (x0, w) = (a.0 + 28.0 * sc, a.2 - 52.0 * sc);
    let ax = x0 + w * MODELS_W + 38.0 * sc;
    for (row, i) in ORDER.into_iter().enumerate().filter(|(_, i)| *i < 2) {
        v.push(((ax, a.1 + (451.0 + 56.0 + row as f32 * 26.0) * sc, x0 + w - ax, 26.0 * sc), Hit::Agent(i)));
    }
    v
}
pub fn hit_at(a: R, sc: f32, m: (f32, f32)) -> Option<Hit> {
    hits(a, sc).into_iter().find(|(r, _)| m.0 >= r.0 && m.0 < r.0 + r.2 && m.1 >= r.1 && m.1 < r.1 + r.3).map(|(_, h)| h)
}

fn mon(d: chrono::NaiveDate) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    use chrono::Datelike;
    format!("{} {}", M[d.month0() as usize], d.day())
}

/// "claude-opus-5-5-20260101" -> "Opus 5.5"; los demás nombres quedan igual.
fn pretty(model: &str) -> String {
    let Some(rest) = model.strip_prefix("claude-") else { return model.into() };
    let (mut name, mut ver) = (vec![], vec![]);
    for p in rest.split('-') {
        if p.chars().all(|c| c.is_ascii_digit()) { if p.len() < 8 { ver.push(p); } } else {
            let mut c = p.chars();
            name.push(c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect()));
        }
    }
    format!("{} {}", name.join(" "), ver.join(".")).trim().into()
}

/// Raya de un encabezado, desde `from` hasta antes del texto `right` que termina en `end`.
fn rule(out: &mut Vec<(R, Rgb)>, from: f32, end: f32, right: &str, y: f32, sc: f32) {
    // Ancho medio de la letra del sistema a 14 px.
    let w = end - right.chars().count() as f32 * 7.0 * sc - 12.0 * sc - from;
    if w > 0.0 { out.push(((from, y + 9.0 * sc, w, 1.0), LINE)); }
}

/// Alto de una barra de gráfica: nunca el de las píldoras, que se dibujan redondeadas.
fn solid(h: f32, sc: f32) -> f32 { if (h - PILL_H * sc).abs() < 0.5 { PILL_H * sc + 0.5 } else { h } }

/// Mini barras de una serie dentro de `r`.
fn spark(out: &mut Vec<(R, Rgb)>, r: R, vals: &[f64], c: Rgb, sc: f32) {
    let max = vals.iter().cloned().fold(0.0, f64::max);
    if vals.is_empty() { return; }
    // Al menos 30 casillas, pegadas a la derecha: con pocos días no salen bloques.
    let n = vals.len().max(30);
    let (bw, skip) = (r.2 / n as f32, n - vals.len());
    for (i, v) in vals.iter().enumerate() {
        // Los días sin uso dejan una rayita.
        let h = solid(if max > 0.0 { ((*v / max) as f32 * r.3).max(1.5 * sc) } else { 1.5 * sc }, sc);
        out.push(((r.0 + (skip + i) as f32 * bw, r.1 + r.3 - h, (bw - 2.5 * sc).max(1.0), h), c));
    }
}

/// Rects y textos de la pantalla en el área `a` (px). `st` None = cargando.
pub fn build(a: R, sc: f32, st: Option<&Stats>, range: usize, picked: Option<usize>, mouse: (f32, f32)) -> (Vec<(R, Rgb)>, Vec<Txt>, Vec<Logo>) {
    let (raw, sc) = (sc, zoom(a, sc));
    let mut rs: Vec<(R, Rgb)> = vec![(a, BG)];
    let mut logos: Vec<Logo> = vec![];
    let mut tx: Vec<Txt> = vec![];
    let (x0, y0, w) = (a.0 + 28.0 * sc, a.1 + 10.0 * sc, a.2 - 52.0 * sc);
    let t = |tx: &mut Vec<Txt>, s: String, x: f32, y: f32, c: Rgb, size: f32, b: bool| tx.push((s, x, y, c, size, b, false));
    // Igual, pero `x` es donde termina el texto.
    let tr = |tx: &mut Vec<Txt>, s: String, x: f32, y: f32, c: Rgb, size: f32| tx.push((s, x, y, c, size, false, true));
    t(&mut tx, "Agent usage".into(), x0, y0, LIT, 15.0, true);
    let hs = hits(a, raw);
    if let (Some(first), Some(last)) = (hs.get(RANGES.len()), hs.get(1)) { rs.push(((first.0 .0 - 3.0 * sc, first.0 .1 - 3.0 * sc, last.0 .0 + last.0 .2 - first.0 .0 + 6.0 * sc, first.0 .3 + 6.0 * sc), CARD)); }
    for (r, h) in hs.into_iter().filter(|h| !matches!(h.1, Hit::Agent(_))) {
        let on = matches!(h, Hit::Range(i) if i == range);
        let hov = mouse.0 >= r.0 && mouse.0 < r.0 + r.2 && mouse.1 >= r.1 && mouse.1 < r.1 + r.3;
        if h == Hit::Close { rs.push((r, if hov { PILL_ON } else { CARD })); }
        else if on || hov { rs.push((r, if on { PILL_ON } else { PILL_HOVER })); }
        let (label, c) = match h { Hit::Range(i) => (RANGES[i].0, if on { LIT } else { DIM }), _ => ("esc", DIM) };
        t(&mut tx, label.into(), r.0 + if h == Hit::Close { 7.0 } else { 11.0 } * sc, r.1 + 5.0 * sc, c, 14.0, false);
    }
    let Some(s) = st else {
        t(&mut tx, "Leyendo el historial de Claude Code…".into(), x0, y0 + 20.0 * sc, DIM, 14.0, false);
        return (rs, scaled(tx, sc / raw), logos);
    };
    t(&mut tx, format!("{} – today", mon(s.from)), x0, y0 + 19.0 * sc, DIM, 14.0, false);

    // Tarjetas.
    let (cy, ch, gap) = (a.1 + 65.0 * sc, 131.0 * sc, 15.0 * sc);
    let cw = (w - 3.0 * gap) / 4.0;
    let c = &s.cur;
    let p = s.prev.as_ref();
    let cache_pct = if c.input > 0 { c.cache_read as f64 / c.input as f64 * 100.0 } else { 0.0 };
    let per_turn = if c.turns > 0 { c.output / c.turns } else { 0 };
    let cards: [(&str, String, String, Option<i64>, Vec<f64>); 4] = [
        ("INPUT TOKENS", stats::fmt_n(c.input), format!("{cache_pct:.0}% from cache"), p.and_then(|p| stats::change(c.input, p.input)), s.days.iter().map(|d| d.0 as f64).collect()),
        ("OUTPUT TOKENS", stats::fmt_n(c.output), format!("{} per turn", stats::fmt_n(per_turn)), p.and_then(|p| stats::change(c.output, p.output)), s.days.iter().map(|d| d.1 as f64).collect()),
        ("TASKS", stats::group(c.tasks), format!("{} turns", stats::group(c.turns)), p.and_then(|p| stats::change(c.tasks, p.tasks)), s.days.iter().map(|d| d.2 as f64).collect()),
        ("AGENT TIME", stats::fmt_dur(c.secs), format!("up to {} at once", c.max_at_once), p.and_then(|p| stats::change(c.secs.max(0) as u64, p.secs.max(0) as u64)), s.days.iter().map(|d| d.3 as f64).collect()),
    ];
    for (i, (label, big, sub, delta, series)) in cards.into_iter().enumerate() {
        let x = x0 + i as f32 * (cw + gap);
        rs.push(((x, cy, cw, ch), CARD));
        t(&mut tx, label.into(), x + 14.0 * sc, cy + 13.0 * sc, DIM, 14.0, false);
        if let Some(d) = delta {
            let s = if d > 999 { "+999%".to_string() } else if d >= 0 { format!("+{d}%") } else { format!("{d}%") };
            let bw = (s.len() as f32 * 7.8 + 12.0) * sc;
            rs.push(((x + cw - 14.0 * sc - bw, cy + 12.0 * sc, bw, 21.0 * sc), if d >= 0 { UP } else { DOWN }));
            tr(&mut tx, s, x + cw - 20.0 * sc, cy + 14.0 * sc, if d >= 0 { GREEN } else { RED }, 13.0);
        }
        t(&mut tx, big, x + 14.0 * sc, cy + 31.0 * sc, LIT, 33.0, false);
        t(&mut tx, sub, x + 14.0 * sc, cy + 73.0 * sc, DIM, 14.0, false);
        spark(&mut rs, (x + 14.0 * sc, cy + 96.0 * sc, cw - 28.0 * sc, 23.0 * sc), &series, GRAY, sc);
    }

    // Tokens por día (izquierda) y mapa de horas (derecha).
    let sy = a.1 + 234.0 * sc;
    let lw = w * 0.56;
    let (rx, rw) = (x0 + lw + 39.0 * sc, w - lw - 39.0 * sc);
    let active = s.days.iter().filter(|d| d.2 > 0).count();
    let n = s.days.len().max(1);
    t(&mut tx, "TOKENS BY DAY".into(), x0, sy, DIM, 14.0, false);
    let per_day = (c.input + c.output) / n as u64;
    let head = format!("{active} of {n} days with agents · {}/day", stats::fmt_n(per_day));
    tr(&mut tx, head.clone(), x0 + lw, sy, DIM, 14.0);
    rule(&mut rs, x0 + 118.0 * sc, x0 + lw, &head, sy, sc);
    let (gx, gw, top, mid, half) = (x0 + 76.0 * sc, lw - 76.0 * sc, sy + 26.0 * sc, sy + 100.0 * sc, 74.0 * sc);
    let max_in = s.days.iter().map(|d| d.0).max().unwrap_or(0);
    let max_out = s.days.iter().map(|d| d.1).max().unwrap_or(0);
    let down = 40.0 * sc;
    let bw = gw / n as f32;
    // Ancho de barra con tope, centrada en su casilla.
    let thick = (bw - 6.0 * sc).clamp(1.0, 14.0 * sc);
    for (i, d) in s.days.iter().enumerate() {
        let bx = gx + i as f32 * bw + (bw - thick) / 2.0;
        if max_in > 0 && d.0 > 0 { let h = solid((d.0 as f32 / max_in as f32 * half).max(1.0), sc); rs.push(((bx, mid - h, thick, h), GRAY)); }
        if max_out > 0 && d.1 > 0 { let h = solid((d.1 as f32 / max_out as f32 * down).max(1.0), sc); rs.push(((bx, mid + 1.0, thick, h), BLUE)); }
    }
    rs.push(((gx, mid, gw, 1.0), GRAY));
    t(&mut tx, stats::fmt_n(max_in), x0, top, LIT, 14.0, false);
    t(&mut tx, "input".into(), x0, top + 17.0 * sc, DIM, 14.0, false);
    t(&mut tx, stats::fmt_n(max_out), x0, mid + 8.0 * sc, LIT, 14.0, false);
    t(&mut tx, "output".into(), x0, mid + 25.0 * sc, BLUE, 14.0, false);
    let ly = sy + 149.0 * sc;
    t(&mut tx, mon(s.from), gx, ly, DIM, 14.0, false);
    if n > 2 { t(&mut tx, mon(s.from + chrono::Duration::days(n as i64 / 2)), gx + gw / 2.0 - 25.0 * sc, ly, DIM, 14.0, false); }
    t(&mut tx, "today".into(), gx + gw - 40.0 * sc, ly, DIM, 14.0, false);

    t(&mut tx, "WHEN THEY WORK".into(), rx, sy, DIM, 14.0, false);
    let hmax = s.heat.iter().flatten().copied().max().unwrap_or(0);
    let busiest = (0..7).flat_map(|d| (0..24).map(move |h| (d, h))).max_by_key(|&(d, h)| s.heat[d][h]);
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    if let (Some((d, h)), true) = (busiest, hmax > 0) {
        let m = format!("most active {} {h} h", DAYS[d]);
        rule(&mut rs, rx + 140.0 * sc, rx + rw, &m, sy, sc);
        tr(&mut tx, m, rx + rw, sy, DIM, 14.0);
        
    }
    let (hx, hy) = (rx + 23.0 * sc, sy + 25.0 * sc);
    let (cell_w, cell) = ((rw - 21.0 * sc) / 24.0, 16.0 * sc);
    for (d, row) in s.heat.iter().enumerate() {
        t(&mut tx, ["M", "T", "W", "T", "F", "S", "S"][d].into(), rx, hy + d as f32 * cell - 2.0 * sc, DIM, 14.0, false);
        for (h, v) in row.iter().enumerate() {
            let k = if hmax > 0 { (*v as f32 / hmax as f32).sqrt() } else { 0.0 };
            let g = (0x1c as f32 + k * (0xd0 - 0x1c) as f32) as u8;
            rs.push(((hx + h as f32 * cell_w, hy + d as f32 * cell, cell_w - 2.0 * sc, cell - 2.0 * sc), [g, g, g + 2]));
        }
    }
    for h in [0, 6, 12, 18] { t(&mut tx, h.to_string(), hx + h as f32 * cell_w, hy + 7.0 * cell + 5.0 * sc, DIM, 14.0, false); }
    let lgx = rx + rw - 111.0 * sc;
    t(&mut tx, "less".into(), lgx, hy + 7.0 * cell + 26.0 * sc, DIM, 14.0, false);
    for (i, g) in [0x2a, 0x5a, 0x90, 0xd0].iter().enumerate() { rs.push(((lgx + 31.0 * sc + i as f32 * 11.5 * sc, hy + 7.0 * cell + 29.0 * sc, 10.0 * sc, 10.0 * sc), [*g, *g, *g])); }
    t(&mut tx, "more".into(), lgx + 80.0 * sc, hy + 7.0 * cell + 26.0 * sc, DIM, 14.0, false);

    // Modelos (izquierda) y agentes (derecha).
    let my = a.1 + 451.0 * sc;
    let mw = w * MODELS_W;
    let (ax, aw) = (x0 + mw + 38.0 * sc, w - mw - 38.0 * sc);
    t(&mut tx, "MODELS".into(), x0, my, DIM, 14.0, false);
    let mm = format!("{} model{}", s.models.len(), if s.models.len() == 1 { "" } else { "s" });
    rule(&mut rs, x0 + 68.0 * sc, x0 + mw, &mm, my, sc);
    tr(&mut tx, mm, x0 + mw, my, DIM, 14.0);
    let total: u64 = s.models.iter().map(|m| m.input + m.output).sum();
    let mut provs: Vec<(&str, u64)> = vec![];
    for m in &s.models {
        let p = stats::provider(&m.name);
        match provs.iter_mut().find(|x| x.0 == p) { Some(x) => x.1 += m.input + m.output, None => provs.push((p, m.input + m.output)) }
    }
    let pcol = |p: &str| match p { "Anthropic" => ORANGE, "Google" => BLUE, "OpenAI" => [0x3a, 0xb0, 0x8a], "xAI" => [0x9a, 0x7a, 0xe0], _ => GRAY };
    let mut bx = x0;
    for (p, v) in &provs {
        let bw = if total > 0 { *v as f32 / total as f32 * mw } else { 0.0 };
        // El tramo mínimo no se sale de la columna.
        let seg = (bw - 2.0 * sc).max(PILL_H * sc);
        rs.push(((bx.min(x0 + mw - seg), my + 26.0 * sc, seg, PILL_H * sc), pcol(p)));
        bx += bw;
    }
    let mut lx = x0;
    for (p, v) in &provs {
        let pct = if total > 0 { *v as f64 / total as f64 * 100.0 } else { 0.0 };
        let s = if pct > 0.0 && pct < 1.0 { format!("{p} <1%") } else { format!("{p} {pct:.0}%") };
        if let Some(i) = logo_of(p) { logos.push(((lx, my + 41.0 * sc, 14.0 * sc, 14.0 * sc), i)); }
        t(&mut tx, s.clone(), lx + 18.0 * sc, my + 39.0 * sc, DIM, 14.0, false);
        lx += (s.chars().count() as f32 * 7.4 + 32.0) * sc;
    }
    let (cin, cout, cl) = (x0 + mw - 143.0 * sc, x0 + mw - 71.0 * sc, x0 + mw);
    for (s, x) in [("input", cin), ("output", cout), ("tasks", cl)] { tr(&mut tx, s.into(), x, my + 65.0 * sc, DIM, 13.0); }
    let top_in = s.models.first().map_or(0, |m| m.input + m.output);
    let rowh = 26.0 * sc;
    let shown = 6;
    for (i, m) in s.models.iter().take(shown).enumerate() {
        let y = my + 90.0 * sc + i as f32 * rowh;
        let col = pcol(stats::provider(&m.name));
        if let Some(i) = logo_of(stats::provider(&m.name)) { logos.push(((x0, y + sc, 16.0 * sc, 16.0 * sc), i)); }
        let name: String = pretty(&m.name).chars().take(26).collect();
        t(&mut tx, name, x0 + 25.0 * sc, y, LIT, 15.0, false);
        let bmax = cin - 81.0 * sc - (x0 + 216.0 * sc);
        if top_in > 0 && bmax > 0.0 { rs.push(((x0 + 216.0 * sc, y + 6.0 * sc, ((m.input + m.output) as f32 / top_in as f32 * bmax).max(PILL_H * sc), PILL_H * sc), col)); }
        tr(&mut tx, stats::fmt_n(m.input), cin, y, LIT, 15.0);
        tr(&mut tx, stats::fmt_n(m.output), cout, y, LIT, 15.0);
        tr(&mut tx, if m.tasks > 0 { stats::group(m.tasks) } else { "—".into() }, cl, y, DIM, 15.0);
    }
    if s.models.len() > shown {
        let rest = &s.models[shown..];
        let y = my + 90.0 * sc + shown as f32 * rowh;
        t(&mut tx, format!("{} more", rest.len()), x0 + 25.0 * sc, y, DIM, 15.0, false);
        tr(&mut tx, stats::fmt_n(rest.iter().map(|m| m.input).sum()), cin, y, DIM, 15.0);
        tr(&mut tx, stats::fmt_n(rest.iter().map(|m| m.output).sum()), cout, y, DIM, 15.0);
        let tasks: u64 = rest.iter().map(|m| m.tasks).sum();
        if tasks > 0 { tr(&mut tx, stats::group(tasks), cl, y, DIM, 15.0); }
    }

    t(&mut tx, "AGENTS".into(), ax, my, DIM, 14.0, false);
    let note = "click one to see only its usage";
    tr(&mut tx, note.into(), ax + aw, my, DIM, 14.0);
    rule(&mut rs, ax + 66.0 * sc, ax + aw, note, my, sc);
    let (ain, aout, atime) = (ax + aw - 143.0 * sc, ax + aw - 71.0 * sc, ax + aw);
    for (s, x) in [("input", ain), ("output", aout), ("time", atime)] { tr(&mut tx, s.into(), x, my + 34.0 * sc, DIM, 13.0); }
    let most = s.agents.iter().map(|a| a.1.input + a.1.output).max().unwrap_or(0);
    for (row, i) in ORDER.into_iter().enumerate() {
        let (name, y) = (AGENTS[i], my + 60.0 * sc + row as f32 * rowh);
        let data = s.agents.iter().find(|a| a.0 == i).map(|a| &a.1);
        let on = data.is_some() && picked.is_none_or(|p| p == i);
        logos.push(((ax, y + sc, 16.0 * sc, 16.0 * sc), i));
        t(&mut tx, name.into(), ax + 26.0 * sc, y, if on { LIT } else { DIM }, 15.0, false);
        let (from, full) = (ax + 204.0 * sc, ain - 80.0 * sc - (ax + 204.0 * sc));
        match data {
            Some(d) => {
                if full > 0.0 && most > 0 { rs.push(((from, y + 6.0 * sc, ((d.input + d.output) as f32 / most as f32 * full).max(PILL_H * sc), PILL_H * sc), if on { [0x9a, 0x9a, 0xa2] } else { GRAY })); }
                for (v, x) in [(stats::fmt_n(d.input), ain), (stats::fmt_n(d.output), aout), (stats::fmt_dur(d.secs), atime)] { tr(&mut tx, v, x, y, if on { LIT } else { DIM }, 15.0); }
            }
            None => for x in [ain, aout, atime] { tr(&mut tx, "—".into(), x, y, DIM, 15.0); }
        }
    }
    (rs, scaled(tx, sc / raw), logos)
}

/// Los tamaños de letra salen en px lógicos de la ventana: se les aplica el mismo zoom.
fn scaled(mut tx: Vec<Txt>, k: f32) -> Vec<Txt> { for t in &mut tx { t.4 *= k; } tx }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nombres_de_modelo() {
        assert_eq!(pretty("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(pretty("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(pretty("gpt-6.1-sol"), "gpt-6.1-sol");
    }
    #[test]
    fn zonas_de_rango_y_cerrar() {
        let a = (500.0, 72.0, 2000.0, 1200.0);
        let hs = hits(a, 2.0);
        assert_eq!(hs.len(), 7);
        let close = hs.iter().find(|h| h.1 == Hit::Close).unwrap().0;
        assert_eq!(hit_at(a, 2.0, (close.0 + 4.0, close.1 + 4.0)), Some(Hit::Close));
        let r30 = hs.iter().find(|h| h.1 == Hit::Range(1)).unwrap().0;
        assert_eq!(hit_at(a, 2.0, (r30.0 + 2.0, r30.1 + 2.0)), Some(Hit::Range(1)));
        assert_eq!(hit_at(a, 2.0, (a.0 + 10.0, a.1 + 10.0)), None);
        // Todas dentro del área y sin solaparse.
        for (r, _) in &hs { assert!(r.0 >= a.0 && r.0 + r.2 <= a.0 + a.2); }
    }
    #[test]
    fn construye_sin_datos_y_con_datos() {
        let (rs, tx, _) = build((0.0, 0.0, 1600.0, 900.0), 1.0, None, 1, None, (0.0, 0.0));
        assert!(!rs.is_empty() && tx.iter().any(|t| t.0.starts_with("Leyendo")));
        let st = stats::compute(&[], &[], None, Some(30), chrono::Local::now().date_naive());
        let (_, tx, _) = build((0.0, 0.0, 1600.0, 900.0), 1.0, Some(&st), 1, None, (0.0, 0.0));
        assert!(tx.iter().any(|t| t.0 == "INPUT TOKENS") && tx.iter().any(|t| t.0 == "—"));
    }
}
