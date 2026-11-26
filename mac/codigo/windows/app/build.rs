//! Mete el ícono de Onix (el mismo del Mac) dentro de onix.exe.
// shortcut: solo la cadena GNU (windres); para compilar con MSVC habría que usar rc.exe.
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/onix.rc");
    println!("cargo:rerun-if-changed=assets/onix.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu") || std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") { return; }
    let windres = if cfg!(windows) { "windres" } else { "x86_64-w64-mingw32-windres" };
    let out = format!("{}/onix-icono.o", std::env::var("OUT_DIR").unwrap());
    let ok = Command::new(windres).args(["-I", "assets", "assets/onix.rc", "-O", "coff", "-o", &out]).status().is_ok_and(|s| s.success());
    assert!(ok, "windres no pudo preparar el icono");
    println!("cargo:rustc-link-arg-bins={out}");
}
