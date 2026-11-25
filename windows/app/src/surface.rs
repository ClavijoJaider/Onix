//! Presentación en Windows con softbuffer: copia propia de los píxeles, repintando solo las franjas que cambian.
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::window::Window;

pub struct Screen { surface: softbuffer::Surface<Arc<Window>, Arc<Window>>, px: Vec<u32>, bands: Vec<u64>, w: usize, h: usize }

impl Screen {
    pub fn new(window: &Arc<Window>) -> Self {
        let ctx = softbuffer::Context::new(window.clone()).expect("contexto");
        let surface = softbuffer::Surface::new(&ctx, window.clone()).expect("superficie");
        Self { surface, px: vec![], bands: vec![], w: 0, h: 0 }
    }
    pub fn resize(&mut self, w: usize, h: usize, _scale: f64) { (self.w, self.h, self.px, self.bands) = (w, h, vec![0; w * h], vec![]); }
    pub fn release_idle(&mut self) {}
    pub fn release_all(&mut self) {}
    pub fn present(&mut self, bands: &[u64], mut paint: impl FnMut(&mut [u32], usize, usize)) -> bool {
        let (Some(w), Some(h)) = (NonZeroU32::new(self.w as u32), NonZeroU32::new(self.h as u32)) else { return true };
        for (i, hash) in bands.iter().enumerate() {
            if self.bands.get(i) != Some(hash) { paint(&mut self.px, self.w, i); }
        }
        self.bands = bands.to_vec();
        // El tamaño es el mismo con el que se pintó: si no, al maximizar el índice se sale del búfer.
        if self.surface.resize(w, h).is_err() { return true; }
        let Ok(mut out) = self.surface.buffer_mut() else { return false };
        if out.len() == self.px.len() { out.copy_from_slice(&self.px); }
        out.present().is_ok()
    }
}
