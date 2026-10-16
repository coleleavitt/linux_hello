//! Ambient-light subtraction, face brightness and the IR lighting status (F12–F14, F43–F45).

use image::GrayImage;

/// Lit face brighter than this: too close or saturated (F44 status 2).
pub const MAX_LIT: f32 = 205.0;
/// Lit face darker than this: too far (F44 status 3).
pub const MIN_LIT: f32 = 35.0;
/// Minimum face brightness the emitter must add (lit − ambient), out of 255 (F14).
pub const MIN_EMITTER_GAIN: f32 = 45.0;
/// Windows' maximum ambient-only face brightness: more means strong ambient IR such as sunlight
/// (F14, status 4: the print is matched, the frame cannot vote live).
pub const MAX_AMBIENT: f32 = 25.0;

static MAX_AMBIENT_LIMIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(MAX_AMBIENT.to_bits());

/// The ambient limit in use ([`MAX_AMBIENT`] unless raised with [`set_max_ambient`]).
pub fn max_ambient() -> f32 {
    f32::from_bits(MAX_AMBIENT_LIMIT.load(std::sync::atomic::Ordering::Relaxed))
}

/// Raise the ambient limit above Windows' 25 (`daemon --max-ambient`). Above 25 the subtraction
/// is a weaker liveness signal: a photo lit by the same ambient IR looks more like a face to the
/// emitter-gain rule, and only the anti-spoofing nets then tell them apart (docs/capture-and-liveness.md §3).
pub fn set_max_ambient(limit: f32) {
    MAX_AMBIENT_LIMIT.store(limit.to_bits(), std::sync::atomic::Ordering::Relaxed);
}
/// Clipped pixels (value 255) a region may hold, as a fraction of its pixels, before it counts as
/// saturated (`CheckIfTooMuchSaturatedPixelOnFace`, F44: area · 1 / 100).
pub const MAX_CLIPPED: f32 = 0.01;
/// The band around the face where clipped pixels give status 7: the face box grown by this
/// fraction of its width and height on every side, outside the face ellipse.
pub const CLIPPED_BAND: f32 = 0.25;

/// A 2×3 affine matrix: `x' = m[0][0]·x + m[0][1]·y + m[0][2]`, `y' = m[1][0]·x + m[1][1]·y + m[1][2]`.
pub type Affine = [[f32; 3]; 2];

/// `lit − min(lit, ambient)` per pixel: the light the emitter added
/// (`SubtractAmbientFrameFromIlluminatedFrame`, F13, F40). For recognition `ambient` is the
/// ambient frame moved onto the lit one by [`warp`]; for the liveness nets it is the frame as captured.
pub fn subtract_ambient(lit: &GrayImage, ambient: &GrayImage) -> GrayImage {
    GrayImage::from_fn(lit.width(), lit.height(), |x, y| {
        let l = lit.get_pixel(x, y).0[0];
        let a = ambient.get_pixel(x, y).0[0];
        image::Luma([l.saturating_sub(a)])
    })
}

/// The three motion-compensation anchors of `ComputeAnchorPoints` (0x1800372c8, F13): the means
/// of the left-eye points, the right-eye points and the mouth points of the 31-point landmark set.
/// On the RetinaEngine path those points are copies of the detector's five
/// (`RetinaEngine::ComputeAlignment` 0x18003d270), so the anchors are the eye centres and the
/// midpoint of the mouth corners.
pub fn anchors(points: &[[f32; 2]; 5]) -> [[f32; 2]; 3] {
    let [le, re, _, lm, rm] = *points;
    [le, re, [(lm[0] + rm[0]) / 2.0, (lm[1] + rm[1]) / 2.0]]
}

/// Where the anchors were in the ambient frame, by linear motion between the current lit frame
/// (`t` = 0) and the previous one (`t` = 1): `cur + (prev − cur)·t` (`AdjustFaceForAmbientLight`
/// 0x180036c10, F13). Windows weighs the two the other way round, which is the same at the only
/// point it sees, `t` = 0.5 (alternate frames at a fixed rate).
pub fn interpolate(cur: &[[f32; 2]; 3], prev: &[[f32; 2]; 3], t: f32) -> [[f32; 2]; 3] {
    let mut out = *cur;
    for (o, p) in out.iter_mut().zip(prev) {
        o[0] += (p[0] - o[0]) * t;
        o[1] += (p[1] - o[1]) * t;
    }
    out
}

/// The affine map taking the three `src` points to the three `dst` points
/// (`Affine2DMatrix::DeriveTransform` 0x180032c34, three rows `[x, y, 1 | u, v]` solved exactly).
/// `None` when the points are collinear.
pub fn affine_from_anchors(src: &[[f32; 2]; 3], dst: &[[f32; 2]; 3]) -> Option<Affine> {
    let [[x0, y0], [x1, y1], [x2, y2]] = src.map(|p| p.map(f64::from));
    let det = (x1 - x0) * (y2 - y0) - (x2 - x0) * (y1 - y0);
    if det.abs() < 1e-6 {
        return None;
    }
    let row = |i: usize| {
        let [u0, u1, u2] = [dst[0][i], dst[1][i], dst[2][i]].map(f64::from);
        let a = ((u1 - u0) * (y2 - y0) - (u2 - u0) * (y1 - y0)) / det;
        let b = ((u2 - u0) * (x1 - x0) - (u1 - u0) * (x2 - x0)) / det;
        [a as f32, b as f32, (u0 - a * x0 - b * y0) as f32]
    };
    Some([row(0), row(1)])
}

/// The ambient frame moved onto the lit one: output pixel `p` reads `ambient(m·p)`, bilinear with
/// zero padding and truncated (`ApplyAffineTransformToImage` 0x18002ad88,
/// `BilinearInterpolationWithPadding<uchar>` 0x18000c6b0), where `m` maps the lit anchors to the
/// ambient ones ([`affine_from_anchors`]).
pub fn warp(ambient: &GrayImage, m: &Affine) -> GrayImage {
    let (w, h) = (ambient.width() as usize, ambient.height() as usize);
    let data = ambient.as_raw();
    let at = |x: i64, y: i64| {
        if x >= 0 && (x as usize) < w && y >= 0 && (y as usize) < h {
            data[y as usize * w + x as usize] as f32
        } else {
            0.0
        }
    };
    let mut out = vec![0u8; w * h];
    for (y, row) in out.chunks_exact_mut(w).enumerate() {
        let (ox, oy) = (m[0][1] * y as f32 + m[0][2], m[1][1] * y as f32 + m[1][2]);
        for (x, px) in row.iter_mut().enumerate() {
            let (sx, sy) = (m[0][0] * x as f32 + ox, m[1][0] * x as f32 + oy);
            let (fx, fy) = (sx.floor(), sy.floor());
            let (x0, y0) = (fx as i64, fy as i64);
            let (tx, ty) = (sx - fx, sy - fy);
            let (p00, p10, p01, p11) = if x0 >= 0 && y0 >= 0 && (x0 as usize) + 1 < w && (y0 as usize) + 1 < h {
                let i = y0 as usize * w + x0 as usize;
                (data[i] as f32, data[i + 1] as f32, data[i + w] as f32, data[i + w + 1] as f32)
            } else {
                (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1))
            };
            let top = p00 + (p10 - p00) * tx;
            let bottom = p01 + (p11 - p01) * tx;
            *px = (top + (bottom - top) * ty) as u8;
        }
    }
    GrayImage::from_raw(ambient.width(), ambient.height(), out).expect("w × h bytes")
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
    /// Status 6: more than [`MAX_CLIPPED`] of the face ellipse is clipped (too close, or a lamp
    /// on the face). The frame is rejected, counted as too close like status 2 (F45).
    Clipped,
    /// Status 7: more than [`MAX_CLIPPED`] of the band around the face is clipped (a lamp behind
    /// the head). Windows keeps the print but tags it (flag 0x20) and, the status not being 1,
    /// does not subtract the ambient frame; here the frame is treated as status 4: print from
    /// the lit image, no live vote, no augmentation (F45).
    ClippedAround,
}

/// Face brightness (F43) in the lit and in the ambient frame, and the clipped fractions of the
/// lit face (F44).
#[derive(Debug, Clone, Copy)]
pub struct Lighting {
    pub lit: f32,
    pub ambient: f32,
    /// Fraction of clipped pixels in the face ellipse of the lit frame
    pub clipped_face: f32,
    /// Fraction of clipped pixels in the band around the face in the lit frame
    pub clipped_around: f32,
}

impl Lighting {
    /// `points` are the face's five landmarks and `bbox` its box on the lit frame; `ambient` is the
    /// ambient frame moved onto the lit one ([`warp`]), where Windows measures the ambient
    /// brightness (F43).
    pub fn measure(lit: &GrayImage, ambient: &GrayImage, points: &[[f32; 2]; 5], bbox: &[f32; 4]) -> Self {
        let (clipped_face, clipped_around) = clipped(lit, bbox);
        Self {
            lit: face_brightness(lit, points),
            ambient: face_brightness(ambient, points),
            clipped_face,
            clipped_around,
        }
    }

    /// What the emitter added to the face: `d` in F14.
    pub fn gain(&self) -> f32 {
        self.lit - self.ambient
    }

    /// Exposure first (F44), then the clipped pixels (F44 status 6/7), then the lit/ambient
    /// comparison (F14). Windows also turns `d > 205` into status 2; that cannot happen here once
    /// the lit face is ≤ 205.
    pub fn status(&self) -> LightingStatus {
        if self.lit > MAX_LIT {
            LightingStatus::TooBright
        } else if self.lit < MIN_LIT {
            LightingStatus::TooDark
        } else if self.clipped_face > MAX_CLIPPED {
            LightingStatus::Clipped
        } else if self.clipped_around > MAX_CLIPPED {
            LightingStatus::ClippedAround
        } else if self.gain() < MIN_EMITTER_GAIN || self.ambient > max_ambient() {
            LightingStatus::Weak
        } else {
            LightingStatus::Good
        }
    }
}

/// Fraction of clipped pixels (value 255) inside the ellipse inscribed in `bbox`, and in the band
/// around it: the box grown by [`CLIPPED_BAND`] of its size on every side, outside the ellipse
/// (`CheckIfTooMuchSaturatedPixelOnFace`, F44). Windows counts a pixel as clipped when it equals
/// its neighbours 2 px away in both directions; here a pixel is clipped at the sensor's maximum.
/// Regions are clamped to the image; an empty region reads 0.
pub fn clipped(image: &GrayImage, bbox: &[f32; 4]) -> (f32, f32) {
    let [x1, y1, x2, y2] = *bbox;
    let (w, h) = (x2 - x1, y2 - y1);
    let (cx, cy, a, b) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0, w / 2.0, h / 2.0);
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let (bx1, bx2) = (clamp(x1 - CLIPPED_BAND * w, image.width()), clamp(x2 + CLIPPED_BAND * w, image.width()));
    let (by1, by2) = (clamp(y1 - CLIPPED_BAND * h, image.height()), clamp(y2 + CLIPPED_BAND * h, image.height()));
    let (mut face, mut around) = ((0u32, 0u32), (0u32, 0u32));
    for y in by1..by2 {
        for x in bx1..bx2 {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / a, (y as f32 + 0.5 - cy) / b);
            let region = if dx * dx + dy * dy <= 1.0 { &mut face } else { &mut around };
            region.0 += 1;
            region.1 += u32::from(image.get_pixel(x, y).0[0] == u8::MAX);
        }
    }
    let fraction = |(n, clipped): (u32, u32)| if n == 0 { 0.0 } else { clipped as f32 / n as f32 };
    (fraction(face), fraction(around))
}

/// Mean of every 2nd pixel in every 2nd row of the block x ∈ [max(p3.x, p9.x), min(p4.x, p15.x)],
/// y ∈ [max(p9.y, p15.y), min(p3.y, p4.y)] of the 31-point landmark set (`EvaluateBrightnessPerFace`
/// 0x180017140, F43): between the eyes and the mouth corners. On the RetinaEngine path the eye-contour
/// points p9 and p15 are the eye centres (`RetinaEngine::ComputeAlignment` 0x18003d270), so the block
/// follows from the five detector points. 0 if the block is empty (strong head turn).
pub fn face_brightness(image: &GrayImage, points: &[[f32; 2]; 5]) -> f32 {
    let [le, re, _nose, lm, rm] = *points;
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
    if n == 0 {
        0.0
    } else {
        sum as f32 / n as f32
    }
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
        let status = |lit, ambient| Lighting { lit, ambient, clipped_face: 0.0, clipped_around: 0.0 }.status();
        assert_eq!(status(80.0, 10.0), LightingStatus::Good);
        assert_eq!(status(210.0, 0.0), LightingStatus::TooBright);
        assert_eq!(status(30.0, 0.0), LightingStatus::TooDark);
        assert_eq!(status(50.0, 10.0), LightingStatus::Weak); // emitter adds too little
        assert_eq!(status(120.0, 30.0), LightingStatus::Weak); // strong ambient IR
        set_max_ambient(50.0);
        assert_eq!(status(120.0, 30.0), LightingStatus::Good);
        set_max_ambient(MAX_AMBIENT);
        let clipped =
            |clipped_face, clipped_around| Lighting { lit: 80.0, ambient: 10.0, clipped_face, clipped_around }.status();
        assert_eq!(clipped(0.02, 0.0), LightingStatus::Clipped);
        assert_eq!(clipped(0.0, 0.02), LightingStatus::ClippedAround);
        assert_eq!(clipped(0.01, 0.01), LightingStatus::Good);
        // Too bright is told first; the band status is not changed to weak.
        assert_eq!(
            Lighting { lit: 210.0, ambient: 0.0, clipped_face: 0.5, clipped_around: 0.5 }.status(),
            LightingStatus::TooBright
        );
        assert_eq!(
            Lighting { lit: 50.0, ambient: 10.0, clipped_face: 0.0, clipped_around: 0.5 }.status(),
            LightingStatus::ClippedAround
        );
    }

    #[test]
    fn clipped_pixels_in_the_face_ellipse_and_the_band_around() {
        // A 40×40 box at (20, 20); the band is 10 px wide (25 % of the box) on every side.
        let bbox = [20.0, 20.0, 60.0, 60.0];
        let dark = GrayImage::from_pixel(100, 100, image::Luma([80]));
        assert_eq!(clipped(&dark, &bbox), (0.0, 0.0));
        // A 4×4 clipped patch at the centre: 16 of the ellipse's ≈ 1257 pixels (1.3 %), none around.
        let spot = GrayImage::from_fn(100, 100, |x, y| {
            image::Luma([if (38..42).contains(&x) && (38..42).contains(&y) { 255 } else { 80 }])
        });
        let (face, around) = clipped(&spot, &bbox);
        assert!((face - 16.0 / 1257.0).abs() < 0.002, "{face}");
        assert_eq!(around, 0.0);
        // A lamp behind the head: the band left of the face is clipped, the face is not.
        let lamp = GrayImage::from_fn(100, 100, |x, _| image::Luma([if x < 20 { 255 } else { 80 }]));
        let (face, around) = clipped(&lamp, &bbox);
        assert_eq!(face, 0.0);
        // 10 × 60 clipped pixels of the band's 3600 − 1257 ≈ 2343 pixels.
        assert!((around - 600.0 / 2343.0).abs() < 0.01, "{around}");
        // Corner pixels of the box lie outside the ellipse, in the band.
        let corner = GrayImage::from_fn(100, 100, |x, y| image::Luma([if x == 20 && y == 20 { 255 } else { 80 }]));
        let (face, around) = clipped(&corner, &bbox);
        assert_eq!(face, 0.0);
        assert!(around > 0.0);
        // A box at the image edge is clamped.
        assert_eq!(clipped(&dark, &[-10.0, -10.0, 30.0, 30.0]), (0.0, 0.0));
    }

    #[test]
    fn brightness_box_spans_eyes_to_mouth() {
        // 100 inside x 10..20, y 10..20; 0 elsewhere.
        let image = GrayImage::from_fn(40, 40, |x, y| {
            image::Luma([if (10..20).contains(&x) && (10..20).contains(&y) { 100 } else { 0 }])
        });
        let points = [[8.0, 9.0], [22.0, 10.0], [15.0, 15.0], [10.0, 21.0], [20.0, 20.0]];
        assert_eq!(face_brightness(&image, &points), 100.0);
        let turned = [[20.0, 10.0], [10.0, 10.0], [15.0, 15.0], [20.0, 20.0], [10.0, 20.0]];
        assert_eq!(face_brightness(&image, &turned), 0.0);
    }

    #[test]
    fn warp_moves_the_ambient_frame_onto_the_lit_one() {
        // The face moved 3 px right and 1 px down between the ambient and the lit frame.
        let ambient = GrayImage::from_fn(40, 40, |x, y| image::Luma([(x * 5 + y * 3) as u8]));
        let lit_anchors = [[13.0, 11.0], [23.0, 11.0], [18.0, 21.0]];
        let ambient_anchors = lit_anchors.map(|[x, y]| [x - 3.0, y - 1.0]);
        let m = affine_from_anchors(&lit_anchors, &ambient_anchors).unwrap();
        let warped = warp(&ambient, &m);
        assert_eq!(warped.get_pixel(20, 20).0[0], ambient.get_pixel(17, 19).0[0]);
        assert_eq!(warped.get_pixel(1, 0).0[0], 0, "reads outside the ambient frame as 0");
        assert!(affine_from_anchors(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0]], &lit_anchors).is_none());
    }

    #[test]
    fn anchors_interpolate_between_lit_frames() {
        let cur = [[10.0, 10.0], [20.0, 10.0], [15.0, 20.0]];
        let prev = [[12.0, 10.0], [22.0, 10.0], [17.0, 20.0]];
        assert_eq!(interpolate(&cur, &prev, 0.5), [[11.0, 10.0], [21.0, 10.0], [16.0, 20.0]]);
        assert_eq!(interpolate(&cur, &prev, -0.5), [[9.0, 10.0], [19.0, 10.0], [14.0, 20.0]]);
        assert_eq!(
            anchors(&[[1.0, 2.0], [3.0, 2.0], [2.0, 3.0], [1.0, 5.0], [3.0, 7.0]]),
            [[1.0, 2.0], [3.0, 2.0], [2.0, 6.0]]
        );
    }
}
