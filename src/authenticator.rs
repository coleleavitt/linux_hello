//! Camera handling around the pipeline: open the cameras, vote over frame pairs, return one
//! verdict.

use anyhow::{bail, Result};
use image::RgbImage;
use tracing::{debug, info};

use crate::liveness::MAX_PAIRS;
use crate::{
    Camera, CameraType, Embedding, FacePipeline, FrameCheck, IrCamera, IrPair, LivenessSession, LivenessState,
    PrintMatch, Verdict, Vote, MATCH_THRESHOLD,
};

const LIVENESS_BUFFERS: u32 = 2;
const MAX_DARK_SKIPS: usize = 4;
const DARK_RATIO: f32 = 0.5;
/// Enrollment reads at most this many frame pairs per print it needs.
const ENROLL_PAIRS_PER_PRINT: usize = 4;

/// Mean pixel value over a sparse sample of the image.
fn brightness(image: &RgbImage) -> f32 {
    let raw = image.as_raw();
    let sample: Vec<u8> = raw.iter().step_by(97).copied().collect();
    sample.iter().map(|&v| v as f32).sum::<f32>() / sample.len().max(1) as f32
}

pub struct CameraSetup {
    /// IR camera used for recognition
    pub device: String,
    /// uvcvideo metadata node of the IR camera, for the per-frame illumination flag. Without it,
    /// the brighter of two consecutive frames is taken as the lit one.
    pub ir_metadata: Option<String>,
    /// RGB camera for the liveness models (they only work on visible light)
    pub liveness_device: Option<String>,
}

/// One IR frame pair and, if configured, the RGB frame captured right after it.
pub struct Capture {
    pub pair: IrPair,
    pub rgb: Option<RgbImage>,
}

/// Cameras opened for one authentication. The emitter and privacy LED stay off between requests.
pub struct OpenCameras {
    ir: IrCamera,
    liveness: Option<Camera>,
    /// Running mean brightness of accepted RGB frames
    rgb_level: Option<f32>,
}

impl OpenCameras {
    pub fn open(setup: &CameraSetup) -> Result<Self> {
        // Open RGB first: starting it after the IR stream stalls it. Reading one frame starts
        // the stream (v4l starts streaming on the first read); its auto-exposure then keeps
        // adjusting while the IR camera warms up, so afterwards we only drop the frames it
        // buffered meanwhile.
        let mut liveness = setup
            .liveness_device
            .as_ref()
            .map(|d| Camera::open_with(d, CameraType::Rgb, 1, LIVENESS_BUFFERS))
            .transpose()?;
        let ir = IrCamera::open(&setup.device, setup.ir_metadata.as_deref())?;
        if let Some(cam) = liveness.as_mut() {
            cam.skip(LIVENESS_BUFFERS as usize)?;
        }
        Ok(Self { ir, liveness, rgb_level: None })
    }

    pub fn capture(&mut self) -> Result<Capture> {
        let pair = self.ir.capture_pair()?;
        let rgb = self.capture_liveness()?;
        Ok(Capture { pair, rgb })
    }

    /// The RGB camera underexposes 2-3 frames about every 5 s, and the liveness models reject
    /// those. Skip frames much darker than the recent average (up to a few in a row).
    fn capture_liveness(&mut self) -> Result<Option<RgbImage>> {
        let Some(cam) = self.liveness.as_mut() else { return Ok(None) };
        let mut frame = cam.capture_rgb()?;
        for _ in 0..MAX_DARK_SKIPS {
            let level = brightness(&frame);
            match self.rgb_level {
                Some(avg) if level < DARK_RATIO * avg => {
                    debug!(level, avg, "skipping underexposed RGB frame");
                    frame = cam.capture_rgb()?;
                }
                _ => {
                    self.rgb_level = Some(self.rgb_level.map_or(level, |avg| 0.8 * avg + 0.2 * level));
                    break;
                }
            }
        }
        Ok(Some(frame))
    }
}

/// The unlock decision for one authentication: accept once the liveness session is live and at
/// least one live frame matched the user's template.
#[derive(Default)]
pub struct Attempt {
    session: LivenessSession,
    /// The best-matching live frame so far
    matched: Option<(Embedding, PrintMatch)>,
    /// Best similarity over live frames, matched or not
    best_similarity: Option<f32>,
    saw_face: bool,
}

impl Attempt {
    /// Count one frame pair; returns the verdict once there is one.
    pub fn add(&mut self, check: &FrameCheck) -> Option<Verdict> {
        let state = self.session.add(check.vote);
        self.saw_face |= check.face.is_some();
        if let (Vote::Live, Some(print), Some(best)) = (check.vote, &check.print, check.best) {
            self.best_similarity = Some(self.best_similarity.map_or(best.similarity, |s| s.max(best.similarity)));
            let better = self.matched.as_ref().is_none_or(|(_, m)| best.similarity > m.similarity);
            if best.similarity > MATCH_THRESHOLD && better {
                self.matched = Some((print.clone(), best));
            }
        }
        match (state, &self.matched) {
            (LivenessState::Spoof, _) => Some(Verdict::NotLive),
            (LivenessState::Live, Some((_, m))) => Some(Verdict::Match { similarity: m.similarity }),
            _ if self.session.pairs() >= MAX_PAIRS => Some(self.give_up()),
            _ => None,
        }
    }

    pub fn state(&self) -> LivenessState {
        self.session.state()
    }

    /// The best-matching live frame's print and the stored print it matched.
    pub fn matched(&self) -> Option<(&Embedding, PrintMatch)> {
        self.matched.as_ref().map(|(print, m)| (print, *m))
    }

    fn give_up(&self) -> Verdict {
        match (self.session.state(), self.best_similarity) {
            (LivenessState::Live, Some(similarity)) => Verdict::NoMatch { similarity },
            (LivenessState::Live, None) => Verdict::NotEnrolled,
            _ if self.saw_face => Verdict::NotLive,
            _ => Verdict::NoFace,
        }
    }
}

/// One line per frame pair: face brightness lit/ambient, emitter gain, gate, RGB P(real), vote,
/// similarity.
pub fn describe(check: &FrameCheck) -> String {
    let num = |v: Option<f32>, digits: usize| v.map_or("-".to_string(), |v| format!("{v:.digits$}"));
    let l = check.lighting;
    let gate = match l {
        None => "no-face",
        Some(l) if l.is_good() => "good",
        Some(_) => "bad",
    };
    format!(
        "lit={} ambient={} d={} gate={gate} rgb_real={} vote={:?} similarity={}",
        num(l.map(|l| l.lit), 1),
        num(l.map(|l| l.ambient), 1),
        num(l.map(|l| l.gain()), 1),
        num(check.liveness, 3),
        check.vote,
        num(check.best.map(|b| b.similarity), 3),
    )
}

pub struct Authenticator {
    pub pipeline: FacePipeline,
    pub cameras: CameraSetup,
}

impl Authenticator {
    /// Vote over up to [`MAX_PAIRS`] frame pairs. On a match, add the best live frame to the
    /// template (F31) and save the store.
    pub fn verify(&mut self, user: &str) -> Result<Verdict> {
        if !self.pipeline.is_enrolled(user) {
            return Ok(Verdict::NotEnrolled);
        }
        let mut cams = OpenCameras::open(&self.cameras)?;
        let mut attempt = Attempt::default();
        let verdict = loop {
            let capture = cams.capture()?;
            let check = self.pipeline.check(user, &capture.pair, capture.rgb.as_ref())?;
            let verdict = attempt.add(&check);
            info!(user, state = ?attempt.state(), "{}", describe(&check));
            if let Some(verdict) = verdict {
                break verdict;
            }
        };
        drop(cams);
        if let (Verdict::Match { .. }, Some((print, matched))) = (verdict, attempt.matched()) {
            let added = self.pipeline.augment(user, print, matched)?;
            info!(user, added, generation = matched.generation + 1, "template augmentation");
        }
        Ok(verdict)
    }

    /// Enroll `frames` prints from frame pairs that show a face with good IR lighting.
    pub fn enroll(&mut self, user: &str, frames: usize) -> Result<()> {
        let mut ir = IrCamera::open(&self.cameras.device, self.cameras.ir_metadata.as_deref())?;
        let mut prints = Vec::new();
        for _ in 0..frames * ENROLL_PAIRS_PER_PRINT {
            if prints.len() == frames {
                break;
            }
            let analysis = self.pipeline.analyze(&ir.capture_pair()?)?;
            match analysis.print {
                Some(print) => prints.push(print),
                None => info!(face = analysis.face.is_some(), lighting = ?analysis.lighting, "enrollment frame skipped"),
            }
        }
        drop(ir);
        if prints.len() < frames {
            bail!(
                "only {} of {frames} frame pairs showed a face with good IR lighting (lit − ambient 45–205, ambient ≤ 25)",
                prints.len()
            );
        }
        self.pipeline.enroll(user, prints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Face;
    use crate::lighting::Lighting;

    fn check(vote: Vote, similarity: f32) -> FrameCheck {
        FrameCheck {
            face: Some(Face { bbox: [0.0; 4], score: 1.0, landmarks: [[0.0; 2]; 5] }),
            lighting: Some(Lighting { lit: 80.0, ambient: 5.0 }),
            liveness: None,
            vote,
            print: Some(Embedding { features: vec![similarity] }),
            best: Some(PrintMatch { similarity, generation: 1 }),
        }
    }

    #[test]
    fn accepts_once_live_and_a_live_frame_matched() {
        let mut a = Attempt::default();
        assert_eq!(a.add(&check(Vote::Live, 0.9)), None);
        assert_eq!(a.add(&check(Vote::Live, 0.6)), Some(Verdict::Match { similarity: 0.9 }));
        assert_eq!(a.matched().unwrap().1.generation, 1);
    }

    #[test]
    fn spoof_frames_do_not_match() {
        let mut a = Attempt::default();
        a.add(&check(Vote::Spoof, 0.9));
        a.add(&check(Vote::Live, 0.1));
        assert_eq!(a.add(&check(Vote::Live, 0.2)), None);
        assert!(a.matched().is_none());
        assert_eq!(a.add(&check(Vote::Live, 0.8)), Some(Verdict::Match { similarity: 0.8 }));
    }

    #[test]
    fn live_stranger_is_no_match_after_max_pairs() {
        let mut a = Attempt::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(a.add(&check(Vote::Live, 0.1)), None);
        }
        assert_eq!(a.add(&check(Vote::Live, 0.2)), Some(Verdict::NoMatch { similarity: 0.2 }));
    }
}
