//! Lee los jsonl de Claude Code y agrega el uso de tokens (solo cifras).
//!
//! Duplicados (datos reales, 34.678 líneas de asistente con usage): un mismo
//! requestId aparece en varias líneas (una por bloque de contenido) con el mismo
//! message.id; 20.079 líneas sobraban. El usage es idéntico salvo en streaming
//! parcial (7 casos), donde la última línea trae el mayor output_tokens. Criterio:
//! una entrada por requestId (global, entre archivos), conservando la de mayor
//! output_tokens (empate: la última). Sin requestId se usa message.id.
pub mod stats;

use chrono::{DateTime, Local, NaiveDate};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use walkdir::WalkDir;

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type")]
    kind: Option<&'a str>,
    message: Option<Msg<'a>>,
    timestamp: Option<&'a str>,
    cwd: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
}

#[derive(Deserialize)]
struct Msg<'a> {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    #[serde(skip)]
    _p: std::marker::PhantomData<&'a ()>,
}

#[derive(Deserialize, Clone, Copy, Default, Debug, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_creation_input_tokens += o.cache_creation_input_tokens;
        self.cache_read_input_tokens += o.cache_read_input_tokens;
    }
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

#[derive(Clone, Debug)]
pub struct Record {
    pub day: NaiveDate,
    /// Hora local de la respuesta.
    pub at: DateTime<Local>,
    /// Sesión (id del jsonl; los subagentes cuentan en la sesión que los lanzó).
    pub session: String,
    /// true si la línea vino de un subagente.
    pub sub: bool,
    pub model: String,
    pub project: String,
    pub usage: Usage,
}

#[derive(Default)]
pub struct Scan {
    pub records: HashMap<String, Record>,
    pub invalid: u64,
    pub duplicates: u64,
    unkeyed: u64,
    cur: (String, bool),
}

impl Scan {
    /// Procesa una línea; las que no son de asistente con usage se ignoran.
    pub fn feed(&mut self, line: &str) {
        if !line.contains("\"usage\"") {
            return;
        }
        let Ok(l) = serde_json::from_str::<Line>(line) else {
            self.invalid += 1;
            return;
        };
        let (Some(m), Some(ts)) = (l.message, l.timestamp) else { return };
        let Some(usage) = m.usage else { return };
        if l.kind != Some("assistant") {
            return;
        }
        let Ok(t) = DateTime::parse_from_rfc3339(ts) else {
            self.invalid += 1;
            return;
        };
        let at = t.with_timezone(&Local);
        let rec = Record {
            day: at.date_naive(), at, session: self.cur.0.clone(), sub: self.cur.1,
            model: m.model.unwrap_or_else(|| "?".into()),
            project: l.cwd.unwrap_or_else(|| "?".into()),
            usage,
        };
        let key = match l.request_id.or(m.id) {
            Some(k) => k,
            None => {
                self.unkeyed += 1;
                format!("#{}", self.unkeyed)
            }
        };
        match self.records.get_mut(&key) {
            Some(old) => {
                self.duplicates += 1;
                if rec.usage.output_tokens >= old.usage.output_tokens {
                    *old = rec;
                }
            }
            None => {
                self.records.insert(key, rec);
            }
        }
    }

    pub fn scan_dir(&mut self, root: &Path) {
        for e in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if !e.file_type().is_file() || e.path().extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let Ok(f) = File::open(e.path()) else { continue };
            self.cur = session_of(e.path());
            let mut r = BufReader::new(f);
            let mut buf = String::new();
            // read_line (no lines()): una línea con UTF-8 inválido no aborta el archivo.
            loop {
                buf.clear();
                match r.read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => self.feed(&buf),
                    Err(_) => self.invalid += 1, // shortcut: salta el resto del archivo si el error es de lectura
                }
                if buf.is_empty() {
                    break;
                }
            }
        }
    }
}

/// (sesión, es_subagente) de un jsonl: `<id>.jsonl` o `<id>/subagents/<x>.jsonl`.
pub fn session_of(p: &Path) -> (String, bool) {
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = p.parent();
    if parent.and_then(|d| d.file_name()).is_some_and(|n| n == "subagents") {
        if let Some(id) = parent.and_then(|d| d.parent()).and_then(|d| d.file_name()) { return (id.to_string_lossy().into_owned(), true); }
    }
    (stem, false)
}

#[derive(Default, Debug)]
pub struct Agg {
    pub usage: Usage,
    pub turns: u64,
}

pub type Groups = Vec<(String, Agg)>;

/// Agrega por clave los registros con día >= since, ordenado por total desc.
pub fn group_by<F: Fn(&Record) -> String>(s: &Scan, since: NaiveDate, key: F) -> Groups {
    let mut m: HashMap<String, Agg> = HashMap::new();
    for r in s.records.values().filter(|r| r.day >= since) {
        let a = m.entry(key(r)).or_default();
        a.usage.add(&r.usage);
        a.turns += 1;
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.usage.total().cmp(&a.1.usage.total()).then(a.0.cmp(&b.0)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r#"{"type":"assistant","requestId":"r1","timestamp":"2026-11-10T15:00:00Z","cwd":"/p/a","message":{"id":"m1","model":"claude-x","usage":{"input_tokens":2,"output_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}"#;
    // misma requestId, streaming posterior con más output
    const A2: &str = r#"{"type":"assistant","requestId":"r1","timestamp":"2026-11-10T15:00:01Z","cwd":"/p/a","message":{"id":"m1","model":"claude-x","usage":{"input_tokens":2,"output_tokens":30,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}"#;
    const B: &str = r#"{"type":"assistant","requestId":"r2","timestamp":"2026-11-11T15:00:00Z","cwd":"/p/b","message":{"id":"m2","model":"claude-y","usage":{"input_tokens":1,"output_tokens":5}}}"#;
    const BAD: &str = r#"{"type":"assistant","usage": {{{"#;

    #[test]
    fn parsea_dedup_e_invalidas() {
        let mut s = Scan::default();
        for l in [A, BAD, A2, B, r#"{"type":"user","message":{"content":"hola"}}"#] {
            s.feed(l);
        }
        assert_eq!((s.invalid, s.duplicates, s.records.len()), (1, 1, 2));
        assert_eq!(s.records["r1"].usage.output_tokens, 30);
        let g = group_by(&s, NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(), |r| r.model.clone());
        assert_eq!(g[0].0, "claude-x");
        assert_eq!(g[0].1.usage.total(), 2 + 30 + 100 + 1000);
        assert_eq!(g[1].1.usage.cache_read_input_tokens, 0);
    }
    #[test]
    fn sesion_de_archivo() {
        assert_eq!(session_of(Path::new("/x/proj/abc.jsonl")), ("abc".into(), false));
        assert_eq!(session_of(Path::new("/x/proj/abc/subagents/agent-1.jsonl")), ("abc".into(), true));
    }
}
