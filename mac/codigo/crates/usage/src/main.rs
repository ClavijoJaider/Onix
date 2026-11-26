use chrono::{Duration, Local};
use std::path::PathBuf;
use usage::{group_by, Agg, Groups, Scan};

fn n(v: u64) -> String {
    let s = v.to_string();
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            o.push('.');
        }
        o.push(c);
    }
    o
}

fn table(title: &str, g: &Groups, limit: usize, w: usize) {
    println!("\n{title}");
    println!("{:<w$} {:>8} {:>14} {:>14} {:>16} {:>16}", "", "turnos", "input", "output", "cache_creac", "cache_lect");
    for (k, a) in g.iter().take(limit) {
        row(k, a, w);
    }
}

fn row(k: &str, a: &Agg, w: usize) {
    let k: String = if k.chars().count() > w { format!("…{}", k.chars().skip(k.chars().count() - w + 1).collect::<String>()) } else { k.into() };
    let u = &a.usage;
    println!("{:<w$} {:>8} {:>14} {:>14} {:>16} {:>16}", k, n(a.turns), n(u.input_tokens), n(u.output_tokens), n(u.cache_creation_input_tokens), n(u.cache_read_input_tokens));
}

fn main() {
    let mut days = 30i64;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match (a.as_str(), it.next().and_then(|v| v.parse().ok())) {
            ("--days", Some(v)) if v > 0 => days = v,
            _ => return eprintln!("uso: usage [--days N]"),
        }
    }
    let root = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/projects");
    let mut s = Scan::default();
    s.scan_dir(&root);
    let since = Local::now().date_naive() - Duration::days(days - 1);

    let total = group_by(&s, since, |_| "TOTAL".into());
    let mut per_day = group_by(&s, since, |r| r.day.to_string());
    per_day.sort_by(|a, b| a.0.cmp(&b.0));
    println!("Uso de tokens, últimos {days} días (desde {since})");
    table("Total", &total, 1, 10);
    table("Por día", &per_day, usize::MAX, 10);
    table("Top modelos", &group_by(&s, since, |r| r.model.clone()), 8, 28);
    table("Top proyectos (cwd)", &group_by(&s, since, |r| r.project.clone()), 10, 40);
    println!("\nLíneas inválidas: {} | duplicadas descartadas: {}", n(s.invalid), n(s.duplicates));
}
