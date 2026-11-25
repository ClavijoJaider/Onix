//! Pantalla "Agent usage": cifras de Claude Code leídas de ~/.claude/projects (solo lectura).
//! Se dibuja encima del área de paneles; esc o el botón la cierran. Lo que no se mide sale con "—".
use std::sync::{Arc, Mutex};
use std::time::Instant;
use usage::stats::{self, Stats};

pub type R = (f32, f32, f32, f32);
pub type Rgb = [u8; 3];
/// (texto, x, y, color, tamaño en px lógicos, negrita)
pub type Txt = (String, f32, f32, Rgb, f32, bool);

pub const RANGES: [(&str, Option<i64>); 4] = [("7 days", Some(7)), ("30 days", Some(30)), ("90 days", Some(90)), ("All", None)];

const DIM: Rgb = [0x8a, 0x8a, 0x94];
const LIT: Rgb = [0xe6, 0xe6, 0xea];
const CARD: Rgb = [0x12, 0x12, 0x15];
const LINE: Rgb = [0x26, 0x26, 0x2c];
const GRAY: Rgb = [0x4a, 0x4a, 0x52];
const BLUE: Rgb = [0x4a, 0x9e, 0xff];
const GREEN: Rgb = [0x5a, 0xc0, 0x6a];
const RED: Rgb = [0xe0, 0x60, 0x60];
const ORANGE: Rgb = [0xd9, 0x77, 0x57];

/// Agentes de la lista (solo Claude Code tiene fuente de datos por ahora).
const AGENTS: [&str; 6] = ["Claude Code", "Codex", "Antigravity", "opencode", "Grok CLI", "Junie"];

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Hit { Range(usize), Close }

pub struct View {
    pub open: bool,
    pub range: usize,
    /// Registros leídos (con la hora de lectura); se relee al abrir si tienen más de 60 s.
    pub data: Arc<Mutex<Option<(Instant, Vec<usage::Record>)>>>,
    pub loading: Arc<std::sync::atomic::AtomicBool>,
    cache: Option<(usize, Instant, Stats)>,
}

impl View {
    pub fn new() -> Self { View { open: false, range: 1, data: Arc::default(), loading: Arc::default(), cache: None } }

    /// Lee los jsonl en un hilo; `done` avisa al terminar (para redibujar).
    pub fn load(&self, done: impl FnOnce() + Send + 'static) {
        use std::sync::atomic::Ordering;
        let fresh = self.data.lock().unwrap().as_ref().is_some_and(|(t, _)| t.elapsed().as_secs() < 60);
        if fresh || self.loading.swap(true, Ordering::SeqCst) { return; }
        let (data, loading) = (self.data.clone(), self.loading.clone());
        std::thread::spawn(move || {
            let mut s = usage::Scan::default();
            if let Ok(h) = std::env::var("HOME") { s.scan_dir(&std::path::Path::new(&h).join(".claude/projects")); }
            *data.lock().unwrap() = Some((Instant::now(), s.records.into_values().collect()));
            loading.store(false, Ordering::SeqCst);
            done();
        });
    }

    /// Estadísticas del rango elegido (se recalculan solo si cambian datos o rango).
    pub fn stats(&mut self) -> Option<&Stats> {
        let d = self.data.lock().unwrap();
        let (t, recs) = d.as_ref()?;
        if !self.cache.as_ref().is_some_and(|(r, ct, _)| *r == self.range && ct == t) {
            let s = stats::compute(recs, RANGES[self.range].1, chrono::Local::now().date_naive());
            self.cache = Some((self.range, *t, s));
        }
        drop(d);
        self.cache.as_ref().map(|c| &c.2)
    }
}

/// Zonas clicables (px) dentro del área `a`.
pub fn hits(a: R, sc: f32) -> Vec<(R, Hit)> {
    let (x1, y0) = (a.0 + a.2 - 24.0 * sc, a.1 + 18.0 * sc);
    let mut v = vec![((x1 - 40.0 * sc, y0, 40.0 * sc, 24.0 * sc), Hit::Close)];
    let mut x = x1 - 52.0 * sc;
    for i in (0..RANGES.len()).rev() {
        let w = (RANGES[i].0.chars().count() as f32 * 7.2 + 20.0) * sc;
        x -= w;
        v.push(((x, y0, w, 24.0 * sc), Hit::Range(i)));
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

/// Mini barras de una serie dentro de `r`.
fn spark(out: &mut Vec<(R, Rgb)>, r: R, vals: &[f64], c: Rgb) {
    let max = vals.iter().cloned().fold(0.0, f64::max);
    if max <= 0.0 || vals.is_empty() { return; }
    let bw = r.2 / vals.len() as f32;
    for (i, v) in vals.iter().enumerate() {
        let h = (*v / max) as f32 * r.3;
        if h > 0.0 { out.push(((r.0 + i as f32 * bw, r.1 + r.3 - h.max(1.0), (bw - 2.0).max(1.0), h.max(1.0)), c)); }
    }
}

/// Rects y textos de la pantalla en el área `a` (px). `st` None = cargando.
pub fn build(a: R, sc: f32, st: Option<&Stats>, range: usize, mouse: (f32, f32)) -> (Vec<(R, Rgb)>, Vec<Txt>) {
    let mut rs: Vec<(R, Rgb)> = vec![(a, [0x08, 0x08, 0x0a])];
    let mut tx: Vec<Txt> = vec![];
    let (x0, y0, w) = (a.0 + 24.0 * sc, a.1 + 16.0 * sc, a.2 - 48.0 * sc);
    let t = |tx: &mut Vec<Txt>, s: String, x: f32, y: f32, c: Rgb, size: f32, b: bool| tx.push((s, x, y, c, size, b));
    t(&mut tx, "Agent usage".into(), x0, y0, LIT, 15.0, true);
    for (r, h) in hits(a, sc) {
        let on = matches!(h, Hit::Range(i) if i == range);
        let hov = mouse.0 >= r.0 && mouse.0 < r.0 + r.2 && mouse.1 >= r.1 && mouse.1 < r.1 + r.3;
        if on || hov { rs.push((r, if on { [0x2a, 0x2a, 0x30] } else { [0x1a, 0x1a, 0x20] })); }
        let (label, c) = match h { Hit::Range(i) => (RANGES[i].0, if on { LIT } else { DIM }), Hit::Close => ("esc", DIM) };
        t(&mut tx, label.into(), r.0 + 10.0 * sc, r.1 + 5.0 * sc, c, 12.0, false);
    }
    let Some(s) = st else {
        t(&mut tx, "Leyendo el historial de Claude Code…".into(), x0, y0 + 22.0 * sc, DIM, 12.0, false);
        return (rs, tx);
    };
    let today = s.to;
    t(&mut tx, format!("{} – today", mon(s.from)), x0, y0 + 20.0 * sc, DIM, 12.0, false);

    // Tarjetas.
    let (cy, ch, gap) = (y0 + 52.0 * sc, 112.0 * sc, 12.0 * sc);
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
        t(&mut tx, label.into(), x + 14.0 * sc, cy + 12.0 * sc, DIM, 11.0, false);
        if let Some(d) = delta {
            let s = if d > 999 { "+999%".to_string() } else if d >= 0 { format!("+{d}%") } else { format!("{d}%") };
            let bw = (s.len() as f32 * 6.6 + 12.0) * sc;
            rs.push(((x + cw - 14.0 * sc - bw, cy + 9.0 * sc, bw, 18.0 * sc), if d >= 0 { [0x14, 0x2a, 0x1a] } else { [0x2e, 0x16, 0x16] }));
            t(&mut tx, s, x + cw - 14.0 * sc - bw + 6.0 * sc, cy + 11.0 * sc, if d >= 0 { GREEN } else { RED }, 11.0, false);
        }
        t(&mut tx, big, x + 14.0 * sc, cy + 30.0 * sc, LIT, 26.0, false);
        t(&mut tx, sub, x + 14.0 * sc, cy + 64.0 * sc, DIM, 12.0, false);
        spark(&mut rs, (x + 14.0 * sc, cy + ch - 30.0 * sc, cw - 28.0 * sc, 20.0 * sc), &series, GRAY);
    }

    // Tokens por día (izquierda) y mapa de horas (derecha).
    let (sy, sh) = (cy + ch + 28.0 * sc, 170.0 * sc);
    let lw = w * 0.56;
    let (rx, rw) = (x0 + lw + 32.0 * sc, w - lw - 32.0 * sc);
    let active = s.days.iter().filter(|d| d.2 > 0).count();
    let n = s.days.len().max(1);
    t(&mut tx, "TOKENS BY DAY".into(), x0, sy, DIM, 11.0, false);
    let per_day = (c.input + c.output) / n as u64;
    let head = format!("{active} of {n} days with agents · {}/day", stats::fmt_n(per_day));
    t(&mut tx, head.clone(), x0 + lw - head.chars().count() as f32 * 6.6 * sc, sy, DIM, 11.0, false);
    rs.push(((x0 + 110.0 * sc, sy + 7.0 * sc, lw - 110.0 * sc - head.chars().count() as f32 * 6.6 * sc - 10.0 * sc, 1.0), LINE));
    let (gx, gw, top, mid, half) = (x0 + 70.0 * sc, lw - 70.0 * sc, sy + 22.0 * sc, sy + 22.0 * sc + (sh - 40.0 * sc) * 0.62, (sh - 40.0 * sc) * 0.62);
    let max_in = s.days.iter().map(|d| d.0).max().unwrap_or(0);
    let max_out = s.days.iter().map(|d| d.1).max().unwrap_or(0);
    let down = (sh - 40.0 * sc) * 0.38;
    let bw = gw / n as f32;
    for (i, d) in s.days.iter().enumerate() {
        let bx = gx + i as f32 * bw;
        if max_in > 0 && d.0 > 0 { let h = (d.0 as f32 / max_in as f32 * half).max(1.0); rs.push(((bx, mid - h, (bw - 3.0 * sc).max(1.0), h), GRAY)); }
        if max_out > 0 && d.1 > 0 { let h = (d.1 as f32 / max_out as f32 * down).max(1.0); rs.push(((bx, mid + 1.0, (bw - 3.0 * sc).max(1.0), h), BLUE)); }
    }
    rs.push(((gx, mid, gw, 1.0), LINE));
    t(&mut tx, stats::fmt_n(max_in), x0, top, LIT, 12.0, false);
    t(&mut tx, "input".into(), x0, top + 16.0 * sc, DIM, 11.0, false);
    t(&mut tx, stats::fmt_n(max_out), x0, mid + 8.0 * sc, LIT, 12.0, false);
    t(&mut tx, "output".into(), x0, mid + 24.0 * sc, BLUE, 11.0, false);
    let ly = mid + down + 6.0 * sc;
    t(&mut tx, mon(s.from), gx, ly, DIM, 11.0, false);
    if n > 2 { t(&mut tx, mon(s.from + chrono::Duration::days(n as i64 / 2)), gx + gw / 2.0 - 20.0 * sc, ly, DIM, 11.0, false); }
    t(&mut tx, "today".into(), gx + gw - 36.0 * sc, ly, DIM, 11.0, false);

    t(&mut tx, "WHEN THEY WORK".into(), rx, sy, DIM, 11.0, false);
    let hmax = s.heat.iter().flatten().copied().max().unwrap_or(0);
    let busiest = (0..7).flat_map(|d| (0..24).map(move |h| (d, h))).max_by_key(|&(d, h)| s.heat[d][h]);
    const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    if let (Some((d, h)), true) = (busiest, hmax > 0) {
        let m = format!("most active {} {h} h", DAYS[d]);
        t(&mut tx, m.clone(), rx + rw - m.chars().count() as f32 * 6.6 * sc, sy, DIM, 11.0, false);
    }
    let (hx, hy) = (rx + 22.0 * sc, sy + 22.0 * sc);
    let cell = ((rw - 22.0 * sc) / 24.0).min((sh - 60.0 * sc) / 7.0);
    for (d, row) in s.heat.iter().enumerate() {
        t(&mut tx, ["M", "T", "W", "T", "F", "S", "S"][d].into(), rx, hy + d as f32 * cell, DIM, 11.0, false);
        for (h, v) in row.iter().enumerate() {
            let k = if hmax > 0 { (*v as f32 / hmax as f32).sqrt() } else { 0.0 };
            let g = (0x1c as f32 + k * (0xd0 - 0x1c) as f32) as u8;
            rs.push(((hx + h as f32 * cell, hy + d as f32 * cell, cell - 2.0 * sc, cell - 2.0 * sc), [g, g, g + 2]));
        }
    }
    for h in [0, 6, 12, 18] { t(&mut tx, h.to_string(), hx + h as f32 * cell, hy + 7.0 * cell + 4.0 * sc, DIM, 11.0, false); }
    let lgx = rx + rw - 120.0 * sc;
    t(&mut tx, "less".into(), lgx, hy + 7.0 * cell + 22.0 * sc, DIM, 11.0, false);
    for (i, g) in [0x2a, 0x5a, 0x90, 0xd0].iter().enumerate() { rs.push(((lgx + 36.0 * sc + i as f32 * 12.0 * sc, hy + 7.0 * cell + 25.0 * sc, 10.0 * sc, 10.0 * sc), [*g, *g, *g])); }
    t(&mut tx, "more".into(), lgx + 88.0 * sc, hy + 7.0 * cell + 22.0 * sc, DIM, 11.0, false);

    // Modelos (izquierda) y agentes (derecha).
    let my = sy + sh + 40.0 * sc;
    let mw = w * 0.48;
    let (ax, aw) = (x0 + mw + 32.0 * sc, w - mw - 32.0 * sc);
    t(&mut tx, "MODELS".into(), x0, my, DIM, 11.0, false);
    let mm = format!("{} models", s.models.len());
    t(&mut tx, mm.clone(), x0 + mw - mm.len() as f32 * 6.6 * sc, my, DIM, 11.0, false);
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
        rs.push(((bx, my + 20.0 * sc, (bw - 2.0 * sc).max(1.0), 5.0 * sc), pcol(p)));
        bx += bw;
    }
    let mut lx = x0;
    for (p, v) in &provs {
        let s = format!("● {p} {:.0}%", if total > 0 { *v as f64 / total as f64 * 100.0 } else { 0.0 });
        t(&mut tx, s.clone(), lx, my + 32.0 * sc, pcol(p), 11.0, false);
        lx += (s.chars().count() as f32 * 6.6 + 16.0) * sc;
    }
    let (cin, cout, cl) = (x0 + mw - 170.0 * sc, x0 + mw - 100.0 * sc, x0 + mw - 36.0 * sc);
    t(&mut tx, "input".into(), cin, my + 56.0 * sc, DIM, 11.0, false);
    t(&mut tx, "output".into(), cout, my + 56.0 * sc, DIM, 11.0, false);
    t(&mut tx, "tasks".into(), cl, my + 56.0 * sc, DIM, 11.0, false);
    let top_in = s.models.first().map_or(0, |m| m.input + m.output);
    let rowh = 22.0 * sc;
    let shown = 6;
    for (i, m) in s.models.iter().take(shown).enumerate() {
        let y = my + 76.0 * sc + i as f32 * rowh;
        let col = pcol(stats::provider(&m.name));
        t(&mut tx, "●".into(), x0, y, col, 11.0, false);
        let name: String = m.name.chars().take(26).collect();
        t(&mut tx, name, x0 + 16.0 * sc, y, LIT, 12.0, false);
        let bmax = cin - 12.0 * sc - (x0 + 200.0 * sc);
        if top_in > 0 && bmax > 0.0 { rs.push(((x0 + 200.0 * sc, y + 5.0 * sc, ((m.input + m.output) as f32 / top_in as f32 * bmax).max(2.0 * sc), 5.0 * sc), col)); }
        t(&mut tx, stats::fmt_n(m.input), cin, y, LIT, 12.0, false);
        t(&mut tx, stats::fmt_n(m.output), cout, y, LIT, 12.0, false);
        t(&mut tx, stats::group(m.tasks), cl, y, DIM, 12.0, false);
    }
    if s.models.len() > shown {
        let rest = &s.models[shown..];
        let y = my + 76.0 * sc + shown as f32 * rowh;
        t(&mut tx, format!("{} more", rest.len()), x0 + 16.0 * sc, y, DIM, 12.0, false);
        t(&mut tx, stats::fmt_n(rest.iter().map(|m| m.input).sum()), cin, y, DIM, 12.0, false);
        t(&mut tx, stats::fmt_n(rest.iter().map(|m| m.output).sum()), cout, y, DIM, 12.0, false);
    }

    t(&mut tx, "AGENTS".into(), ax, my, DIM, 11.0, false);
    let note = "solo Claude Code tiene datos por ahora";
    t(&mut tx, note.into(), ax + aw - note.chars().count() as f32 * 6.6 * sc, my, DIM, 11.0, false);
    let (ain, aout, atime) = (ax + aw - 190.0 * sc, ax + aw - 120.0 * sc, ax + aw - 50.0 * sc);
    t(&mut tx, "input".into(), ain, my + 56.0 * sc, DIM, 11.0, false);
    t(&mut tx, "output".into(), aout, my + 56.0 * sc, DIM, 11.0, false);
    t(&mut tx, "time".into(), atime, my + 56.0 * sc, DIM, 11.0, false);
    for (i, name) in AGENTS.iter().enumerate() {
        let y = my + 76.0 * sc + i as f32 * rowh;
        t(&mut tx, (*name).into(), ax, y, if i == 0 { LIT } else { DIM }, 12.0, false);
        if i == 0 {
            rs.push(((ax + 130.0 * sc, y + 5.0 * sc, (ain - 16.0 * sc - (ax + 130.0 * sc)).max(2.0), 5.0 * sc), [0x9a, 0x9a, 0xa2]));
            t(&mut tx, stats::fmt_n(c.input), ain, y, LIT, 12.0, false);
            t(&mut tx, stats::fmt_n(c.output), aout, y, LIT, 12.0, false);
            t(&mut tx, stats::fmt_dur(c.secs), atime, y, LIT, 12.0, false);
        } else {
            for x in [ain, aout, atime] { t(&mut tx, "—".into(), x, y, DIM, 12.0, false); }
        }
    }
    let _ = today;
    (rs, tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zonas_de_rango_y_cerrar() {
        let a = (500.0, 72.0, 2000.0, 1200.0);
        let hs = hits(a, 2.0);
        assert_eq!(hs.len(), 5);
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
        let (rs, tx) = build((0.0, 0.0, 1600.0, 900.0), 1.0, None, 1, (0.0, 0.0));
        assert!(!rs.is_empty() && tx.iter().any(|t| t.0.starts_with("Leyendo")));
        let st = stats::compute(&[], Some(30), chrono::Local::now().date_naive());
        let (_, tx) = build((0.0, 0.0, 1600.0, 900.0), 1.0, Some(&st), 1, (0.0, 0.0));
        assert!(tx.iter().any(|t| t.0 == "INPUT TOKENS") && tx.iter().any(|t| t.0 == "—"));
    }
}
