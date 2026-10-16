//! Physics-based liveness checks: each measures a physical difference between a face and a
//! print or a screen that infrared-rich room light cannot fake (docs/liveness-physics.md). They
//! are model-free [`Check`]s over what the pipeline found, scored on every face with a print
//! ([`crate::Authenticator`] logs them as `glint=0.8 shape=0.3 …`); a check vetoes a frame only
//! with a [`Check::threshold`], set where no genuine recording loses more than 1 % of its good
//! frames, and none has one yet (`VETO_THRESHOLDS`).

use anyhow::Result;
use image::{GrayImage, RgbImage};

use crate::lighting::block_mean;
use crate::liveness::check::{Check, Frame, Observed};
use crate::{FaceCheck, Vote};

/// The checks, in the order they are logged.
pub fn all() -> Vec<Box<dyn Check>> {
    vec![Box::new(Glint), Box::new(Shape), Box::new(Flicker), Box::new(Material)]
}

/// Score every check on every face of `check` that has a print, record the scores in
/// [`FaceCheck::physics`] and, where a check has a threshold the face fails, take the frame's
/// live vote away (−1 on Windows' frame total, as its colour net, F24: the frame abstains).
pub fn apply(checks: &mut [Box<dyn Check>], frame: &Frame, check: &mut crate::FrameCheck) -> Result<()> {
    for (i, face) in check.faces.iter_mut().enumerate() {
        if face.analysis.print.is_none() {
            continue;
        }
        for c in checks.iter_mut() {
            let score = c.score(frame, i)?;
            face.physics.push((c.name().to_string(), score));
            let vetoed = match (score, c.threshold()) {
                (Some(s), Some(t)) => !c.passes(s, t),
                _ => false,
            };
            if vetoed && face.vote == Vote::Live {
                face.vote = Vote::Abstain;
            }
        }
    }
    Ok(())
}

/// The face's five points as the checks use them: the Windows IR detector's when it has the
/// face (the points Windows crops with, F23), else SCRFD's.
fn points(f: &FaceCheck) -> [[f32; 2]; 5] {
    f.analysis.windows_face.as_ref().unwrap_or(&f.analysis.face).landmarks
}

/// Inter-ocular distance: the scale every measure is normalised by (∝ 1 / distance).
fn iod(points: &[[f32; 2]; 5]) -> f32 {
    let [le, re, ..] = points;
    (re[0] - le[0]).hypot(re[1] - le[1])
}

/// What the emitter added at a pixel: `lit − min(lit, ambient)` (F13), 0 outside the frame.
fn emitter_at(lit: &GrayImage, ambient: &GrayImage, x: i64, y: i64) -> f32 {
    if x < 0 || y < 0 || x >= lit.width() as i64 || y >= lit.height() as i64 {
        return 0.0;
    }
    let (x, y) = (x as u32, y as u32);
    lit.get_pixel(x, y).0[0].saturating_sub(ambient.get_pixel(x, y).0[0]) as f32
}

/// The glint: the emitter's specular reflection on each cornea, a point of a few pixels within
/// [`GLINT_RADIUS`] of the eye centre in the lit frame and absent from the ambient frame; a
/// print's matte surface has none, and a screen reflects the emitter off its glass as a broad
/// blob, not at the eyes. Score = the larger over the two eyes of the brightest small spot in
/// the eye window of the emitter image (pixel minus the mean of the ring between 3×3 and 9×9
/// around it), as a fraction of the face's emitter gain (F14 `d`), so that distance and
/// exposure cancel. The glint hides under the lid when the user looks down at the screen, so a
/// single frame without one proves nothing: a session's maximum is the measure.
pub struct Glint;

/// Eye window around each eye centre, as a fraction of the inter-ocular distance.
pub const GLINT_RADIUS: f32 = 0.15;
const GLINT_INNER: i64 = 1;
const GLINT_OUTER: i64 = 4;

/// The brightest small spot of the emitter image within `radius` px of `centre`.
fn spot(lit: &GrayImage, ambient: &GrayImage, centre: [f32; 2], radius: i64) -> f32 {
    let (cx, cy) = (centre[0].round() as i64, centre[1].round() as i64);
    let ring_pixels = ((2 * GLINT_OUTER + 1).pow(2) - (2 * GLINT_INNER + 1).pow(2)) as f32;
    let mut best = f32::MIN;
    for y in cy - radius..=cy + radius {
        for x in cx - radius..=cx + radius {
            let mut ring = 0.0;
            for dy in -GLINT_OUTER..=GLINT_OUTER {
                for dx in -GLINT_OUTER..=GLINT_OUTER {
                    if dx.abs() > GLINT_INNER || dy.abs() > GLINT_INNER {
                        ring += emitter_at(lit, ambient, x + dx, y + dy);
                    }
                }
            }
            best = best.max(emitter_at(lit, ambient, x, y) - ring / ring_pixels);
        }
    }
    best
}

impl Check for Glint {
    fn name(&self) -> &str {
        "glint"
    }

    fn score(&mut self, frame: &Frame, face: usize) -> Result<Option<f32>> {
        let f = frame.face(face);
        let Some(gain) = f.analysis.lighting.map(|l| l.gain()).filter(|g| *g > 0.0) else { return Ok(None) };
        let pair = &frame.current.capture.pair;
        let points = points(f);
        let radius = (GLINT_RADIUS * iod(&points)).round() as i64;
        let [le, re, ..] = points;
        let best = spot(&pair.lit, &pair.ambient, le, radius).max(spot(&pair.lit, &pair.ambient, re, radius));
        Ok(Some(best / gain))
    }
}

/// Mean of the emitter's light over the square of half-side `half` at `centre`, clamped to the
/// frame; `None` when the square lies outside it.
fn patch(lit: &GrayImage, ambient: &GrayImage, centre: [f32; 2], half: f32) -> Option<f32> {
    let (cx, cy, h) = (centre[0].round() as i64, centre[1].round() as i64, (half as i64).max(1));
    let (x1, x2) = ((cx - h).max(0), (cx + h + 1).min(lit.width() as i64));
    let (y1, y2) = ((cy - h).max(0), (cy + h + 1).min(lit.height() as i64));
    if x2 <= x1 || y2 <= y1 {
        return None;
    }
    let mut sum = 0.0;
    for y in y1..y2 {
        for x in x1..x2 {
            sum += emitter_at(lit, ambient, x, y);
        }
    }
    Some(sum / ((x2 - x1) * (y2 - y1)) as f32)
}

/// Emitter falloff over the face: the emitter is a point source a few centimetres from the
/// lens, so a face at 40–70 cm is lit by 1/r² and by Lambert's cosine: the nose, cheeks and
/// forehead face the emitter and are near, the sides of the face turn away and fall off, and
/// the emitter sits to one side of the lens, so one cheek is brighter than the other. A flat
/// print is lit evenly: what shading it shows is printed, and most inks are transparent at
/// 850 nm, so the print reads nearly flat. Score = the RMS z-distance of the face's profile
/// from the genuine profile: the mean of the emitter image over nine patches (half-side 0.1·IOD)
/// at the nose, the cheeks, the forehead, the chin, the temples and the brows, each divided by
/// their mean, against [`GENUINE_PROFILE`]. Patches outside the frame (chin cut) are left out.
pub struct Shape;

/// Mean and standard deviation of each patch's share of the profile over 145 frontal frames of
/// the owner (`lh_lock`, `lh_now`, `lh_view`, `validation_frames`; one person, one camera): nose,
/// left cheek, right cheek, forehead, chin, left temple, right temple, left brow, right brow.
pub const GENUINE_PROFILE: [(f32, f32); 9] = [
    (1.349, 0.034),
    (1.495, 0.059),
    (1.242, 0.076),
    (1.312, 0.054),
    (1.037, 0.034),
    (0.461, 0.110),
    (0.352, 0.067),
    (0.994, 0.053),
    (0.758, 0.043),
];

/// The patch centres in the order of [`GENUINE_PROFILE`], from the five points: along the
/// eye line (`right`) and its normal (`down`), in units of the inter-ocular distance.
fn profile_centres(points: &[[f32; 2]; 5]) -> [[f32; 2]; 9] {
    let [le, re, nose, lm, rm] = *points;
    let iod = iod(points);
    let right = [(re[0] - le[0]) / iod, (re[1] - le[1]) / iod];
    let down = [-right[1], right[0]];
    let at = |p: [f32; 2], r: f32, d: f32| {
        [p[0] + (r * right[0] + d * down[0]) * iod, p[1] + (r * right[1] + d * down[1]) * iod]
    };
    let eyes = [(le[0] + re[0]) / 2.0, (le[1] + re[1]) / 2.0];
    let mouth = [(lm[0] + rm[0]) / 2.0, (lm[1] + rm[1]) / 2.0];
    [
        nose,
        [(le[0] + lm[0]) / 2.0, (le[1] + lm[1]) / 2.0],
        [(re[0] + rm[0]) / 2.0, (re[1] + rm[1]) / 2.0],
        at(eyes, 0.0, -0.6),
        at(mouth, 0.0, 0.45),
        at(le, -0.55, 0.0),
        at(re, 0.55, 0.0),
        at(le, 0.0, -0.25),
        at(re, 0.0, -0.25),
    ]
}

/// The emitter image's mean over each patch divided by the mean of the patches present.
pub fn profile(lit: &GrayImage, ambient: &GrayImage, points: &[[f32; 2]; 5]) -> [Option<f32>; 9] {
    let half = 0.1 * iod(points);
    let means = profile_centres(points).map(|c| patch(lit, ambient, c, half));
    let present: Vec<f32> = means.iter().flatten().copied().collect();
    let mean = present.iter().sum::<f32>() / present.len().max(1) as f32;
    means.map(|m| m.filter(|_| mean > 0.0).map(|m| m / mean))
}

impl Check for Shape {
    fn name(&self) -> &str {
        "shape"
    }

    fn higher_is_live(&self) -> bool {
        false
    }

    fn score(&mut self, frame: &Frame, face: usize) -> Result<Option<f32>> {
        let pair = &frame.current.capture.pair;
        let profile = profile(&pair.lit, &pair.ambient, &points(frame.face(face)));
        let z: Vec<f32> =
            profile.iter().zip(GENUINE_PROFILE).filter_map(|(p, (mu, sd))| Some(((*p)? - mu) / sd)).collect();
        if z.is_empty() {
            return Ok(None);
        }
        Ok(Some((z.iter().map(|z| z * z).sum::<f32>() / z.len() as f32).sqrt()))
    }
}

/// Flicker fingerprint: a lamp on mains flickers at 100/120 Hz and a screen refreshes at
/// 60–120 Hz; a rolling-shutter camera turns either into bands whose phase drifts from frame
/// to frame, so the mean brightness of a region wanders over the pairs of a session. A face
/// and the wall behind it are lit by the same lamps and wander together; a screen held up as
/// the face carries its own refresh pattern, and a print its own only if it is lit by another
/// lamp than the background. Score = over the ambient IR frames and the RGB frames of the
/// face's track, the standard deviation of the difference between the face region's and the
/// background's relative change from one pair to the next (the larger of the two frames'
/// values); needs [`FLICKER_PAIRS`] pairs and an ambient frame that is not blank.
pub struct Flicker;

/// Pairs the series needs (three changes).
pub const FLICKER_PAIRS: usize = 4;
/// The background is the frame outside the face box grown by this fraction per side.
const BACKGROUND_MARGIN: f32 = 0.5;
/// Every n-th pixel in both directions is read for the region means.
const REGION_STEP: u32 = 4;

/// Mean over the face box and over the background of an image of `width` × `height` read
/// through `at`; the background mean is `None` when the grown box covers the frame.
fn regions(width: u32, height: u32, bbox: &[f32; 4], at: impl Fn(u32, u32) -> f32) -> (f32, Option<f32>) {
    let [x1, y1, x2, y2] = *bbox;
    let (w, h) = (x2 - x1, y2 - y1);
    let clamp = |v: f32, max: u32| (v.max(0.0) as u32).min(max);
    let face = [clamp(x1, width), clamp(y1, height), clamp(x2, width), clamp(y2, height)];
    let grown = [
        clamp(x1 - BACKGROUND_MARGIN * w, width),
        clamp(y1 - BACKGROUND_MARGIN * h, height),
        clamp(x2 + BACKGROUND_MARGIN * w, width),
        clamp(y2 + BACKGROUND_MARGIN * h, height),
    ];
    let inside = |r: &[u32; 4], x, y| x >= r[0] && x < r[2] && y >= r[1] && y < r[3];
    let (mut face_sum, mut face_n, mut back_sum, mut back_n) = (0.0, 0u32, 0.0, 0u32);
    for y in (0..height).step_by(REGION_STEP as usize) {
        for x in (0..width).step_by(REGION_STEP as usize) {
            if inside(&face, x, y) {
                face_sum += at(x, y);
                face_n += 1;
            } else if !inside(&grown, x, y) {
                back_sum += at(x, y);
                back_n += 1;
            }
        }
    }
    (face_sum / face_n.max(1) as f32, (back_n > 0).then(|| back_sum / back_n as f32))
}

/// Luminance of an RGB pixel (Rec. 601).
fn luma(rgb: &RgbImage, x: u32, y: u32) -> f32 {
    let [r, g, b] = rgb.get_pixel(x, y).0;
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}

/// Face and background means of the ambient frame and of the RGB frame of one pair.
fn pair_regions(o: &Observed, face: usize) -> (Option<(f32, f32)>, Option<(f32, f32)>) {
    let f = &o.check.faces[face];
    let ir = o.capture.pair.ambient_image().and_then(|a| {
        let (face, back) = regions(a.width(), a.height(), &f.analysis.face.bbox, |x, y| a.get_pixel(x, y).0[0] as f32);
        Some((face, back?))
    });
    let rgb = o.capture.rgb.as_ref().zip(f.rgb_face.as_ref()).and_then(|(image, face)| {
        let (face, back) = regions(image.width(), image.height(), &face.bbox, |x, y| luma(image, x, y));
        Some((face, back?))
    });
    (ir, rgb)
}

/// Standard deviation of (face relative change − background relative change) over consecutive
/// samples; `None` with fewer than [`FLICKER_PAIRS`] samples or a region reading 0.
fn mismatch(series: &[(f32, f32)]) -> Option<f32> {
    if series.len() < FLICKER_PAIRS || series.iter().any(|(f, b)| *f <= 0.0 || *b <= 0.0) {
        return None;
    }
    let changes: Vec<f32> = series.windows(2).map(|w| (w[1].0 / w[0].0 - 1.0) - (w[1].1 / w[0].1 - 1.0)).collect();
    let mean = changes.iter().sum::<f32>() / changes.len() as f32;
    Some((changes.iter().map(|c| (c - mean) * (c - mean)).sum::<f32>() / changes.len() as f32).sqrt())
}

impl Check for Flicker {
    fn name(&self) -> &str {
        "flicker"
    }

    fn higher_is_live(&self) -> bool {
        false
    }

    fn score(&mut self, frame: &Frame, face: usize) -> Result<Option<f32>> {
        let mut pairs: Vec<(&Observed, usize)> = frame.history(face).collect();
        pairs.reverse();
        pairs.push((frame.current, face));
        let (mut ir, mut rgb) = (Vec::new(), Vec::new());
        for (o, i) in pairs {
            let (a, b) = pair_regions(o, i);
            ir.extend(a);
            rgb.extend(b);
        }
        Ok(match (mismatch(&ir), mismatch(&rgb)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        })
    }
}

/// IR/RGB material: skin reflects 850 nm at ≈ 0.5–0.6 and visible light at ≈ 0.3–0.4; paper
/// reflects both at ≈ 0.8–0.9 and most inks are transparent at 850 nm, so a printed face is as
/// dark as skin in the colour frame (that is what the print is for) but paper-white to the
/// emitter; a screen emits visible light and no infrared. Score = the face's emitter gain
/// (F14 `d`, on the F43 block), corrected for the emitter's 1/r² by (IOD / [`IOD_REF`])²,
/// over the luminance of the colour face's F43 block (Rec. 601, on the colour face's own five
/// points). Both cameras auto-expose, so the ratio is not an absolute albedo ratio; on the
/// genuine café recordings it reads 0.44–0.51 with the RGB face at 190–220 of 255. `None`
/// without a colour face or when the colour face is dark ([`MIN_RGB_FACE`]: a dark room says
/// nothing about the material).
pub struct Material;

/// Inter-ocular distance (px) at which the emitter gain is taken as is (≈ 60 cm on this camera).
pub const IOD_REF: f32 = 50.0;
/// Colour face luminance below which the ratio is not meaningful.
pub const MIN_RGB_FACE: f32 = 40.0;

impl Check for Material {
    fn name(&self) -> &str {
        "material"
    }

    fn score(&mut self, frame: &Frame, face: usize) -> Result<Option<f32>> {
        let f = frame.face(face);
        let (Some(lighting), Some(rgb), Some(rgb_face)) =
            (f.analysis.lighting, &frame.current.capture.rgb, &f.rgb_face)
        else {
            return Ok(None);
        };
        let luminance = block_mean(rgb.width(), rgb.height(), &rgb_face.landmarks, |x, y| luma(rgb, x, y));
        if luminance < MIN_RGB_FACE {
            return Ok(None);
        }
        let scale = iod(&points(f)) / IOD_REF;
        Ok(Some(lighting.gain() * scale * scale / luminance))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTS: [[f32; 2]; 5] = [[60.0, 80.0], [120.0, 80.0], [90.0, 110.0], [70.0, 140.0], [110.0, 140.0]];

    fn frame(value: u8, dots: &[([u32; 2], u8)]) -> GrayImage {
        let mut image = GrayImage::from_pixel(200, 200, image::Luma([value]));
        for ([x, y], v) in dots {
            image.put_pixel(*x, *y, image::Luma([*v]));
        }
        image
    }

    #[test]
    fn glint_is_a_small_spot_near_the_eye_that_the_ambient_frame_lacks() {
        let radius = (GLINT_RADIUS * iod(&POINTS)).round() as i64;
        assert_eq!(radius, 9);
        let flat = frame(100, &[]);
        assert_eq!(spot(&flat, &frame(0, &[]), POINTS[0], radius), 0.0);
        // A dot 2 px from the eye centre: 250 over a ring of 100.
        let lit = frame(100, &[([62, 82], 250)]);
        assert_eq!(spot(&lit, &frame(0, &[]), POINTS[0], radius), 150.0);
        // The same dot in the ambient frame is the room's, not the emitter's.
        assert_eq!(spot(&lit, &frame(0, &[([62, 82], 150)]), POINTS[0], radius), 0.0);
        // A dot outside the eye window is not a glint.
        assert_eq!(spot(&frame(100, &[([90, 110], 250)]), &frame(0, &[]), POINTS[0], radius), 0.0);
    }

    #[test]
    fn profile_is_flat_on_an_even_print_and_skips_patches_off_the_frame() {
        let flat = profile(&frame(100, &[]), &frame(0, &[]), &POINTS);
        assert!(flat.iter().all(|p| (p.unwrap() - 1.0).abs() < 1e-6));
        // Eyes at the bottom edge: the chin patch (0.45 IOD below the mouth) is off the frame.
        let low = POINTS.map(|[x, y]| [x, y + 60.0]);
        let cut = profile(&frame(100, &[]), &frame(0, &[]), &low);
        assert!(cut[4].is_none() && cut[0].is_some());
        // Dark temples and a bright nose, as the emitter lights a face: closer to the genuine profile.
        let lit = GrayImage::from_fn(200, 200, |x, _| image::Luma([if (40..140).contains(&x) { 130 } else { 50 }]));
        let face = profile(&lit, &frame(0, &[]), &POINTS);
        let z = |p: &[Option<f32>; 9]| {
            let z: Vec<f32> = p.iter().zip(GENUINE_PROFILE).map(|(p, (mu, sd))| (p.unwrap() - mu) / sd).collect();
            (z.iter().map(|z| z * z).sum::<f32>() / 9.0).sqrt()
        };
        assert!(z(&face) < z(&flat), "{} < {}", z(&face), z(&flat));
    }

    #[test]
    fn flicker_mismatch_is_the_face_wandering_apart_from_the_background() {
        // Face and background flicker together: no mismatch.
        let together: Vec<(f32, f32)> = [1.0, 1.1, 0.9, 1.05, 1.0].iter().map(|&k| (50.0 * k, 10.0 * k)).collect();
        assert!(mismatch(&together).unwrap() < 1e-6);
        // The face carries its own pattern.
        let apart = [(50.0, 10.0), (55.0, 10.0), (45.0, 10.0), (52.0, 10.0)];
        assert!(mismatch(&apart).unwrap() > 0.1);
        assert_eq!(mismatch(&apart[..3]), None);
        assert_eq!(mismatch(&[(0.0, 10.0), (1.0, 10.0), (1.0, 10.0), (1.0, 10.0)]), None);
        // Region means: a 10×10 face box at (20, 20) in a 100×100 frame, background outside the 20×20 grown box.
        let image = GrayImage::from_fn(100, 100, |x, y| {
            image::Luma([if (20..30).contains(&x) && (20..30).contains(&y) { 200 } else { 20 }])
        });
        let (face, back) = regions(100, 100, &[20.0, 20.0, 30.0, 30.0], |x, y| image.get_pixel(x, y).0[0] as f32);
        assert_eq!((face, back), (200.0, Some(20.0)));
        assert_eq!(regions(100, 100, &[-100.0, -100.0, 200.0, 200.0], |_, _| 1.0).1, None);
    }
}
