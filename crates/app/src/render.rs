//! Pintado en CPU: cada frame es una lista de rects e imágenes; solo se repintan las franjas que cambian.
use crate::sidebar as sb;
use cosmic_text::{Buffer, CacheKey, Color, FontSystem, SwashCache, SwashContent};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use term_core::Rgb;

/// Alto en px de cada franja de repintado.
const BAND: i32 = 32;
/// Tope de glifos rasterizados en memoria; al superarlo se vacía.
const MAX_GLYPHS: usize = 8000;

#[derive(Clone, Copy)]
pub struct TextBounds { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }

/// Ícono dentro de un TextArea, en px relativos a su esquina; sin `color` toma el del área.
#[derive(Clone, Copy)]
pub struct CustomGlyph { pub id: u16, pub left: f32, pub top: f32, pub width: f32, pub height: f32, pub color: Option<Color> }

pub struct TextArea<'a> { pub buffer: &'a Buffer, pub left: f32, pub top: f32, pub bounds: TextBounds, pub default_color: Color, pub custom_glyphs: &'a [CustomGlyph] }

#[derive(Clone, Copy, Hash)]
enum Src { Fill, Glyph(CacheKey), Icon(u16) }

/// `r`: x, y, ancho, alto. `clip`: x0, y0, x1, y1.
#[derive(Clone, Copy, Hash)]
pub struct Op { r: [i32; 4], clip: [i32; 4], color: Rgb, src: Src }

#[derive(Default)]
pub struct Frame { pub ops: Vec<Op>, icons: HashMap<(u16, i32, i32), Option<Vec<u8>>> }

impl Frame {
    pub fn begin(&mut self, sc: &mut SwashCache) {
        self.ops.clear();
        if sc.image_cache.len() > MAX_GLYPHS { sc.image_cache.clear(); }
    }

    /// Rect en px; los bordes se redondean al píxel más cercano.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgb) {
        let (x0, y0, x1, y1) = (x.round() as i32, y.round() as i32, (x + w).round() as i32, (y + h).round() as i32);
        if x1 > x0 && y1 > y0 { self.ops.push(Op { r: [x0, y0, x1 - x0, y1 - y0], clip: [x0, y0, x1, y1], color, src: Src::Fill }); }
    }

    pub fn text<'a>(&mut self, areas: impl Iterator<Item = TextArea<'a>>, fs: &mut FontSystem, sc: &mut SwashCache) {
        for a in areas {
            let b = a.bounds;
            let clip = [b.left, b.top, b.right, b.bottom];
            let rgb = |c: Color| [c.r(), c.g(), c.b()];
            for g in a.custom_glyphs {
                let (w, h) = (g.width.round() as i32, g.height.round() as i32);
                if w <= 0 || h <= 0 { continue; }
                if self.icons.entry((g.id, w, h)).or_insert_with(|| sb::raster_icon(g.id, w as u16, h as u16)).is_none() { continue; }
                let r = [(a.left + g.left).round() as i32, (a.top + g.top).round() as i32, w, h];
                self.ops.push(Op { r, clip, color: rgb(g.color.unwrap_or(a.default_color)), src: Src::Icon(g.id) });
            }
            for run in a.buffer.layout_runs() {
                for g in run.glyphs {
                    let p = g.physical((a.left, a.top), 1.0);
                    let Some(img) = sc.get_image(fs, p.cache_key) else { continue };
                    let (w, h) = (img.placement.width as i32, img.placement.height as i32);
                    if w == 0 || h == 0 { continue; }
                    let r = [p.x + img.placement.left, run.line_y.round() as i32 + p.y - img.placement.top, w, h];
                    self.ops.push(Op { r, clip, color: rgb(g.color_opt.unwrap_or(a.default_color)), src: Src::Glyph(p.cache_key) });
                }
            }
        }
    }

    /// Huella de cada franja de una pantalla de alto `h`, y qué operaciones la tocan.
    pub fn bands(&self, h: usize) -> (Vec<u64>, Vec<Vec<u32>>) {
        let n = (h as i32 + BAND - 1) / BAND;
        let mut hs: Vec<_> = (0..n).map(|_| std::collections::hash_map::DefaultHasher::new()).collect();
        let mut idx = vec![vec![]; n as usize];
        for (i, op) in self.ops.iter().enumerate() {
            let (y0, y1) = (op.r[1].max(op.clip[1]).max(0), (op.r[1] + op.r[3]).min(op.clip[3]).min(h as i32));
            if y0 >= y1 { continue; }
            for b in y0 / BAND..=(y1 - 1) / BAND {
                op.hash(&mut hs[b as usize]);
                idx[b as usize].push(i as u32);
            }
        }
        (hs.iter().map(|h| h.finish()).collect(), idx)
    }

    /// Repinta la franja `band` sobre `px` (BGRA, `w` píxeles por fila).
    pub fn paint_band(&self, px: &mut [u32], w: usize, band: usize, ops: &[u32], bg: Rgb, fs: &mut FontSystem, sc: &mut SwashCache) {
        let h = px.len() / w;
        let (top, bottom) = (band as i32 * BAND, ((band as i32 + 1) * BAND).min(h as i32));
        px[top as usize * w..bottom as usize * w].fill(pixel(bg));
        for &i in ops {
            let op = &self.ops[i as usize];
            let [x, y, ow, oh] = op.r;
            let (x0, x1) = (x.max(op.clip[0]).max(0), (x + ow).min(op.clip[2]).min(w as i32));
            let (y0, y1) = (y.max(op.clip[1]).max(top), (y + oh).min(op.clip[3]).min(bottom));
            if x0 >= x1 || y0 >= y1 { continue; }
            let (mask, color): (&[u8], bool) = match op.src {
                Src::Fill => (&[], false),
                Src::Icon(id) => match self.icons.get(&(id, ow, oh)) { Some(Some(d)) => (d, false), _ => continue },
                Src::Glyph(k) => match sc.get_image(fs, k) {
                    Some(img) if img.content != SwashContent::SubpixelMask => (&img.data, img.content == SwashContent::Color),
                    _ => continue,
                },
            };
            for j in y0..y1 {
                let row = j as usize * w;
                let dst = &mut px[row + x0 as usize..row + x1 as usize];
                let k = ((j - y) * ow + (x0 - x)) as usize;
                if matches!(op.src, Src::Fill) {
                    dst.fill(pixel(op.color));
                } else if color {
                    for (d, p) in dst.iter_mut().zip(mask[k * 4..].chunks_exact(4)) { *d = blend(*d, [p[0], p[1], p[2]], p[3]); }
                } else {
                    for (d, &a) in dst.iter_mut().zip(&mask[k..]) {
                        if a == 255 { *d = pixel(op.color) } else if a != 0 { *d = blend(*d, op.color, a) }
                    }
                }
            }
        }
    }
}

/// BGRA opaco.
fn pixel(c: Rgb) -> u32 { 0xff00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32 }

/// Mezcla `fg` sobre el píxel `dst` con cobertura `a`.
fn blend(dst: u32, fg: Rgb, a: u8) -> u32 {
    let a = a as u32;
    let mix = |s: u32, f: u8| (s * (255 - a) + f as u32 * a + 127) / 255;
    0xff00_0000 | mix(dst >> 16 & 255, fg[0]) << 16 | mix(dst >> 8 & 255, fg[1]) << 8 | mix(dst & 255, fg[2])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn solo_cambia_la_franja_tocada_y_se_recorta() {
        let (mut fs, mut sc) = (FontSystem::new_with_locale_and_db("en-US".into(), cosmic_text::fontdb::Database::new()), SwashCache::new());
        let (w, h) = (4, 70);
        let mut f = Frame::default();
        f.rect(0.0, 0.0, 4.0, 2.0, [1, 1, 1]);
        let a = f.bands(h).0;
        f.rect(-5.0, 40.0, 100.0, 100.0, [0, 0, 2]);
        let (b, idx) = f.bands(h);
        assert_eq!(a.len(), 3);
        assert!(a[0] == b[0] && a[1] != b[1] && a[2] != b[2]);
        let mut px = vec![7; w * h];
        for i in 0..b.len() { f.paint_band(&mut px, w, i, &idx[i], [0, 0, 0], &mut fs, &mut sc); }
        assert_eq!((px[0], px[2 * w], px[39 * w], px[40 * w], px[h * w - 1]), (0xff010101, 0xff000000, 0xff000000, 0xff000002, 0xff000002));
        assert_eq!(blend(0, [255, 0, 0], 128), 0xff800000);
    }
}
