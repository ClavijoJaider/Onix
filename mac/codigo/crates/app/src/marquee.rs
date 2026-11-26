//! Carrusel de títulos largos: desplazamiento horizontal ida y vuelta con pausas.

/// Desplazamiento (px) en el instante `t` (s) desde que empezó el hover.
/// Ciclo: pausa, avance a `speed` px/s, pausa, retorno; siempre en [0, overflow].
pub fn marquee_offset(t: f32, overflow: f32, speed: f32, pause: f32) -> f32 {
    if overflow <= 0.0 || speed <= 0.0 || t <= 0.0 { return 0.0; }
    let travel = overflow / speed;
    let c = t % (2.0 * (pause + travel));
    let off = if c < pause { 0.0 }
        else if c < pause + travel { (c - pause) * speed }
        else if c < 2.0 * pause + travel { overflow }
        else { overflow - (c - 2.0 * pause - travel) * speed };
    off.clamp(0.0, overflow)
}

/// Frames braille del spinner de "trabajando".
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Índice de frame (0..n) en el instante `t` (s) a `fps` cuadros por segundo.
pub fn spinner_frame(t: f32, fps: f32, n: usize) -> usize {
    if n == 0 || t <= 0.0 || fps <= 0.0 { return 0; }
    (t * fps) as usize % n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pausa_inicial_es_cero() {
        assert_eq!(marquee_offset(0.0, 100.0, 35.0, 1.0), 0.0);
        assert_eq!(marquee_offset(0.99, 100.0, 35.0, 1.0), 0.0);
    }
    #[test]
    fn pausa_final_es_overflow() {
        let travel = 100.0 / 35.0;
        assert_eq!(marquee_offset(1.0 + travel + 0.5, 100.0, 35.0, 1.0), 100.0);
    }
    #[test]
    fn avanza_a_la_velocidad() {
        assert!((marquee_offset(2.0, 100.0, 35.0, 1.0) - 35.0).abs() < 1e-4);
    }
    #[test]
    fn nunca_fuera_de_rango() {
        for i in 0..5000 {
            let o = marquee_offset(i as f32 * 0.013, 80.0, 35.0, 1.0);
            assert!((0.0..=80.0).contains(&o), "{o}");
        }
    }
    #[test]
    fn sin_overflow_es_cero() {
        assert_eq!(marquee_offset(5.0, 0.0, 35.0, 1.0), 0.0);
        assert_eq!(marquee_offset(5.0, -3.0, 35.0, 1.0), 0.0);
    }
    #[test]
    fn spinner_avanza_y_da_la_vuelta() {
        assert_eq!(spinner_frame(0.0, 10.0, 10), 0);
        assert_eq!(spinner_frame(0.15, 10.0, 10), 1);
        assert_eq!(spinner_frame(1.0, 10.0, 10), 0);
        assert_eq!(spinner_frame(0.35, 10.0, 4), 3);
        assert_eq!(spinner_frame(0.5, 20.0, 10), 0);
        assert_eq!(spinner_frame(0.5, 0.0, 10), 0);
        assert_eq!(spinner_frame(0.5, 10.0, 0), 0);
    }
}
