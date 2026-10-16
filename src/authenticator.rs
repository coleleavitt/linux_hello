//! Camera handling around the pipeline: open the cameras, vote over frame pairs, return one
//! verdict.

use anyhow::{bail, Result};
use image::RgbImage;
use std::collections::HashMap;
use tracing::{debug, info};

use crate::lid;
use crate::matcher::MIN_PRINTS;
use crate::{
    Camera, CameraType, Embedding, FacePipeline, FaceProblem, FaceTracker, FrameCheck, IrCamera, IrPair,
    LightingStatus, LivenessSession, LivenessState, PrintMatch, Verdict, Vote,
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
/// its counter passes 16 (F25); one pair is two camera frames.
pub const MAX_PAIRS: u32 = 16;

/// One authentication: up to [`MAX_PAIRS`] frame pairs. The largest face is tracked across pairs
/// and each track gets its own [`Attempt`] (F37, F47), so the prints and votes of one face never
/// count for another (the owner's face, then a photo held up in its place).
#[derive(Default)]
pub struct Session {
    tracker: FaceTracker,
    /// Track of the current attempt
    track: Option<u32>,
    attempt: Attempt,
    pairs: u32,
    /// Frame rejection reasons over the whole session, for `NOFACE <reason>`
    problems: HashMap<FaceProblem, u32>,
}

impl Session {
    /// Count one frame pair; returns the verdict once there is one.
    pub fn add(&mut self, check: &FrameCheck) -> Option<Verdict> {
        self.pairs += 1;
        if let Some(problem) = check.analysis.problem {
            *self.problems.entry(problem).or_default() += 1;
        }
        let track = self.tracker.update(check.analysis.face.as_ref());
        if track.is_some() && track != self.track {
            if self.track.is_some() {
                debug!(?track, "new face track: starting over");
            }
            self.track = track;
            self.attempt = Attempt::default();
        }
        self.attempt.add(check).or_else(|| (self.pairs >= MAX_PAIRS).then(|| self.attempt.give_up(&self.problems)))
    }

    /// Id of the tracked face the current attempt belongs to.
    pub fn track(&self) -> Option<u32> {
        self.track
    }

    /// The attempt of the current track.
    pub fn attempt(&self) -> &Attempt {
        &self.attempt
    }

    /// The enroll-on-failure candidate (F31) of a session that saw only one face: Windows aborts
    /// enroll-on-failure when other faces were seen (`AbortingSinceOtherFacesWereSeenDuringTrackedSession`).
    pub fn failed_print(&self) -> Option<&Embedding> {
        if self.track != Some(1) {
            return None;
        }
        self.attempt.failed_print()
    }
}

/// The unlock decision for one tracked face (`MakeDecisions`, F47, F48): the user is recognised
/// once the face has [`MIN_PRINTS`] prints and the best one matches (estimated FAR under the
/// target, F27); liveness votes count from that frame on; 2 live votes accept, 3 spoof votes reject.
#[derive(Default)]
pub struct Attempt {
    prints: u32,
    /// Best match over the face's prints
    best: Option<PrintMatch>,
    recognised: bool,
    session: LivenessSession,
    /// Every vote of the face, also before recognition: whether a face that is never recognised
    /// is real (`FaceNotDeemedAsARealFace`)
    votes: LivenessSession,
    /// The best-matching print of a live-voting frame, the candidate for augmentation and for
    /// enroll-on-failure
    live_best: Option<(Embedding, PrintMatch)>,
}

impl Attempt {
    /// Count one frame pair of this face; returns the verdict once liveness decides.
    pub fn add(&mut self, check: &FrameCheck) -> Option<Verdict> {
        if let Some(print) = &check.analysis.print {
            self.prints += 1;
            if let Some(m) = check.best {
                if self.best.is_none_or(|b| m.similarity > b.similarity) {
                    self.best = Some(m);
                }
                let better = self.live_best.as_ref().is_none_or(|(_, b)| m.similarity > b.similarity);
                if check.vote == Vote::Live && better {
                    self.live_best = Some((print.clone(), m));
                }
            }
        }
        self.votes.add(check.vote);
        if !self.recognised {
            self.recognised = self.prints >= MIN_PRINTS && self.best.is_some_and(|m| m.recognised());
        }
        let state = if self.recognised { self.session.add(check.vote) } else { LivenessState::Pending };
        match state {
            LivenessState::Live => Some(Verdict::Match { similarity: self.best.expect("recognised").similarity }),
            LivenessState::Spoof => Some(Verdict::NotLive),
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

    /// The print to add after a failed attempt that the password then confirms (F31): the
    /// best-matching print of a live-voting (good lighting) frame of a face that was not
    /// recognised but would be live by its votes.
    pub fn failed_print(&self) -> Option<&Embedding> {
        let real = self.votes.state() == LivenessState::Live;
        self.live_best.as_ref().filter(|_| real && !self.recognised).map(|(print, _)| print)
    }

    /// The verdict when the session runs out of frame pairs; `problems` are the session's frame
    /// rejection reasons.
    fn give_up(&self, problems: &HashMap<FaceProblem, u32>) -> Verdict {
        if self.recognised {
            return Verdict::NotLive;
        }
        if self.prints >= MIN_PRINTS {
            return self.best.map_or(Verdict::NotEnrolled, |m| Verdict::NoMatch { similarity: m.similarity });
        }
        let problem = problems.iter().max_by_key(|(_, n)| **n).map_or(FaceProblem::NoFace, |(p, _)| *p);
        Verdict::NoFace { problem }
    }
}

/// One line per frame pair: face brightness lit/ambient, emitter gain, lighting status or
/// rejection reason, RGB P(real), vote, similarity and its estimated FAR.
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
        "lit={} ambient={} d={} status={status} rgb_real={} windows={} vote={:?} similarity={} far={}",
        num(l.map(|l| l.lit), 1),
        num(l.map(|l| l.ambient), 1),
        num(l.map(|l| l.gain()), 1),
        num(check.liveness, 3),
        check.windows.as_ref().map_or("-".to_string(), |w| w.total.to_string()),
        check.vote,
        num(check.best.map(|b| b.similarity), 3),
        check.best.map_or("-".to_string(), |b| format!("{:.1e}", b.far)),
    )
}

/// What [`Authenticator::verify`] found.
pub struct Outcome {
    pub verdict: Verdict,
    /// After `NOMATCH`: the print that enroll-on-failure may add once the password confirms the
    /// user ([`Session::failed_print`])
    pub failed_print: Option<Embedding>,
}

impl From<Verdict> for Outcome {
    fn from(verdict: Verdict) -> Self {
        Self { verdict, failed_print: None }
    }
}

pub struct Authenticator {
    pub pipeline: FacePipeline,
    pub cameras: CameraSetup,
}

impl Authenticator {
    /// Refuse when the lid is closed over a built-in camera. Otherwise read frame pairs until the
    /// [`Session`] decides. On a match, add the best live frame of the matched face to the
    /// template if it was a borderline match (F53) and save the store. After `NOMATCH`, return
    /// the session's enroll-on-failure candidate.
    pub fn verify(&mut self, user: &str) -> Result<Outcome> {
        if lid::lid_closed() && lid::built_in(&self.cameras.device) {
            return Ok(Verdict::LidClosed.into());
        }
        if !self.pipeline.is_enrolled(user) {
            return Ok(Verdict::NotEnrolled.into());
        }
        let mut cams = OpenCameras::open(&self.cameras)?;
        let mut session = Session::default();
        let verdict = loop {
            let capture = cams.capture()?;
            let check = self.pipeline.check(user, &capture.pair, capture.rgb.as_ref())?;
            let verdict = session.add(&check);
            let attempt = session.attempt();
            info!(
                user,
                track = ?session.track(),
                recognised = attempt.recognised(),
                liveness = ?attempt.liveness(),
                "{}",
                describe(&check)
            );
            if let Some(verdict) = verdict {
                break verdict;
            }
        };
        drop(cams);
        if let (Verdict::Match { .. }, Some((print, matched))) = (verdict, session.attempt().augmentation()) {
            let added = self.pipeline.augment(user, print, matched)?;
            info!(
                user,
                added,
                similarity = matched.similarity,
                far = format!("{:.1e}", matched.far),
                generation = matched.generation + 1,
                "template augmentation"
            );
        }
        let failed_print = match verdict {
            Verdict::NoMatch { .. } => session.failed_print().cloned(),
            _ => None,
        };
        Ok(Outcome { verdict, failed_print })
    }

    /// Enroll `frames` prints from frame pairs that pass every frame gate with good lighting.
    /// Unless `force`, refuse a face that matches another enrolled user.
    pub fn enroll(&mut self, user: &str, frames: usize, force: bool) -> Result<()> {
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
        self.pipeline.enroll(user, prints, force)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::Lighting;
    use crate::pipeline::Analysis;
    use crate::Face;

    const OWNER: [f32; 4] = [200.0, 100.0, 300.0, 230.0];
    const PHOTO: [f32; 4] = [400.0, 120.0, 480.0, 220.0];

    fn face(bbox: [f32; 4]) -> Option<Face> {
        Some(Face { bbox, score: 1.0, landmarks: [[0.0; 2]; 5] })
    }

    /// A usable pair of the face at `bbox` with good (`live`) or weak lighting, matching at
    /// `similarity` (FAR from the shipped table: 0.50 ↔ 1e-5).
    fn usable_at(bbox: [f32; 4], vote: Vote, similarity: f32) -> FrameCheck {
        let ambient = if vote == Vote::Abstain { 60.0 } else { 5.0 };
        let far = crate::calibration::lfw_gray().far(similarity);
        FrameCheck {
            analysis: Analysis {
                face: face(bbox),
                windows_face: None,
                lighting: Some(Lighting { lit: 80.0, ambient }),
                problem: None,
                print: Some(Embedding { features: vec![similarity] }),
            },
            liveness: None,
            windows: None,
            vote,
            best: Some(PrintMatch { similarity, far, generation: 1 }),
        }
    }

    fn usable(vote: Vote, similarity: f32) -> FrameCheck {
        usable_at(OWNER, vote, similarity)
    }

    fn rejected(problem: FaceProblem) -> FrameCheck {
        let face = if problem == FaceProblem::NoFace { None } else { face(OWNER) };
        FrameCheck {
            analysis: Analysis { face, windows_face: None, lighting: None, problem: Some(problem), print: None },
            liveness: None,
            windows: None,
            vote: Vote::Abstain,
            best: None,
        }
    }

    #[test]
    fn recognises_after_min_prints_then_counts_live_votes() {
        let mut s = Session::default();
        for _ in 1..MIN_PRINTS {
            assert_eq!(s.add(&usable(Vote::Live, 0.9)), None);
        }
        assert!(!s.attempt().recognised());
        // Votes before recognition do not count: this frame recognises and is the first live vote.
        assert_eq!(s.add(&usable(Vote::Live, 0.7)), None);
        assert!(s.attempt().recognised());
        assert_eq!(s.add(&usable(Vote::Live, 0.6)), Some(Verdict::Match { similarity: 0.9 }));
    }

    #[test]
    fn three_spoof_votes_after_recognition_reject() {
        let mut s = Session::default();
        for _ in 0..MIN_PRINTS {
            s.add(&usable(Vote::Abstain, 0.9));
        }
        assert!(s.attempt().recognised());
        assert_eq!(s.add(&usable(Vote::Spoof, 0.9)), None);
        assert_eq!(s.add(&usable(Vote::Spoof, 0.9)), None);
        assert_eq!(s.add(&usable(Vote::Spoof, 0.9)), Some(Verdict::NotLive));
    }

    #[test]
    fn gives_up_after_max_pairs() {
        let mut stranger = Session::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(stranger.add(&usable(Vote::Live, 0.1)), None);
        }
        assert_eq!(stranger.add(&usable(Vote::Live, 0.3)), Some(Verdict::NoMatch { similarity: 0.3 }));

        // Recognised, but weak lighting all along: never live.
        let mut weak = Session::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(weak.add(&usable(Vote::Abstain, 0.9)), None);
        }
        assert_eq!(weak.add(&usable(Vote::Abstain, 0.9)), Some(Verdict::NotLive));

        // No usable face: the most frequent reason is reported.
        let mut dark = Session::default();
        let verdicts: Vec<_> = (1..=MAX_PAIRS)
            .map(|i| dark.add(&rejected(if i % 3 == 0 { FaceProblem::NoFace } else { FaceProblem::TooDark })))
            .collect();
        assert!(verdicts[..MAX_PAIRS as usize - 1].iter().all(Option::is_none));
        assert_eq!(verdicts.last().unwrap(), &Some(Verdict::NoFace { problem: FaceProblem::TooDark }));
    }

    #[test]
    fn prints_of_one_face_do_not_recognise_another() {
        // Three owner prints, then a photo elsewhere in the frame: counted over the session, the
        // photo's first print would be the 4th and the owner's 0.9 would recognise it, and its
        // live votes would unlock. Per track, the photo starts with no prints.
        let mut s = Session::default();
        for _ in 1..MIN_PRINTS {
            assert_eq!(s.add(&usable(Vote::Live, 0.9)), None);
        }
        let owner = s.track();
        for _ in 0..MIN_PRINTS + 2 {
            assert_eq!(s.add(&usable_at(PHOTO, Vote::Live, 0.1)), None);
        }
        assert_ne!(s.track(), owner);
        assert!(!s.attempt().recognised());
    }

    #[test]
    fn votes_of_one_face_do_not_count_for_another() {
        // The owner is recognised and casts one live vote; a different face then votes live.
        let mut s = Session::default();
        for _ in 0..MIN_PRINTS {
            assert_eq!(s.add(&usable(Vote::Live, 0.9)), None);
        }
        assert!(s.attempt().recognised());
        assert_eq!(s.attempt().liveness(), LivenessState::Pending);
        assert_eq!(s.add(&usable_at(PHOTO, Vote::Live, 0.3)), None);
        assert!(!s.attempt().recognised());
        // The pair limit counts the whole session; the verdict is the last face's.
        let rest: Vec<_> = (MIN_PRINTS + 2..MAX_PAIRS).map(|_| s.add(&usable_at(PHOTO, Vote::Live, 0.3))).collect();
        assert!(rest.iter().all(Option::is_none));
        assert_eq!(s.add(&usable_at(PHOTO, Vote::Live, 0.3)), Some(Verdict::NoMatch { similarity: 0.3 }));
    }

    #[test]
    fn failed_print_needs_one_real_unrecognised_face() {
        // Not recognised, two live votes: the best live print is the candidate.
        let mut s = Session::default();
        for similarity in [0.30, 0.45, 0.42, 0.35] {
            s.add(&usable(Vote::Live, similarity));
        }
        assert_eq!(s.failed_print().map(|p| p.features[0]), Some(0.45));

        // Weak lighting only: no live vote, not deemed real.
        let mut weak = Session::default();
        for _ in 0..MIN_PRINTS {
            weak.add(&usable(Vote::Abstain, 0.45));
        }
        assert!(weak.failed_print().is_none());

        // One live vote is not enough.
        let mut once = Session::default();
        once.add(&usable(Vote::Live, 0.45));
        once.add(&usable(Vote::Abstain, 0.45));
        assert!(once.failed_print().is_none());

        // A second face was seen: none, even for the first face coming back.
        let mut two = Session::default();
        two.add(&usable_at(PHOTO, Vote::Live, 0.45));
        for _ in 0..MIN_PRINTS {
            two.add(&usable(Vote::Live, 0.45));
        }
        assert!(two.attempt().failed_print().is_some());
        assert!(two.failed_print().is_none());

        // Recognised (then not live): not a recognition failure.
        let mut recognised = Session::default();
        for _ in 0..MIN_PRINTS {
            recognised.add(&usable(Vote::Live, 0.9));
        }
        assert!(recognised.failed_print().is_none());
    }

    #[test]
    fn augments_only_borderline_live_matches() {
        let session = |similarity| {
            let mut s = Session::default();
            for _ in 0..MIN_PRINTS {
                s.add(&usable(Vote::Abstain, 0.9));
            }
            s.add(&usable(Vote::Live, similarity));
            s
        };
        // FAR in [5e-6, 1e-5): similarity in (0.50, 0.52) with the shipped table.
        assert!(session(0.51).attempt().augmentation().is_some());
        assert!(session(0.60).attempt().augmentation().is_none());
        assert!(session(0.45).attempt().augmentation().is_none());
    }
}
