//! Camera handling around the pipeline: open the cameras, vote over frame pairs, return one
//! verdict.

use anyhow::{bail, Result};
use image::RgbImage;
use std::collections::HashMap;
use tracing::{debug, info};

use crate::matcher::MIN_PRINTS;
use crate::{
    Camera, CameraType, Embedding, FacePipeline, FaceProblem, FrameCheck, IrCamera, IrPair, LightingStatus,
    LivenessSession, LivenessState, PrintMatch, Verdict, Vote, MATCH_THRESHOLD,
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

/// Frame pairs read per authentication before giving up. Windows stops after 48 frames or when
/// its counter passes 16 (F25); one pair here is two camera frames.
pub const MAX_PAIRS: u32 = 16;

/// The unlock decision for one authentication (`MakeDecisions`, F47, F48): the user is
/// recognised once the session has [`MIN_PRINTS`] prints and the best one matches; liveness
/// votes count from that frame on; 2 live votes accept, 3 spoof votes reject.
#[derive(Default)]
pub struct Attempt {
    pairs: u32,
    prints: u32,
    /// Best similarity over the session's prints
    best: Option<f32>,
    recognised: bool,
    session: LivenessSession,
    /// The best-matching print of a live-voting frame, the candidate for augmentation
    live_best: Option<(Embedding, PrintMatch)>,
    problems: HashMap<FaceProblem, u32>,
}

impl Attempt {
    /// Count one frame pair; returns the verdict once there is one.
    pub fn add(&mut self, check: &FrameCheck) -> Option<Verdict> {
        self.pairs += 1;
        let analysis = &check.analysis;
        if let Some(problem) = analysis.problem {
            *self.problems.entry(problem).or_default() += 1;
        }
        if let Some(print) = &analysis.print {
            self.prints += 1;
            if let Some(m) = check.best {
                self.best = Some(self.best.map_or(m.similarity, |s| s.max(m.similarity)));
                let better = self.live_best.as_ref().is_none_or(|(_, b)| m.similarity > b.similarity);
                if check.vote == Vote::Live && better {
                    self.live_best = Some((print.clone(), m));
                }
            }
        }
        if !self.recognised {
            self.recognised = self.prints >= MIN_PRINTS && self.best.is_some_and(|s| s > MATCH_THRESHOLD);
        }
        let state = if self.recognised { self.session.add(check.vote) } else { LivenessState::Pending };
        match state {
            LivenessState::Live => Some(Verdict::Match { similarity: self.best.expect("recognised") }),
            LivenessState::Spoof => Some(Verdict::NotLive),
            LivenessState::Pending if self.pairs >= MAX_PAIRS => Some(self.give_up()),
            LivenessState::Pending => None,
        }
    }

    pub fn recognised(&self) -> bool {
        self.recognised
    }

    pub fn liveness(&self) -> LivenessState {
        self.session.state()
    }

    /// The print to add to the template after a match: the best-matching live frame, only when
    /// it is a borderline match (F53).
    pub fn augmentation(&self) -> Option<(&Embedding, PrintMatch)> {
        self.live_best.as_ref().filter(|(_, m)| m.is_borderline()).map(|(print, m)| (print, *m))
    }

    fn give_up(&self) -> Verdict {
        if self.recognised {
            return Verdict::NotLive;
        }
        if self.prints >= MIN_PRINTS {
            return self.best.map_or(Verdict::NotEnrolled, |similarity| Verdict::NoMatch { similarity });
        }
        let problem = self.problems.iter().max_by_key(|(_, n)| **n).map_or(FaceProblem::NoFace, |(p, _)| *p);
        Verdict::NoFace { problem }
    }
}

/// One line per frame pair: face brightness lit/ambient, emitter gain, lighting status or
/// rejection reason, RGB P(real), vote, similarity.
pub fn describe(check: &FrameCheck) -> String {
    let num = |v: Option<f32>, digits: usize| v.map_or("-".to_string(), |v| format!("{v:.digits$}"));
    let a = &check.analysis;
    let l = a.lighting;
    let status = match (a.problem, a.status()) {
        (Some(p), _) => p.as_str().to_string(),
        (None, Some(s)) => format!("{s:?}").to_lowercase(),
        (None, None) => "-".to_string(),
    };
    format!(
        "lit={} ambient={} d={} status={status} rgb_real={} windows={} vote={:?} similarity={}",
        num(l.map(|l| l.lit), 1),
        num(l.map(|l| l.ambient), 1),
        num(l.map(|l| l.gain()), 1),
        num(check.liveness, 3),
        check.windows.as_ref().map_or("-".to_string(), |w| w.total.to_string()),
        check.vote,
        num(check.best.map(|b| b.similarity), 3),
    )
}

pub struct Authenticator {
    pub pipeline: FacePipeline,
    pub cameras: CameraSetup,
}

impl Authenticator {
    /// Read frame pairs until [`Attempt`] decides. On a match, add the best live frame to the
    /// template if it was a borderline match (F53) and save the store.
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
            info!(user, recognised = attempt.recognised(), liveness = ?attempt.liveness(), "{}", describe(&check));
            if let Some(verdict) = verdict {
                break verdict;
            }
        };
        drop(cams);
        if let (Verdict::Match { .. }, Some((print, matched))) = (verdict, attempt.augmentation()) {
            let added = self.pipeline.augment(user, print, matched)?;
            info!(user, added, similarity = matched.similarity, generation = matched.generation + 1, "template augmentation");
        }
        Ok(verdict)
    }

    /// Enroll `frames` prints from frame pairs that pass every frame gate with good lighting.
    pub fn enroll(&mut self, user: &str, frames: usize) -> Result<()> {
        let mut ir = IrCamera::open(&self.cameras.device, self.cameras.ir_metadata.as_deref())?;
        self.pipeline.set_enrolling(true);
        let mut prints = Vec::new();
        for _ in 0..frames * ENROLL_PAIRS_PER_PRINT {
            if prints.len() == frames {
                break;
            }
            let analysis = self.pipeline.analyze(&ir.capture_pair()?)?;
            match (analysis.status(), analysis.print) {
                (Some(LightingStatus::Good), Some(print)) => prints.push(print),
                _ => info!(problem = ?analysis.problem, lighting = ?analysis.lighting, "enrollment frame skipped"),
            }
        }
        drop(ir);
        self.pipeline.set_enrolling(false);
        if prints.len() < frames {
            bail!(
                "only {} of {frames} frame pairs showed a usable face with good IR lighting \
                 (face ≥ 72 px, away from the edges, not covered, lit 35–205, lit − ambient ≥ 45, ambient ≤ 25)",
                prints.len()
            );
        }
        self.pipeline.enroll(user, prints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::Lighting;
    use crate::pipeline::Analysis;
    use crate::Face;

    fn face() -> Option<Face> {
        Some(Face { bbox: [0.0; 4], score: 1.0, landmarks: [[0.0; 2]; 5] })
    }

    /// A usable pair with good (`live`) or weak lighting, matching at `similarity`.
    fn usable(vote: Vote, similarity: f32) -> FrameCheck {
        let ambient = if vote == Vote::Abstain { 60.0 } else { 5.0 };
        FrameCheck {
            analysis: Analysis {
                face: face(),
                lighting: Some(Lighting { lit: 80.0, ambient }),
                problem: None,
                print: Some(Embedding { features: vec![similarity] }),
            },
            liveness: None,
            windows: None,
            vote,
            best: Some(PrintMatch { similarity, generation: 1 }),
        }
    }

    fn rejected(problem: FaceProblem) -> FrameCheck {
        FrameCheck {
            analysis: Analysis { face: face(), lighting: None, problem: Some(problem), print: None },
            liveness: None,
            windows: None,
            vote: Vote::Abstain,
            best: None,
        }
    }

    #[test]
    fn recognises_after_min_prints_then_counts_live_votes() {
        let mut a = Attempt::default();
        for _ in 1..MIN_PRINTS {
            assert_eq!(a.add(&usable(Vote::Live, 0.9)), None);
        }
        assert!(!a.recognised());
        // Votes before recognition do not count: this frame recognises and is the first live vote.
        assert_eq!(a.add(&usable(Vote::Live, 0.7)), None);
        assert!(a.recognised());
        assert_eq!(a.add(&usable(Vote::Live, 0.6)), Some(Verdict::Match { similarity: 0.9 }));
    }

    #[test]
    fn three_spoof_votes_after_recognition_reject() {
        let mut a = Attempt::default();
        for _ in 0..MIN_PRINTS {
            a.add(&usable(Vote::Abstain, 0.9));
        }
        assert!(a.recognised());
        assert_eq!(a.add(&usable(Vote::Spoof, 0.9)), None);
        assert_eq!(a.add(&usable(Vote::Spoof, 0.9)), None);
        assert_eq!(a.add(&usable(Vote::Spoof, 0.9)), Some(Verdict::NotLive));
    }

    #[test]
    fn gives_up_after_max_pairs() {
        let mut stranger = Attempt::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(stranger.add(&usable(Vote::Live, 0.1)), None);
        }
        assert_eq!(stranger.add(&usable(Vote::Live, 0.3)), Some(Verdict::NoMatch { similarity: 0.3 }));

        // Recognised, but weak lighting all along: never live.
        let mut weak = Attempt::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(weak.add(&usable(Vote::Abstain, 0.9)), None);
        }
        assert_eq!(weak.add(&usable(Vote::Abstain, 0.9)), Some(Verdict::NotLive));

        // No usable face: the most frequent reason is reported.
        let mut dark = Attempt::default();
        let verdicts: Vec<_> = (1..=MAX_PAIRS)
            .map(|i| dark.add(&rejected(if i % 3 == 0 { FaceProblem::NoFace } else { FaceProblem::TooDark })))
            .collect();
        assert!(verdicts[..MAX_PAIRS as usize - 1].iter().all(Option::is_none));
        assert_eq!(verdicts.last().unwrap(), &Some(Verdict::NoFace { problem: FaceProblem::TooDark }));
    }

    #[test]
    fn augments_only_borderline_live_matches() {
        let session = |similarity| {
            let mut a = Attempt::default();
            for _ in 0..MIN_PRINTS {
                a.add(&usable(Vote::Abstain, 0.9));
            }
            a.add(&usable(Vote::Live, similarity));
            a
        };
        assert!(session(0.52).augmentation().is_some());
        assert!(session(0.60).augmentation().is_none());
        assert!(session(0.45).augmentation().is_none());
    }
}
