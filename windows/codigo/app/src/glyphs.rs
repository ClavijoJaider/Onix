//! Glifos dibujados a mano en la celda (cajas, bloques, braille y símbolos de Claude Code).
//! Devuelven rects [x, y, w, h, alfa] en px relativos a la celda; el llamador redondea y mezcla el alfa con el fondo.

pub type Rc = [f32; 5];

/// Brazos [izq, der, arriba, abajo] de U+2500..257F: 0 nada, 1 fino, 2 grueso, 3 doble. Las discontinuas salen continuas.
const BOX: [&str; 128] = [
    "1100", "2200", "0011", "0022", "1100", "2200", "0011", "0022", "1100", "2200", "0011", "0022", "0101", "0201", "0102", "0202",
    "1001", "2001", "1002", "2002", "0110", "0210", "0120", "0220", "1010", "2010", "1020", "2020", "0111", "0211", "0121", "0112",
    "0122", "0221", "0212", "0222", "1011", "2011", "1021", "1012", "1022", "2021", "2012", "2022", "1101", "2101", "1201", "2201",
    "1102", "2102", "1202", "2202", "1110", "2110", "1210", "2210", "1120", "2120", "1220", "2220", "1111", "2111", "1211", "2211",
    "1121", "1112", "1122", "2121", "1221", "2112", "1212", "2221", "2212", "2122", "1222", "2222", "1100", "2200", "0011", "0022",
    "3300", "0033", "0301", "0103", "0303", "3001", "1003", "3003", "0310", "0130", "0330", "3010", "1030", "3030", "0311", "0133",
    "0333", "3011", "1033", "3033", "3301", "1103", "3303", "3310", "1130", "3330", "3311", "1133", "3333", "0101", "1001", "1010",
    "0110", "0000", "0000", "0000", "1000", "0010", "0100", "0001", "2000", "0020", "0200", "0002", "1200", "0012", "2100", "0021",
];

/// Cuadrantes de U+2596..259F: bits 1 sup-izq, 2 sup-der, 4 inf-izq, 8 inf-der.
const QUAD: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];

/// Rects del glifo `c` en una celda de `cw` x `ch` px, o None si lo dibuja la fuente.
pub fn cell_rects(c: char, cw: f32, ch: f32) -> Option<Vec<Rc>> {
    let u = c as u32;
    let r = |x: f32, y: f32, w: f32, h: f32| [x, y, w, h, 1.0];
    let lt = (cw / 8.0).round().max(1.0);
    Some(match u {
        0x2500..=0x257F => return lines(BOX[(u - 0x2500) as usize].as_bytes(), cw, ch, lt),
        0x2580 => vec![r(0.0, 0.0, cw, ch / 2.0)],
        0x2581..=0x2588 => { let h = ch * (u - 0x2580) as f32 / 8.0; vec![r(0.0, ch - h, cw, h)] }
        0x2589..=0x258F => vec![r(0.0, 0.0, cw * (0x2590 - u) as f32 / 8.0, ch)],
        0x2590 => vec![r(cw / 2.0, 0.0, cw / 2.0, ch)],
        0x2591..=0x2593 => vec![[0.0, 0.0, cw, ch, (u - 0x2590) as f32 * 0.25]],
        0x2594 => vec![r(0.0, 0.0, cw, ch / 8.0)],
        0x2595 => vec![r(cw * 7.0 / 8.0, 0.0, cw / 8.0, ch)],
        0x2596..=0x259F => (0..4).filter(|b| QUAD[(u - 0x2596) as usize] >> b & 1 == 1)
            .map(|b| r((b & 1) as f32 * cw / 2.0, (b >> 1) as f32 * ch / 2.0, cw / 2.0, ch / 2.0)).collect(),
        0x2800..=0x28FF => {
            let d = (cw / 5.0).round().max(1.0);
            (0..8).filter(|b| (u - 0x2800) >> b & 1 == 1).map(|b| {
                let (col, row) = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)][b];
                r(cw * (0.3 + 0.4 * col as f32) - d / 2.0, ch * (0.125 + 0.25 * row as f32) - d / 2.0, d, d)
            }).collect()
        }
        // ⏺ círculo lleno, por filas de 1 px.
        0x23FA => { let (rad, cy) = (cw.min(ch) * 0.4, ch / 2.0); scan(cy - rad, cy + rad, |dy| { let h = (rad * rad - dy * dy).sqrt(); (cw / 2.0 - h, 2.0 * h) }, cy) }
        // ⏵ triángulo a la derecha.
        0x23F5 => { let (hh, w, cy) = (cw * 0.45, cw * 0.65, ch / 2.0); scan(cy - hh, cy + hh, |dy| ((cw - w) / 2.0, w * (1.0 - dy.abs() / hh)), cy) }
        // ⎿ codo: vertical desde arriba hasta el centro y horizontal hacia la derecha.
        0x23BF => { let (x, y) = ((cw * 0.35).round(), (ch / 2.0).round()); vec![r(x, 0.0, lt, y + lt), r(x, y, cw * 0.85 - x, lt)] }
        // ✢✳✶✻✽ del indicador de trabajo de Claude: desde una fuente de respaldo miden distinto y la línea salta.
        0x2722 => star(4, 0.80, lt * 1.2, cw, ch),
        0x2731 => star(6, 0.80, lt * 1.2, cw, ch), // en lugar del '*' del indicador
        0x2733 => star(8, 0.85, lt, cw, ch),
        0x2736 => star(6, 0.90, lt * 1.5, cw, ch),
        0x273B => star(8, 0.95, lt * 1.2, cw, ch),
        0x273D => star(6, 1.0, lt * 1.7, cw, ch),
        _ => return None,
    })
}

/// Estrella de `n` brazos centrada en la celda; `k` es el largo (1 = todo el ancho) y `t` el grosor.
fn star(n: u32, k: f32, t: f32, cw: f32, ch: f32) -> Vec<Rc> {
    let (cx, cy, rad) = (cw / 2.0, ch / 2.0, cw.min(ch) * 0.5 * k);
    let arms: Vec<(f32, f32)> = (0..n).map(|i| (std::f32::consts::TAU * i as f32 / n as f32 - std::f32::consts::FRAC_PI_2).sin_cos()).collect();
    // Cobertura suavizada (en cuartos) para que no salga pixelada.
    let alpha = |x: f32, y: f32| {
        let d = arms.iter().map(|&(s, c)| { let a = (x * c + y * s).clamp(0.0, rad); (x - a * c).hypot(y - a * s) }).fold(f32::MAX, f32::min);
        ((t / 2.0 + 0.5 - d).clamp(0.0, 1.0) * 4.0).round() / 4.0
    };
    let mut v = vec![];
    for j in (cy - rad - t).floor().max(0.0) as i32..(cy + rad + t).ceil().min(ch) as i32 {
        let (mut from, mut cur) = (0, 0.0);
        for i in 0..=cw as i32 {
            let a = if i < cw as i32 { alpha(i as f32 + 0.5 - cx, j as f32 + 0.5 - cy) } else { 0.0 };
            if a != cur {
                if cur > 0.0 { v.push([from as f32, j as f32, (i - from) as f32, 1.0, cur]); }
                (from, cur) = (i, a);
            }
        }
    }
    v
}

/// Filas de 1 px entre `y0` e `y1`; `f(dy)` da (x, ancho) de la fila a distancia `dy` del centro `cy`.
fn scan(y0: f32, y1: f32, f: impl Fn(f32) -> (f32, f32), cy: f32) -> Vec<Rc> {
    (y0.floor() as i32..y1.ceil() as i32).filter_map(|j| {
        let (x, w) = f(j as f32 + 0.5 - cy);
        (w > 0.0 && w.is_finite()).then_some([x, j as f32, w, 1.0, 1.0])
    }).collect()
}

/// Líneas de caja: cada brazo llega al bloque central del eje perpendicular; en las dobles, el trazo
/// pegado a un brazo perpendicular se detiene en la esquina interior (así no se unen los dos trazos).
fn lines(w: &[u8], cw: f32, ch: f32, lt: f32) -> Option<Vec<Rc>> {
    let [l, r, u, d] = [0, 1, 2, 3].map(|i| w[i] - b'0');
    if l + r + u + d == 0 { return None; }
    // fino = lt, grueso = 2 lt, doble = dos trazos de lt con hueco lt (3 lt).
    let th = |k: u8| lt * k as f32;
    let (vext, hext) = (th(u).max(th(d)), th(l).max(th(r)));
    let (bl, hb0) = ((cw / 2.0 - vext / 2.0).round(), (ch / 2.0 - hext / 2.0).round());
    let (br, hb1) = (bl + vext, hb0 + hext);
    // (inicio, largo) de cada trazo del peso `k` dentro de un bloque [b0, b0 + ext].
    let strokes = |k: u8, b0: f32, ext: f32| -> Vec<(f32, f32)> {
        match k { 0 => vec![], 3 => vec![(b0, lt), (b0 + 2.0 * lt, lt)], _ => vec![(b0 + ((ext - th(k)) / 2.0).round(), th(k))] }
    };
    let mut v = vec![];
    for (i, (ys, len)) in strokes(l, hb0, hext).into_iter().enumerate() {
        let near = l == 3 && (i == 0 && u > 0 || i == 1 && d > 0);
        v.push([0.0, ys, if near { bl + lt } else { br }, len, 1.0]);
    }
    for (i, (ys, len)) in strokes(r, hb0, hext).into_iter().enumerate() {
        let near = r == 3 && (i == 0 && u > 0 || i == 1 && d > 0);
        let x = if near { br - lt } else { bl };
        v.push([x, ys, cw - x, len, 1.0]);
    }
    for (i, (xs, len)) in strokes(u, bl, vext).into_iter().enumerate() {
        let near = u == 3 && (i == 0 && l > 0 || i == 1 && r > 0);
        v.push([xs, 0.0, len, if near { hb0 + lt } else { hb1 }, 1.0]);
    }
    for (i, (xs, len)) in strokes(d, bl, vext).into_iter().enumerate() {
        let near = d == 3 && (i == 0 && l > 0 || i == 1 && r > 0);
        let y = if near { hb1 - lt } else { hb0 };
        v.push([xs, y, len, ch - y, 1.0]);
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    const CW: f32 = 16.0;
    const CH: f32 = 32.0;
    fn hit(rs: &[Rc], x: f32, y: f32) -> bool { rs.iter().any(|r| x >= r[0] && x < r[0] + r[2] && y >= r[1] && y < r[1] + r[3]) }
    #[test]
    fn cajas_dentro_de_la_celda_y_completas() {
        for u in 0x2500..=0x257F {
            let rs = cell_rects(char::from_u32(u).unwrap(), CW, CH);
            if (0x2571..=0x2573).contains(&u) { assert!(rs.is_none()); continue; }
            for r in rs.unwrap() { assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[0] + r[2] <= CW && r[1] + r[3] <= CH && r[2] > 0.0 && r[3] > 0.0, "{u:x} {r:?}"); }
        }
        let h = cell_rects('─', CW, CH).unwrap();
        assert!(hit(&h, 0.5, 16.0) && hit(&h, 15.5, 16.0) && !hit(&h, 8.0, 10.0));
        let c = cell_rects('┼', CW, CH).unwrap();
        assert!(hit(&c, 0.5, 16.0) && hit(&c, 15.5, 16.0) && hit(&c, 8.0, 0.5) && hit(&c, 8.0, 31.5));
    }
    #[test]
    fn esquina_doble_no_une_los_trazos() {
        // lt = 2: trazos en x 5..7 y 9..11, y 13..15 y 17..19; el hueco central queda vacío.
        let r = cell_rects('╔', CW, CH).unwrap();
        assert!(hit(&r, 15.5, 14.5) && hit(&r, 15.5, 18.5) && !hit(&r, 15.5, 16.5));
        assert!(!hit(&r, 8.5, 20.5) && hit(&r, 6.5, 20.5) && hit(&r, 10.5, 20.5));
    }
    #[test]
    fn bloques_y_braille() {
        assert_eq!(cell_rects('█', CW, CH).unwrap(), vec![[0.0, 0.0, CW, CH, 1.0]]);
        assert_eq!(cell_rects('▀', CW, CH).unwrap(), vec![[0.0, 0.0, CW, 16.0, 1.0]]);
        assert_eq!(cell_rects('▒', CW, CH).unwrap()[0][4], 0.5);
        assert_eq!(cell_rects('▙', CW, CH).unwrap().len(), 3);
        assert_eq!(cell_rects('⣿', CW, CH).unwrap().len(), 8);
        assert!(cell_rects('\u{2800}', CW, CH).unwrap().is_empty());
        assert!(cell_rects('a', CW, CH).is_none());
        for c in ['⏺', '⏵', '⎿'] { assert!(!cell_rects(c, CW, CH).unwrap().is_empty()); }
        for c in ['✢', '✱', '✳', '✶', '✻', '✽'] {
            let rs = cell_rects(c, CW, CH).unwrap();
            assert!(!rs.is_empty() && rs.iter().all(|r| r[0] >= 0.0 && r[0] + r[2] <= CW && r[1] >= 0.0 && r[1] + r[3] <= CH), "{c}");
        }
    }
}
