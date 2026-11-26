//! Cifras de la pantalla "Agent usage" a partir de los registros de Claude Code (solo lo medido).
use crate::{Past, Record, Usage};
use chrono::{Datelike, Duration, Local, NaiveDate, Timelike};
use std::collections::{HashMap, HashSet};

/// Hueco máximo entre dos respuestas de una sesión para contarlo como tiempo de trabajo.
pub const IDLE_GAP_S: i64 = 10 * 60;

#[derive(Default, Debug, Clone)]
pub struct Totals {
    pub input: u64,      // input + cache (lectura y escritura)
    pub cache_read: u64,
    pub output: u64,
    pub turns: u64,
    pub tasks: u64,      // sesiones principales con actividad
    pub secs: i64,       // tiempo de agente (suma por sesión, sin huecos > IDLE_GAP_S)
    pub max_at_once: u64,
    /// Días tomados del resumen de Claude, y mensajes (los suyos más las respuestas medidas).
    pub past_days: u64,
    pub msgs: u64,
}

#[derive(Default, Debug, Clone)]
pub struct Row { pub name: String, pub input: u64, pub output: u64, pub tasks: u64, pub secs: i64 }

#[derive(Default, Debug, Clone)]
pub struct Stats {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub cur: Totals,
    /// Mismo periodo inmediatamente anterior (para el % de cambio); None con "All".
    pub prev: Option<Totals>,
    /// Por día (from..=to): (input, output, turns, secs).
    pub days: Vec<(u64, u64, u64, i64)>,
    /// Respuestas por [día de la semana lun=0][hora].
    pub heat: [[u64; 24]; 7],
    pub models: Vec<Row>,
    /// Primer día con registros detallados (horas y tiempo de agente salen solo de ahí).
    pub detail_from: Option<NaiveDate>,
}

fn inp(u: &Usage) -> u64 { u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens }

/// Totales con día en [from, to]. En los días de `past` los tokens salen del resumen, no de los registros.
fn totals(recs: &[&Record], past: &HashMap<NaiveDate, &Past>, from: NaiveDate, to: NaiveDate) -> (Totals, HashMap<NaiveDate, i64>) {
    let mut t = Totals::default();
    // Línea de tiempo por sesión principal y otra por sus subagentes (para el "a la vez").
    let mut by_sess: HashMap<(&str, bool), Vec<i64>> = HashMap::new();
    let mut tasks: HashSet<&str> = HashSet::new();
    for r in recs.iter().filter(|r| r.day >= from && r.day <= to) {
        if !past.contains_key(&r.day) {
            t.input += inp(&r.usage);
            t.cache_read += r.usage.cache_read_input_tokens;
            t.output += r.usage.output_tokens;
            t.turns += 1;
            t.msgs += 1;
            tasks.insert(&r.session);
        }
        by_sess.entry((&r.session, r.sub)).or_default().push(r.at.timestamp());
    }
    t.tasks = tasks.len() as u64;
    for p in past.values().filter(|p| p.day >= from && p.day <= to) {
        t.past_days += 1;
        t.tasks += p.sessions;
        t.msgs += p.messages;
        for (_, u) in &p.models { t.input += inp(u); t.cache_read += u.cache_read_input_tokens; t.output += u.output_tokens; }
    }
    let mut per_day: HashMap<NaiveDate, i64> = HashMap::new();
    let mut minutes: HashMap<i64, u64> = HashMap::new();
    for ts in by_sess.values_mut() {
        ts.sort_unstable();
        let mut seen = HashSet::new();
        for w in ts.windows(2) {
            let gap = w[1] - w[0];
            if gap <= IDLE_GAP_S {
                t.secs += gap;
                if let Some(d) = chrono::DateTime::from_timestamp(w[1], 0) { *per_day.entry(d.with_timezone(&Local).date_naive()).or_default() += gap; }
                for m in (w[0] / 60)..=(w[1] / 60) { if seen.insert(m) { *minutes.entry(m).or_default() += 1; } }
            }
        }
    }
    t.max_at_once = minutes.values().copied().max().unwrap_or(if t.turns > 0 { 1 } else { 0 });
    (t, per_day)
}

/// Estadísticas de los últimos `days` días hasta `today` (None = todo el historial).
pub fn compute(records: &[Record], history: &[Past], days: Option<i64>, today: NaiveDate) -> Stats {
    let recs: Vec<&Record> = records.iter().filter(|r| !r.model.starts_with('<')).collect();
    // El resumen manda en los días en que tiene más tokens que los jsonl (ya borrados en parte).
    let mut measured: HashMap<NaiveDate, u64> = HashMap::new();
    for r in &recs { *measured.entry(r.day).or_default() += r.usage.total(); }
    let past: HashMap<NaiveDate, &Past> = history.iter()
        .filter(|p| p.models.iter().map(|m| m.1.total()).sum::<u64>() > measured.get(&p.day).copied().unwrap_or(0))
        .map(|p| (p.day, p)).collect();
    let detail_from = recs.iter().map(|r| r.day).min();
    let first = detail_from.into_iter().chain(past.keys().copied()).min().unwrap_or(today);
    let from = days.map_or(first, |n| today - Duration::days(n - 1));
    let (cur, per_day) = totals(&recs, &past, from, today);
    let prev = days.map(|n| totals(&recs, &past, from - Duration::days(n), from - Duration::days(1)).0);
    let n = (today - from).num_days().max(0) as usize + 1;
    let mut daysv = vec![(0u64, 0u64, 0u64, 0i64); n];
    let mut heat = [[0u64; 24]; 7];
    let mut models: HashMap<&str, (Row, HashSet<&str>)> = HashMap::new();
    for r in recs.iter().filter(|r| r.day >= from && r.day <= today) {
        let d = &mut daysv[(r.day - from).num_days() as usize];
        let detail = !past.contains_key(&r.day);
        if detail { d.0 += inp(&r.usage); d.1 += r.usage.output_tokens; d.2 += 1; }
        heat[r.at.weekday().num_days_from_monday() as usize][r.at.hour() as usize] += 1;
        let m = models.entry(&r.model).or_insert_with(|| (Row { name: r.model.clone(), ..Default::default() }, HashSet::new()));
        if detail { m.0.input += inp(&r.usage); m.0.output += r.usage.output_tokens; }
        m.1.insert(&r.session);
    }
    for p in past.values().filter(|p| p.day >= from && p.day <= today) {
        let d = &mut daysv[(p.day - from).num_days() as usize];
        d.2 += p.messages.max(1);
        for (name, u) in &p.models {
            d.0 += inp(u);
            d.1 += u.output_tokens;
            let m = models.entry(name).or_insert_with(|| (Row { name: name.clone(), ..Default::default() }, HashSet::new()));
            m.0.input += inp(u);
            m.0.output += u.output_tokens;
        }
    }
    for (day, s) in per_day { if day >= from && day <= today { daysv[(day - from).num_days() as usize].3 += s; } }
    let mut models: Vec<Row> = models.into_values().map(|(mut r, s)| { r.tasks = s.len() as u64; r }).collect();
    models.sort_by(|a, b| (b.input + b.output).cmp(&(a.input + a.output)).then(a.name.cmp(&b.name)));
    Stats { from, to: today, cur, prev, days: daysv, heat, models, detail_from }
}

/// Cambio porcentual redondeado (None si no hay base).
pub fn change(cur: u64, prev: u64) -> Option<i64> {
    (prev > 0).then(|| ((cur as f64 - prev as f64) / prev as f64 * 100.0).round() as i64)
}

/// 11.6B, 61.3M, 22.9K, 744.
pub fn fmt_n(n: u64) -> String {
    let f = n as f64;
    let one = |x: f64, s: &str| if x >= 100.0 { format!("{x:.0}{s}") } else { format!("{x:.1}{s}") };
    if f >= 1e9 { one(f / 1e9, "B") } else if f >= 1e6 { one(f / 1e6, "M") } else if f >= 1e4 { one(f / 1e3, "K") } else { group(n) }
}
/// 1,110
pub fn group(n: u64) -> String {
    let s = n.to_string();
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() { if i > 0 && (s.len() - i) % 3 == 0 { o.push(','); } o.push(c); }
    o
}
/// 347 h, 9.2 h, 23 min.
pub fn fmt_dur(secs: i64) -> String {
    let h = secs as f64 / 3600.0;
    if h >= 10.0 { format!("{h:.0} h") } else if h >= 1.0 { format!("{h:.1} h") } else { format!("{} min", (secs / 60).max(if secs > 0 { 1 } else { 0 })) }
}
/// Empresa del modelo según su nombre.
pub fn provider(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("claude") || ["opus", "sonnet", "haiku", "fable"].iter().any(|k| m.contains(k)) { "Anthropic" }
    else if m.contains("gemini") { "Google" }
    else if m.contains("gpt") || m.starts_with("o1") || m.starts_with("o3") || m.starts_with("o4") || m.contains("codex") { "OpenAI" }
    else if m.contains("grok") { "xAI" }
    else { "Otro" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn rec(day: u32, h: u32, m: u32, sess: &str, model: &str, i: u64, o: u64) -> Record {
        let at = Local.with_ymd_and_hms(2026, 11, day, h, m, 0).unwrap();
        Record { day: at.date_naive(), at, session: sess.into(), sub: false, model: model.into(), project: "p".into(),
            usage: Usage { input_tokens: i, output_tokens: o, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 } }
    }

    #[test]
    fn calcula_periodo() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        let r = vec![
            rec(24, 10, 0, "a", "claude-opus-5-5", 100, 10), rec(24, 10, 5, "a", "claude-opus-5-5", 100, 10),
            rec(24, 10, 3, "b", "claude-sonnet-5-5", 50, 5), rec(24, 10, 30, "a", "claude-opus-5-5", 1, 1),
            rec(20, 9, 0, "c", "claude-opus-5-5", 7, 7), rec(10, 9, 0, "d", "claude-opus-5-5", 1000, 1000),
            rec(24, 11, 0, "e", "<synthetic>", 9, 9),
        ];
        let s = compute(&r, &[], Some(7), today);
        assert_eq!(s.from, NaiveDate::from_ymd_opt(2026, 11, 18).unwrap());
        assert_eq!((s.cur.input, s.cur.output, s.cur.turns, s.cur.tasks), (258, 33, 5, 3));
        // a: 10:00→10:05 cuenta 300 s; 10:05→10:30 es hueco > 10 min.
        assert_eq!(s.cur.secs, 300);
        assert_eq!(s.days.len(), 7);
        assert_eq!(s.days[6].2, 4);
        assert_eq!(s.heat[Local.with_ymd_and_hms(2026, 11, 24, 0, 0, 0).unwrap().weekday().num_days_from_monday() as usize][10], 4);
        assert_eq!(s.models[0].name, "claude-opus-5-5");
        assert_eq!(s.models[0].tasks, 2);
        assert_eq!(s.prev.as_ref().unwrap().turns, 0);
        let all = compute(&r, &[], None, today);
        assert_eq!(all.cur.turns, 6);
        assert!(all.prev.is_none());
    }
    #[test]
    fn suma_el_resumen_de_claude() {
        let past = crate::history(r#"{"dailyActivity":[{"date":"2026-11-20","sessionCount":3,"messageCount":40}],
            "dailyModelTokens":[{"date":"2026-11-20","tokensByModel":{"claude-opus-5-5":1000}},{"date":"2026-11-24","tokensByModel":{"claude-opus-5-5":5}}],
            "modelUsage":{"claude-opus-5-5":{"inputTokens":10,"outputTokens":10,"cacheReadInputTokens":70,"cacheCreationInputTokens":10}}}"#);
        let today = NaiveDate::from_ymd_opt(2026, 11, 24).unwrap();
        // El 24 hay más tokens medidos que en el resumen: mandan los registros.
        let s = compute(&[rec(24, 10, 0, "a", "claude-opus-5-5", 100, 10)], &past, Some(7), today);
        assert_eq!((s.cur.input, s.cur.output, s.cur.cache_read), (1000, 110, 700));
        assert_eq!((s.cur.tasks, s.cur.msgs, s.cur.turns, s.cur.past_days), (4, 41, 1, 1));
        assert_eq!((s.models.len(), s.models[0].input + s.models[0].output), (1, 1110));
        assert!(crate::history("no es json").is_empty());
    }
    #[test]
    fn formatos() {
        assert_eq!((fmt_n(11_600_000_000), fmt_n(61_300_000), fmt_n(22_900), fmt_n(744), fmt_n(1110)), ("11.6B".into(), "61.3M".into(), "22.9K".into(), "744".into(), "1,110".into()));
        assert_eq!((fmt_dur(347 * 3600), fmt_dur(33120), fmt_dur(23 * 60), fmt_dur(0)), ("347 h".into(), "9.2 h".into(), "23 min".into(), "0 min".into()));
        assert_eq!((change(150, 100), change(5, 0)), (Some(50), None));
        assert_eq!((provider("claude-opus-5-5"), provider("gpt-6.2-astra"), provider("gemini-4")), ("Anthropic", "OpenAI", "Google"));
    }
}
