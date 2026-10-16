//! Physics-based liveness checks: each measures a physical difference between a face and a
//! print or a screen that infrared-rich room light cannot fake (docs/liveness-physics.md). They
//! are model-free [`Check`]s over what the pipeline found, scored on every face with a print
//! ([`crate::Authenticator`] logs them as `glint=0.8 shape=0.3 …`); a check vetoes a frame only
//! with a [`Check::threshold`], set where no genuine recording loses more than 1 % of its good
//! frames, and none has one yet (`VETO_THRESHOLDS`).

use anyhow::Result;
use image::GrayImage;

use crate::liveness::check::{Check, Frame};
use crate::{FaceCheck, Vote};

/// The checks, in the order they are logged.
pub fn all() -> Vec<Box<dyn Check>> {
    vec![Box::new(Glint)]
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
}
