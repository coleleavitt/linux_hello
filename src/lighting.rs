//! Ambient-light subtraction and the IR lighting gate (F12–F14).

use image::GrayImage;

use crate::Face;

/// Minimum mean face brightness the emitter must add (lit − ambient), out of 255 (F14).
pub const MIN_EMITTER_GAIN: f32 = 45.0;
/// Above this the face is saturated (F14 status 2).
pub const MAX_EMITTER_GAIN: f32 = 205.0;
/// Maximum mean ambient-only face brightness: more means strong ambient IR such as sunlight
/// (F14, `ImproveFaceQualityFilter`).
pub const MAX_AMBIENT: f32 = 25.0;

/// `lit − min(lit, ambient)` per pixel: the light the emitter added (F13).
///
/// Windows first warps the ambient face onto the lit one with an affine transform from eye and
/// nose/mouth anchors. The two frames here are 67 ms apart (15 fps), so the subtraction is done
/// without motion compensation.
pub fn subtract_ambient(lit: &GrayImage, ambient: &GrayImage) -> GrayImage {
    GrayImage::from_fn(lit.width(), lit.height(), |x, y| {
        let l = lit.get_pixel(x, y).0[0];
        let a = ambient.get_pixel(x, y).0[0];
        image::Luma([l.saturating_sub(a)])
    })
}

/// Mean brightness of the face box in the lit and in the ambient frame.
#[derive(Debug, Clone, Copy)]
pub struct Lighting {
    pub lit: f32,
    pub ambient: f32,
}

impl Lighting {
    pub fn measure(lit: &GrayImage, ambient: &GrayImage, face: &Face) -> Self {
        Self { lit: mean_in_box(lit, face.bbox), ambient: mean_in_box(ambient, face.bbox) }
    }

    /// What the emitter added to the face: `d` in F14.
    pub fn gain(&self) -> f32 {
        self.lit - self.ambient
    }

    /// F14 status 1 ("good"). Any other frame casts no liveness vote and is not matched.
    pub fn is_good(&self) -> bool {
        (MIN_EMITTER_GAIN..=MAX_EMITTER_GAIN).contains(&self.gain()) && self.ambient <= MAX_AMBIENT
    }
}

/// Mean pixel value inside `bbox` (clamped to the image); 0 for an empty box.
fn mean_in_box(image: &GrayImage, bbox: [f32; 4]) -> f32 {
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let (x1, y1) = (clamp(bbox[0], image.width()), clamp(bbox[1], image.height()));
    let (x2, y2) = (clamp(bbox[2], image.width()), clamp(bbox[3], image.height()));
    let mut sum = 0u64;
    for y in y1..y2 {
        for x in x1..x2 {
            sum += image.get_pixel(x, y).0[0] as u64;
        }
    }
    let n = (x2.saturating_sub(x1) as u64) * (y2.saturating_sub(y1) as u64);
    if n == 0 { 0.0 } else { sum as f32 / n as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(bbox: [f32; 4]) -> Face {
        Face { bbox, score: 1.0, landmarks: [[0.0; 2]; 5] }
    }

    #[test]
    fn subtraction_saturates_at_zero() {
        let lit = GrayImage::from_raw(2, 1, vec![100, 10]).unwrap();
        let ambient = GrayImage::from_raw(2, 1, vec![30, 50]).unwrap();
        assert_eq!(subtract_ambient(&lit, &ambient).into_raw(), vec![70, 0]);
    }

    #[test]
    fn gate_limits() {
        let lighting = |lit, ambient| Lighting { lit, ambient };
        assert!(lighting(80.0, 10.0).is_good());
        assert!(!lighting(50.0, 10.0).is_good()); // emitter adds too little: screen, far away
        assert!(!lighting(230.0, 10.0).is_good()); // saturated
        assert!(!lighting(120.0, 30.0).is_good()); // strong ambient IR
    }

    #[test]
    fn box_mean_is_clamped_to_the_image() {
        let image = GrayImage::from_fn(4, 4, |x, _| image::Luma([if x < 2 { 100 } else { 0 }]));
        assert_eq!(mean_in_box(&image, face([-5.0, -5.0, 2.0, 10.0]).bbox), 100.0);
        assert_eq!(mean_in_box(&image, face([0.0, 0.0, 4.0, 4.0]).bbox), 50.0);
        assert_eq!(mean_in_box(&image, face([3.0, 3.0, 3.0, 3.0]).bbox), 0.0);
    }
}
