//! Onix encuentra los agentes aunque su carpeta no esté en el PATH.
#[test]
fn agrega_las_carpetas_sin_repetir() {
    let home = std::env::temp_dir().join(format!("onix-carpetas-{}", std::process::id()));
    let (nativa, propia) = (home.join(".local").join("bin"), home.join("mi-npm"));
    for d in [&nativa, &propia] { std::fs::create_dir_all(d).unwrap(); }
    std::fs::write(home.join(".npmrc"), format!("prefix={}\n", propia.display())).unwrap();
    let antes = std::env::join_paths([&nativa]).unwrap();
    let despues = term_core::with_user_bins(&antes, &home).unwrap();
    let v: Vec<_> = std::env::split_paths(&despues).collect();
    assert_eq!(v[0], nativa);
    assert!(v.iter().filter(|d| **d == nativa).count() == 1 && v.contains(&propia));
    // Aún sin crear: ya va en el PATH.
    assert!(v.contains(&home.join(".bun").join("bin")));
    let _ = std::fs::remove_dir_all(home);
}
