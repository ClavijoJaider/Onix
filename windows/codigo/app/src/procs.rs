//! Panel "Processes": muestreo con `ps`/`lsof`/`sysctl`/`vm_stat` (solo con el panel abierto) y lógica pura (parseo, orden, filtro, can_stop).
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_ROWS: usize = 50;

#[derive(Clone, Debug, PartialEq)]
pub struct Proc { pub pid: i32, pub ppid: i32, pub uid: u32, pub cpu: f32, pub rss_kb: u64, pub name: String }

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sort { Cpu, Mem }

#[derive(Clone, Debug, PartialEq)]
pub struct Row { pub pid: i32, pub ppid: i32, pub uid: u32, pub name: String, pub parent: Option<String>, pub cpu: f32, pub rss_kb: u64, pub port: Option<u16> }

/// `ps -axo pid,ppid,uid,pcpu,rss,comm`: comm es una ruta (puede tener espacios); se queda el nombre base.
pub fn parse_ps(out: &str) -> Vec<Proc> {
    out.lines().skip(1).filter_map(|l| {
        let mut rest = l.trim();
        let mut f = [""; 5];
        for x in &mut f { let (a, b) = rest.split_once(char::is_whitespace)?; *x = a; rest = b.trim_start(); }
        let name = rest.rsplit('/').next().filter(|n| !n.is_empty())?.to_string();
        Some(Proc { pid: f[0].parse().ok()?, ppid: f[1].parse().ok()?, uid: f[2].parse().ok()?, cpu: f[3].parse().ok()?, rss_kb: f[4].parse().ok()?, name })
    }).collect()
}

/// `lsof -iTCP -sTCP:LISTEN -nP`: pid -> primer puerto en escucha.
pub fn parse_lsof(out: &str) -> HashMap<i32, u16> {
    let mut m = HashMap::new();
    for l in out.lines().skip(1) {
        let t: Vec<&str> = l.split_whitespace().collect();
        let (Some(pid), Some(i)) = (t.get(1).and_then(|p| p.parse::<i32>().ok()), t.iter().position(|x| *x == "(LISTEN)")) else { continue };
        if let Some(p) = i.checked_sub(1).and_then(|i| t[i].rsplit(':').next()).and_then(|p| p.parse().ok()) { m.entry(pid).or_insert(p); }
    }
    m
}

/// Todos los puertos en escucha de la misma salida: (puerto, pid), sin repetir IPv4/IPv6 y ordenados.
pub fn parse_listen(out: &str) -> Vec<(u16, i32)> {
    let mut v: Vec<(u16, i32)> = out.lines().skip(1).filter_map(|l| {
        let t: Vec<&str> = l.split_whitespace().collect();
        let i = t.iter().position(|x| *x == "(LISTEN)")?;
        Some((t.get(i.checked_sub(1)?)?.rsplit(':').next()?.parse().ok()?, t.get(1)?.parse().ok()?))
    }).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Filas del panel Ports: una por puerto, filtradas por número, proceso o pid.
pub fn port_rows(ps: &[Proc], listen: &[(u16, i32)], q: &str) -> Vec<Row> {
    let all = rows_of(ps);
    let q = q.to_lowercase();
    listen.iter().filter_map(|&(port, pid)| all.get(&pid).map(|r| Row { port: Some(port), ..r.clone() }))
        .filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q) || r.pid.to_string() == q || r.port.is_some_and(|p| p.to_string().starts_with(&q)))
        .take(MAX_ROWS).collect()
}

fn rows_of(ps: &[Proc]) -> HashMap<i32, Row> {
    let names: HashMap<i32, &str> = ps.iter().map(|p| (p.pid, p.name.as_str())).collect();
    ps.iter().map(|p| (p.pid, Row { pid: p.pid, ppid: p.ppid, uid: p.uid, name: p.name.clone(), cpu: p.cpu, rss_kb: p.rss_kb, port: None,
        parent: names.get(&p.ppid).filter(|n| p.ppid > 1 && **n != p.name).map(|n| n.to_string()) })).collect()
}

/// Bytes usados según `vm_stat` (active + wired + compressor); el tamaño de página sale de la primera línea.
pub fn parse_vm_stat(out: &str) -> Option<u64> {
    let page: u64 = out.lines().next()?.split("page size of ").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let get = |k: &str| out.lines().find_map(|l| l.strip_prefix(k)?.trim_start_matches(':').trim().trim_end_matches('.').parse::<u64>().ok());
    Some((get("Pages active")? + get("Pages wired down")? + get("Pages occupied by compressor")?) * page)
}

extern "C" {
    fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut u64) -> i32;
    fn mach_timebase_info(info: *mut [u32; 2]) -> i32;
}

/// % de un núcleo gastado entre dos lecturas separadas `secs`; `tick_ns`: nanosegundos por unidad mach.
fn cpu_pct(before: u64, now: u64, tick_ns: f64, secs: f64) -> f32 { (now.saturating_sub(before) as f64 * tick_ns / (secs * 1e9) * 100.0) as f32 }

/// Total de CPU del sistema: suma de pcpu / núcleos, tope 100.
pub fn sys_cpu(ps: &[Proc], ncpu: u32) -> f32 { (ps.iter().map(|p| p.cpu).sum::<f32>() / ncpu.max(1) as f32).min(100.0) }

/// Filas ordenadas y filtradas (nombre, padre, pid o puerto contienen `q`), con el padre si existe, no es launchd y es otro nombre.
pub fn rows(ps: &[Proc], ports: &HashMap<i32, u16>, sort: Sort, q: &str) -> Vec<Row> {
    let names: HashMap<i32, &str> = ps.iter().map(|p| (p.pid, p.name.as_str())).collect();
    let q = q.to_lowercase();
    let mut v: Vec<Row> = ps.iter().map(|p| Row {
        pid: p.pid, ppid: p.ppid, uid: p.uid, name: p.name.clone(), cpu: p.cpu, rss_kb: p.rss_kb, port: ports.get(&p.pid).copied(),
        parent: names.get(&p.ppid).filter(|n| p.ppid > 1 && **n != p.name).map(|n| n.to_string()),
    }).filter(|r| q.is_empty() || r.name.to_lowercase().contains(&q) || r.parent.as_ref().is_some_and(|n| n.to_lowercase().contains(&q))
        || r.pid.to_string() == q || r.port.is_some_and(|p| p.to_string() == q)).collect();
    v.sort_by(|a, b| match sort {
        Sort::Cpu => b.cpu.total_cmp(&a.cpu).then(b.rss_kb.cmp(&a.rss_kb)),
        Sort::Mem => b.rss_kb.cmp(&a.rss_kb),
    }.then(a.pid.cmp(&b.pid)));
    v.truncate(MAX_ROWS);
    v
}

/// pid y sus ancestros (hasta pid 1), sin ciclos.
pub fn chain(ps: &[Proc], pid: i32) -> Vec<i32> {
    let pp: HashMap<i32, i32> = ps.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut v = vec![pid];
    while let Some(&p) = pp.get(v.last().unwrap()) { if p <= 1 || v.contains(&p) { break; } v.push(p); }
    v
}

/// Solo procesos propios (mismo uid, no root), nunca pid 1, la propia app ni su padre.
pub fn can_stop(uid: u32, my_uid: u32, pid: i32, self_pid: i32, self_ppid: i32) -> bool {
    uid == my_uid && uid != 0 && pid > 1 && pid != self_pid && pid != self_ppid
}

/// El pid sigue en el snapshot con el mismo nombre, ppid y uid (evita matar un pid reciclado).
pub fn same_proc(ps: &[Proc], pid: i32, name: &str, ppid: i32, uid: u32) -> bool {
    ps.iter().any(|p| p.pid == pid && p.name == name && p.ppid == ppid && p.uid == uid)
}

pub fn fmt_mem(kb: u64) -> String { if kb >= 1 << 20 { format!("{:.1} GB", kb as f64 / (1u64 << 20) as f64) } else { format!("{} MB", kb / 1024) } }

#[derive(Default, Clone)]
pub struct Snap { pub procs: Vec<Proc>, pub ports: HashMap<i32, u16>, pub listen: Vec<(u16, i32)>, pub cpu: f32, pub used: u64, pub total: u64 }

/// Hilo único de muestreo; solo trabaja mientras `on` esté activa.
#[derive(Clone)]
pub struct Mon { pub on: Arc<AtomicBool>, pub snap: Arc<Mutex<Snap>>, th: std::thread::Thread }

/// `netstat -ano`: (puerto en escucha, pid), ordenados y sin repetir (IPv4 e IPv6).
fn parse_netstat(text: &str) -> Vec<(u16, i32)> {
    let mut v: Vec<(u16, i32)> = text.lines().filter_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // El estado sale traducido: escucha = la dirección remota es 0.0.0.0:0 o [::]:0.
        if f.len() != 5 || f[0] != "TCP" || !f[2].ends_with(":0") { return None; }
        Some((f[1].rsplit(':').next()?.parse().ok()?, f[4].parse().ok()?))
    }).collect();
    v.sort_unstable();
    v.dedup();
    v
}

impl Mon {
    pub fn start() -> Mon {
        let (on, snap) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(Snap::default())));
        let (on2, snap2) = (on.clone(), snap.clone());
        let h = std::thread::spawn(move || {
            let mut sys = sysinfo::System::new();
            let ncpu = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
            let mut last_net: Option<Instant> = None;
            loop {
                if !on2.load(Ordering::Relaxed) { last_net = None; std::thread::park(); continue; }
                std::thread::sleep(Duration::from_secs(2));
                sys.refresh_memory();
                sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, crate::win::refresh_kind());
                let me = sysinfo::Pid::from_u32(std::process::id());
                let user = sys.process(me).and_then(|p| p.user_id().cloned());
                let p: Vec<Proc> = sys.processes().values().map(|x| Proc {
                    pid: x.pid().as_u32() as i32, ppid: x.parent().map_or(0, |p| p.as_u32() as i32),
                    uid: if x.user_id() == user.as_ref() && user.is_some() { crate::win::OWN_UID } else { 0 },
                    cpu: x.cpu_usage(), rss_kb: x.memory() / 1024,
                    name: x.name().to_string_lossy().trim_end_matches(".exe").to_string(),
                }).collect();
                {
                    let mut s = snap2.lock().unwrap();
                    (s.cpu, s.used, s.total, s.procs) = (sys_cpu(&p, ncpu), sys.used_memory(), sys.total_memory(), p);
                }
                if last_net.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
                    last_net = Some(Instant::now());
                    let mut c = std::process::Command::new("netstat");
                    c.args(["-ano", "-p", "TCP"]);
                    if let Ok(o) = crate::win::hidden(&mut c).output() {
                        let listen = parse_netstat(&String::from_utf8_lossy(&o.stdout));
                        let mut s = snap2.lock().unwrap();
                        // `listen` va ordenada: al recorrerla al revés queda el puerto más bajo de cada pid.
                        (s.ports, s.listen) = (listen.iter().rev().map(|&(port, pid)| (pid, port)).collect(), listen);
                    }
                }
            }
        });
        Mon { on, snap, th: h.thread().clone() }
    }
    pub fn set(&self, on: bool) { self.on.store(on, Ordering::Relaxed); if on { self.th.unpark(); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PS: &str = "  PID  PPID   UID  %CPU    RSS COMM\n    1     0     0   0.0   1000 /sbin/launchd\n  100     1   501  60.5 2097152 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n  101   100   501   5.0 204800 /Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Helper\n  200     1   501   1.0 512000 /usr/bin/node\n  300     1     0  90.0   4096 /usr/sbin/bluetoothd\n basura\n";
    const LSOF: &str = "COMMAND   PID USER   FD   TYPE DEVICE SIZE/OFF NODE NAME\nnode      200 me    23u  IPv4 0xabc      0t0  TCP *:9222 (LISTEN)\nnode      200 me    24u  IPv6 0xabd      0t0  TCP [::1]:3000 (LISTEN)\nrapportd  612 me     8u  IPv6 0xabe      0t0  TCP *:49152 (LISTEN)\n";
    fn ps() -> Vec<Proc> { parse_ps(PS) }
    #[test]
    fn parsea_ps_y_lsof() {
        let p = ps();
        assert_eq!(p.len(), 5);
        assert_eq!(p[1], Proc { pid: 100, ppid: 1, uid: 501, cpu: 60.5, rss_kb: 2097152, name: "Google Chrome".into() });
        assert_eq!(p[2].name, "Google Chrome Helper");
        let l = parse_lsof(LSOF);
        assert_eq!((l[&200], l[&612], l.len()), (9222, 49152, 2));
        assert!(parse_lsof("").is_empty());
    }
    #[test]
    fn netstat_da_los_puertos_en_escucha() {
        let t = "  Proto  Local  Remote  State  PID\n  TCP    0.0.0.0:5173   0.0.0.0:0   LISTENING   40\n  TCP    [::]:5173   [::]:0   LISTENING   40\n  TCP    127.0.0.1:3000   0.0.0.0:0   ESCUCHANDO   40\n  TCP    10.0.0.2:51000   1.2.3.4:443   ESTABLISHED   9\n";
        assert_eq!(parse_netstat(t), [(3000, 40), (5173, 40)]);
    }
    #[test]
    fn panel_de_puertos() {
        let l = parse_listen(LSOF);
        assert_eq!(l, [(3000, 200), (9222, 200), (49152, 612)]);
        let r = port_rows(&ps(), &l, "");
        // 612 no está en la lista de procesos: no se muestra.
        assert_eq!(r.iter().map(|r| (r.port.unwrap(), r.name.as_str())).collect::<Vec<_>>(), [(3000, "node"), (9222, "node")]);
        assert_eq!(port_rows(&ps(), &l, "92").len(), 1);
        assert!(port_rows(&ps(), &l, "chrome").is_empty());
    }
    #[test]
    fn vm_stat_y_cpu() {
        let v = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               10.\nPages active:                            100.\nPages wired down:                         50.\nPages occupied by compressor:             25.\n";
        assert_eq!(parse_vm_stat(v), Some(175 * 16384));
        assert_eq!(parse_vm_stat("nada"), None);
        assert_eq!(sys_cpu(&ps(), 10), (60.5 + 5.0 + 1.0 + 90.0) / 10.0);
        assert_eq!(sys_cpu(&ps(), 1), 100.0);
    }
    #[test]
    fn orden_filtro_y_padre() {
        let p = ps();
        let ports = parse_lsof(LSOF);
        let r = rows(&p, &ports, Sort::Cpu, "");
        assert_eq!(r.iter().map(|x| x.pid).collect::<Vec<_>>(), [300, 100, 101, 200, 1]);
        assert_eq!(rows(&p, &ports, Sort::Mem, "")[0].pid, 100);
        let h = r.iter().find(|x| x.pid == 101).unwrap();
        assert_eq!(h.parent.as_deref(), Some("Google Chrome"));
        assert_eq!(r.iter().find(|x| x.pid == 100).unwrap().parent, None);
        assert_eq!(r.iter().find(|x| x.pid == 200).unwrap().port, Some(9222));
        assert_eq!(rows(&p, &ports, Sort::Cpu, "CHROME").len(), 2);
        assert_eq!(rows(&p, &ports, Sort::Cpu, "9222").len(), 1);
        assert!(rows(&p, &ports, Sort::Cpu, "zzz").is_empty());
        let muchos: Vec<Proc> = (2..200).map(|i| Proc { pid: i, ppid: 1, uid: 501, cpu: 0.0, rss_kb: 1, name: "x".into() }).collect();
        assert_eq!(rows(&muchos, &HashMap::new(), Sort::Cpu, "").len(), MAX_ROWS);
    }
    #[test]
    fn cadena_y_can_stop() {
        let p = ps();
        assert_eq!(chain(&p, 101), [101, 100]);
        assert_eq!(chain(&p, 999), [999]);
        assert!(can_stop(501, 501, 200, 10, 11));
        assert!(!can_stop(0, 501, 300, 10, 11), "root");
        assert!(!can_stop(502, 501, 200, 10, 11), "otro usuario");
        assert!(!can_stop(501, 501, 1, 10, 11), "pid 1");
        assert!(!can_stop(501, 501, 10, 10, 11), "la app");
        assert!(!can_stop(501, 501, 11, 10, 11), "su padre");
        assert!(!can_stop(0, 0, 200, 10, 11), "root nunca");
        assert!(same_proc(&p, 200, "node", 1, 501));
        assert!(!same_proc(&p, 200, "python", 1, 501), "nombre cambió");
        assert!(!same_proc(&p, 200, "node", 2, 501), "ppid cambió");
        assert!(!same_proc(&p, 999, "node", 1, 501), "ya no existe");
        assert_eq!(fmt_mem(204800), "200 MB");
        assert_eq!(fmt_mem(2097152), "2.0 GB");
    }
}
