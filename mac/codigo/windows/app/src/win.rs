//! Lo propio de Windows: procesos (sysinfo), portapapeles, diálogos y vistas previas.
use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use term_core::Session;

thread_local!(static SYS: RefCell<(System, Option<Instant>)> = RefCell::new((System::new(), None)));

pub fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().with_memory().with_cpu().with_exe(UpdateKind::OnlyIfNotSet).with_cmd(UpdateKind::OnlyIfNotSet)
        .with_cwd(UpdateKind::Always).with_user(UpdateKind::OnlyIfNotSet)
}

/// Lista de procesos, releída como mucho una vez por segundo.
fn with<R>(f: impl FnOnce(&System) -> R) -> R {
    SYS.with_borrow_mut(|(s, at)| {
        if at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            s.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh_kind());
            *at = Some(Instant::now());
        }
        f(s)
    })
}

pub fn slashes(s: &str) -> String { s.replace('\\', "/") }

/// Que los programas auxiliares no abran una consola.
pub fn hidden(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(c, 0x0800_0000);
    c.stdin(Stdio::null()).stderr(Stdio::null())
}

pub fn powershell(script: &str) -> Command {
    let mut c = Command::new("powershell");
    c.args(["-NoLogo", "-NoProfile", "-STA", "-Command", script]);
    hidden(&mut c);
    c
}

/// (nombre sin .exe, ruta, cwd) con `/`; vacíos si el proceso no existe.
pub fn proc_info(pid: i32) -> (String, String, String) {
    with(|s| s.process(Pid::from_u32(pid as u32)).map(|p| {
        let name = p.name().to_string_lossy().into_owned();
        let path = |x: Option<&std::path::Path>| x.map(|x| slashes(&x.to_string_lossy())).unwrap_or_default();
        let cwd = path(p.cwd());
        let cwd = if cwd.len() > 3 { cwd.trim_end_matches('/').to_string() } else { cwd };
        (name.strip_suffix(".exe").unwrap_or(&name).to_string(), path(p.exe()), cwd)
    }).unwrap_or_default())
}

pub fn proc_args(pid: i32) -> Vec<String> {
    with(|s| s.process(Pid::from_u32(pid as u32)).map(|p| p.cmd().iter().map(|a| slashes(&a.to_string_lossy())).collect()).unwrap_or_default())
}

/// Windows no tiene "proceso en primer plano": se toma el hijo más nuevo del shell (y el suyo si es el `.cmd` de npm).
pub fn fg_pid(session: &Session) -> Option<i32> {
    let shell = session.child_pid?;
    with(|s| {
        let child = |parent: u32| s.processes().values()
            .filter(|p| p.parent() == Some(Pid::from_u32(parent)) && p.name() != "conhost.exe")
            .max_by_key(|p| (p.start_time(), p.pid().as_u32())).map(|p| (p.pid().as_u32(), p.name() == "cmd.exe"));
        Some(match child(shell) {
            Some((c, true)) => child(c).map_or(c, |g| g.0),
            Some((c, false)) => c,
            None => shell,
        } as i32)
    })
}

/// Líneas "ppid nombre" de todos los procesos.
pub fn ps_list() -> String {
    with(|s| s.processes().values().filter(|p| p.name() != "conhost.exe")
        .filter_map(|p| Some(format!("{} {}\n", p.parent()?.as_u32(), p.name().to_string_lossy()))).collect())
}

/// Como kill(2): con `sig` 0 solo comprueba que exista; si no, lo termina.
pub fn kill(pid: i32, sig: i32) -> i32 {
    with(|s| match s.process(Pid::from_u32(pid as u32)) { Some(p) if sig == 0 || p.kill() => 0, _ => -1 })
}
/// Usuario ficticio: `procs` marca así los procesos propios.
pub const OWN_UID: u32 = 1000;
pub fn parent_of(pid: u32) -> i32 { with(|s| s.process(Pid::from_u32(pid)).and_then(|p| p.parent()).map_or(0, |p| p.as_u32() as i32)) }

pub fn copy(text: &str) { let _ = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)); }
pub fn paste() -> String { arboard::Clipboard::new().and_then(|mut c| c.get_text()).map(|t| t.replace("\r\n", "\n")).unwrap_or_default() }

/// Muestra el archivo en el Explorador, o lo abre con su programa.
pub fn open(path: &std::path::Path, launch: bool) {
    let p = path.to_string_lossy().replace('/', "\\");
    let mut c = Command::new("explorer");
    if launch { c.arg(p); } else { c.arg("/select,").arg(p); }
    let _ = c.spawn();
}

/// Abre una dirección en el navegador.
pub fn browse(url: &str) { let _ = Command::new("explorer").arg(url).spawn(); }

/// Ventana flotante con la imagen junto al ratón, sin quitar el foco. La ruta llega por variable de entorno.
pub const PEEK: &str = r#"Start-Sleep -Milliseconds 350
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type 'using System; using System.Runtime.InteropServices; public class W { [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c); [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr a, int x, int y, int w, int t, uint f); }'
$img = [System.Drawing.Image]::FromFile($env:ONIX_PEEK)
$k = [Math]::Min(1.0, 360 / [Math]::Max($img.Width, $img.Height)); $w = [int]($img.Width * $k) + 16; $h = [int]($img.Height * $k) + 16
$m = [System.Windows.Forms.Cursor]::Position; $a = [System.Windows.Forms.Screen]::FromPoint($m).WorkingArea
$x = [Math]::Max($a.Left, [Math]::Min($m.X + 16, $a.Right - $w)); $y = if ($m.Y + 16 + $h -le $a.Bottom) { $m.Y + 16 } else { [Math]::Max($a.Top, $m.Y - 16 - $h) }
$f = New-Object System.Windows.Forms.Form
$f.FormBorderStyle = 'None'; $f.ShowInTaskbar = $false; $f.StartPosition = 'Manual'; $f.BackColor = [System.Drawing.Color]::FromArgb(22, 22, 26); $f.Padding = 8
$f.Bounds = New-Object System.Drawing.Rectangle $x, $y, $w, $h
$b = New-Object System.Windows.Forms.PictureBox; $b.Dock = 'Fill'; $b.SizeMode = 'Zoom'; $b.Image = $img; $f.Controls.Add($b)
$null = [W]::ShowWindow($f.Handle, 4); $null = [W]::SetWindowPos($f.Handle, [IntPtr](-1), $x, $y, $w, $h, 0x10)
$fin = (Get-Date).AddSeconds(60); while ((Get-Date) -lt $fin) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 30 }"#;
