//! Ambient-light subtraction, face brightness and the IR lighting status (F12–F14, F43–F45).

use image::GrayImage;

use crate::Face;

/// Lit face brighter than this: too close or saturated (F44 status 2).
pub const MAX_LIT: f32 = 205.0;
/// Lit face darker than this: too far (F44 status 3).
pub const MIN_LIT: f32 = 35.0;
/// Minimum face brightness the emitter must add (lit − ambient), out of 255 (F14).
pub const MIN_EMITTER_GAIN: f32 = 45.0;
/// Maximum ambient-only face brightness: more means strong ambient IR such as sunlight (F14).
pub const MAX_AMBIENT: f32 = 25.0;

/// `lit − min(lit, ambient)` per pixel: the light the emitter added (F13).
///
/// Windows first warps the ambient face onto the lit one with an affine transform from eye and
/// mouth anchors. The two frames here are 67 ms apart (15 fps), so the subtraction is done
/// without motion compensation.
pub fn subtract_ambient(lit: &GrayImage, ambient: &GrayImage) -> GrayImage {
    GrayImage::from_fn(lit.width(), lit.height(), |x, y| {
        let l = lit.get_pixel(x, y).0[0];
        let a = ambient.get_pixel(x, y).0[0];
        image::Luma([l.saturating_sub(a)])
    })
}

/// Lighting status of a face, after Windows' exposure and lighting statuses (F44).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightingStatus {
    /// Status 1: recognition uses the ambient-subtracted image and the frame can vote live.
    Good,
    /// Status 2: too bright (too close). The frame is rejected.
    TooBright,
    /// Status 3: too dark (too far). The frame is rejected.
    TooDark,
    /// Status 4: the emitter adds too little or there is strong ambient IR. The print (from the
    /// lit image) is used for matching, but the frame casts no live vote and is not used for
    /// template augmentation (F45).
    Weak,
}

/// Face brightness (F43) in the lit and in the ambient frame.
#[derive(Debug, Clone, Copy)]
pub struct Lighting {
    pub lit: f32,
    pub ambient: f32,
}

impl Lighting {
    pub fn measure(lit: &GrayImage, ambient: &GrayImage, face: &Face) -> Self {
        Self { lit: face_brightness(lit, face), ambient: face_brightness(ambient, face) }
    }

    /// What the emitter added to the face: `d` in F14.
    pub fn gain(&self) -> f32 {
        self.lit - self.ambient
    }

    /// Exposure first (F44), then the lit/ambient comparison (F14). Windows also turns
    /// `d > 205` into status 2; that cannot happen here once the lit face is ≤ 205.
    pub fn status(&self) -> LightingStatus {
        if self.lit > MAX_LIT {
            LightingStatus::TooBright
        } else if self.lit < MIN_LIT {
            LightingStatus::TooDark
        } else if self.gain() < MIN_EMITTER_GAIN || self.ambient > MAX_AMBIENT {
            LightingStatus::Weak
        } else {
            LightingStatus::Good
        }
    }
}

/// Mean of every 2nd pixel in every 2nd row of the block between the eyes and the mouth corners
/// (`EvaluateBrightnessPerFace`, F43). Windows bounds the block with eye-contour and mouth-corner
/// points of its 31-point aligner; with SCRFD's 5 points it spans x from the inner of (left eye,
/// left mouth corner) to the inner of (right eye, right mouth corner), and y from the lower eye
/// to the higher mouth corner. 0 if the block is empty (strong head turn).
pub fn face_brightness(image: &GrayImage, face: &Face) -> f32 {
    let [le, re, _nose, lm, rm] = face.landmarks;
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let (x1, x2) = (clamp(le[0].max(lm[0]), image.width()), clamp(re[0].min(rm[0]), image.width()));
    let (y1, y2) = (clamp(le[1].max(re[1]), image.height()), clamp(lm[1].min(rm[1]), image.height()));
    let (mut sum, mut n) = (0u64, 0u64);
    for y in (y1..y2).step_by(2) {
        for x in (x1..x2).step_by(2) {
            sum += image.get_pixel(x, y).0[0] as u64;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum as f32 / n as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtraction_saturates_at_zero() {
        let lit = GrayImage::from_raw(2, 1, vec![100, 10]).unwrap();
        let ambient = GrayImage::from_raw(2, 1, vec![30, 50]).unwrap();
        assert_eq!(subtract_ambient(&lit, &ambient).into_raw(), vec![70, 0]);
    }

    #[test]
    fn statuses() {
        let status = |lit, ambient| Lighting { lit, ambient }.status();
        assert_eq!(status(80.0, 10.0), LightingStatus::Good);
        assert_eq!(status(210.0, 0.0), LightingStatus::TooBright);
        assert_eq!(status(30.0, 0.0), LightingStatus::TooDark);
        assert_eq!(status(50.0, 10.0), LightingStatus::Weak); // emitter adds too little
        assert_eq!(status(120.0, 30.0), LightingStatus::Weak); // strong ambient IR
    }

    #[test]
    fn brightness_box_spans_eyes_to_mouth() {
        // 100 inside x 10..20, y 10..20; 0 elsewhere.
        let image = GrayImage::from_fn(40, 40, |x, y| {
            image::Luma([if (10..20).contains(&x) && (10..20).contains(&y) { 100 } else { 0 }])
        });
        let face = Face {
            bbox: [0.0, 0.0, 40.0, 40.0],
            score: 1.0,
            landmarks: [[8.0, 9.0], [22.0, 10.0], [15.0, 15.0], [10.0, 21.0], [20.0, 20.0]],
        };
        assert_eq!(face_brightness(&image, &face), 100.0);
        let turned = Face { landmarks: [[20.0, 10.0], [10.0, 10.0], [15.0, 15.0], [20.0, 20.0], [10.0, 20.0]], ..face };
        assert_eq!(face_brightness(&image, &turned), 0.0);
    }
}
