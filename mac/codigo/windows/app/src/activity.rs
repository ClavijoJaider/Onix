//! Qué está haciendo una sesión de Claude Code, según la última herramienta de su transcripción (solo lectura).
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Línea de actividad de la sesión `id`; se relee como mucho una vez por segundo.
pub fn of(id: &str) -> Option<String> {
    static CACHE: Mutex<Option<HashMap<String, (Instant, Option<String>)>>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((t, v)) = cache.get(id) { if t.elapsed() < Duration::from_secs(1) { return v.clone(); } }
    let v = tail(id).and_then(|t| last(&t));
    cache.insert(id.into(), (Instant::now(), v.clone()));
    v
}

/// Archivo de la transcripción de la sesión `id`.
pub fn path(id: &str) -> Option<std::path::PathBuf> {
    let projects = std::path::Path::new(&std::env::var_os("HOME")?).join(".claude/projects");
    std::fs::read_dir(projects).ok()?.flatten().map(|d| d.path().join(format!("{id}.jsonl"))).find(|p| p.is_file())
}

/// Últimos 64 KB de la transcripción.
fn tail(id: &str) -> Option<String> {
    let mut f = std::fs::File::open(path(id)?).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024))).ok()?;
    let mut b = vec![];
    f.read_to_end(&mut b).ok()?;
    Some(String::from_utf8_lossy(&b).into_owned())
}

/// Última herramienta usada desde el último mensaje escrito por el usuario.
fn last(text: &str) -> Option<String> {
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("user") if content.is_string() => return None,
            Some("assistant") => {
                if let Some(t) = content.as_array().and_then(|a| a.iter().rev().find(|c| c["type"] == "tool_use")) {
                    return Some(describe(t["name"].as_str()?, &t["input"]));
                }
            }
            _ => {}
        }
    }
    None
}

fn describe(name: &str, input: &serde_json::Value) -> String {
    let s = |k: &str| input[k].as_str().unwrap_or("").lines().next().unwrap_or("").to_string();
    let file = || s("file_path").rsplit('/').next().unwrap_or("").to_string();
    match name {
        "Read" => format!("Reading {}", file()),
        "Edit" | "NotebookEdit" => format!("Editing {}", file()),
        "Write" => format!("Writing {}", file()),
        "Grep" | "Glob" => format!("Searching {}", s("pattern")),
        "WebSearch" => "Searching the web".into(),
        "Bash" | "Agent" => { let d = s("description"); if d.is_empty() { "Running a command".into() } else { d } }
        n => format!("Using {n}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ultima_herramienta_del_turno() {
        let user = r#"{"type":"user","message":{"content":"hola"}}"#;
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/b.rs"}}]}}"#;
        assert_eq!(super::last(&format!("{user}\n{tool}")).as_deref(), Some("Reading b.rs"));
        assert_eq!(super::last(&format!("{tool}\n{user}")), None);
    }
}
