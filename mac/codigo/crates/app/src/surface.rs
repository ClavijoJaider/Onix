//! Dos IOSurface alternadas como contenido de una CALayer: sin copias ni conversión de color en el proceso.
//! En reposo se suelta la que no se ve; con la ventana tapada, las dos.
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSMutableDictionary, NSNumber, NSString};
use objc2_quartz_core::{kCAGravityTopLeft, CALayer, CATransaction};
use std::ffi::c_void;
use std::time::Instant;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

#[link(name = "IOSurface", kind = "framework")]
extern "C" {}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *const c_void;
    fn CGColorSpaceCopyPropertyList(space: *const c_void) -> *const AnyObject;
    fn CGColorSpaceRelease(space: *const c_void);
}

/// Una superficie y la huella de lo que tiene pintado en cada franja.
struct Buf { s: Retained<AnyObject>, bands: Vec<u64> }

pub struct Screen { layer: Retained<CALayer>, bufs: [Option<Buf>; 2], w: usize, h: usize, shown: usize, changed: Instant }

impl Screen {
    pub fn new(window: &Window) -> Self {
        let RawWindowHandle::AppKit(h) = window.window_handle().unwrap().as_raw() else { panic!("solo macOS") };
        // SAFETY: puntero a un NSView vivo mientras viva la ventana; se usa en el hilo principal.
        let layer = unsafe {
            let view: &AnyObject = h.ns_view.cast().as_ref();
            let _: () = msg_send![view, setWantsLayer: Bool::YES];
            let root: Retained<CALayer> = msg_send![view, layer];
            let layer = CALayer::new();
            layer.setOpaque(true);
            layer.setAnchorPoint(Default::default());
            layer.setGeometryFlipped(true);
            // Al redimensionar, lo anterior queda arriba a la izquierda sin estirarse.
            layer.setContentsGravity(kCAGravityTopLeft);
            root.addSublayer(&layer);
            layer
        };
        Self { layer, bufs: [None, None], w: 0, h: 0, shown: 0, changed: Instant::now() }
    }

    pub fn resize(&mut self, w: usize, h: usize, scale: f64) {
        (self.w, self.h, self.bufs) = (w, h, [None, None]);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: la capa raíz existe; su `bounds` es un CGRect.
        unsafe {
            let root: Retained<CALayer> = msg_send![&*self.layer, superlayer];
            self.layer.setFrame(root.bounds());
        }
        self.layer.setContentsScale(scale);
        CATransaction::commit();
    }

    /// Si nada cambió en pantalla hace 3 s, suelta la superficie que no se ve (se vuelve a crear en el próximo cambio).
    pub fn release_idle(&mut self) { if self.changed.elapsed().as_secs() >= 3 { self.bufs[1 - self.shown] = None; } }

    /// Ventana tapada: suelta las dos.
    pub fn release_all(&mut self) {
        // SAFETY: quitar el contenido de la capa es válido.
        unsafe { self.layer.setContents(None) };
        self.bufs = [None, None];
    }

    /// Muestra un frame cuyas franjas tienen las huellas `bands`; `paint(px, ancho_de_fila, franja)` repinta una.
    /// False si el compositor aún usa la superficie libre: hay que reintentar.
    pub fn present(&mut self, bands: &[u64], mut paint: impl FnMut(&mut [u32], usize, usize)) -> bool {
        if self.w == 0 || self.bufs[self.shown].as_ref().is_some_and(|b| b.bands == bands) { return true; }
        let next = 1 - self.shown;
        let (w, h) = (self.w, self.h);
        let b = self.bufs[next].get_or_insert_with(|| Buf { s: new_surface(w, h), bands: vec![] });
        // SAFETY: -[IOSurface isInUse] devuelve BOOL.
        let busy: Bool = unsafe { msg_send![&*b.s, isInUse] };
        if busy.as_bool() { return false; }
        locked(&b.s, h, |px, stride| {
            for (i, hash) in bands.iter().enumerate() {
                if b.bands.get(i) != Some(hash) { paint(px, stride, i); }
            }
        });
        b.bands = bands.to_vec();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: CALayer acepta una IOSurface como contenido.
        unsafe { self.layer.setContents(Some(&*b.s)) };
        CATransaction::commit();
        (self.shown, self.changed) = (next, Instant::now());
        true
    }
}

/// Bloquea la superficie para la CPU y entrega sus píxeles y los píxeles por fila.
fn locked(b: &AnyObject, h: usize, f: impl FnOnce(&mut [u32], usize)) {
    // SAFETY: entre lock y unlock, baseAddress apunta a bytesPerRow * alto bytes alineados.
    unsafe {
        let r: i32 = msg_send![b, lockWithOptions: 0u32, seed: std::ptr::null_mut::<u32>()];
        assert_eq!(r, 0, "lock de IOSurface");
        let base: *mut c_void = msg_send![b, baseAddress];
        let stride: isize = msg_send![b, bytesPerRow];
        let stride = stride as usize / 4;
        f(std::slice::from_raw_parts_mut(base.cast::<u32>(), stride * h), stride);
        let _: i32 = msg_send![b, unlockWithOptions: 0u32, seed: std::ptr::null_mut::<u32>()];
    }
}

fn new_surface(w: usize, h: usize) -> Retained<AnyObject> {
    let props = NSMutableDictionary::<NSString, NSNumber>::new();
    for (k, v) in [("IOSurfaceWidth", w), ("IOSurfaceHeight", h), ("IOSurfaceBytesPerElement", 4), ("IOSurfacePixelFormat", u32::from_be_bytes(*b"BGRA") as usize)] {
        // SAFETY: NSString es una clave copiable válida.
        unsafe { let _: () = msg_send![&*props, setObject: &*NSNumber::new_usize(v), forKey: &*NSString::from_str(k)]; }
    }
    let cls: &AnyClass = class!(IOSurface);
    // SAFETY: initWithProperties: devuelve un objeto propio o nil; el plist de color es +1 y se libera con Retained.
    unsafe {
        let s: Option<Retained<AnyObject>> = msg_send![msg_send![cls, alloc], initWithProperties: &*props];
        let s = s.expect("IOSurface");
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        let plist = Retained::from_raw(CGColorSpaceCopyPropertyList(space) as *mut AnyObject).expect("espacio de color");
        let _: () = msg_send![&*s, setAttachment: &*plist, forKey: &*NSString::from_str("IOSurfaceColorSpace")];
        CGColorSpaceRelease(space);
        s
    }
}
