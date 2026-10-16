//! Camera handling around the pipeline: open the cameras, vote over frame pairs, return one
//! verdict.

use anyhow::{bail, Result};
use image::RgbImage;
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::camera::{self, Rotation};
use crate::lid;
use crate::matcher::MIN_PRINTS;
use crate::pipeline::Analysis;
use crate::{
    Camera, CameraType, Embedding, Face, FacePipeline, FaceProblem, FaceTracker, FrameCheck, IrCamera, IrPair,
    LightingStatus, LivenessSession, LivenessState, PrintMatch, Verdict, Vote,
};

const LIVENESS_BUFFERS: u32 = 2;
/// How long to wait for an RGB frame. The RGB stream's first frame comes ≈ 50 ms after the IR
/// stream has started (≈ 245 ms when it starts first; docs/capture-and-liveness.md §1). Once,
/// early on, the RGB camera never delivered a frame and the unlock hung; after this long the
/// session goes on without RGB, so the colour net is skipped as Windows skips a classifier whose
/// input is missing (F25 status 6/7).
const RGB_FRAME_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_DARK_SKIPS: usize = 4;
const DARK_RATIO: f32 = 0.5;
/// Prints one enrollment collects (F54: 16 per enrolment in the v5 parameter block).
pub const ENROLL_PRINTS: usize = 16;
/// Enrollment stops collecting after this long and keeps what it has.
pub const ENROLL_TIMEOUT: Duration = Duration::from_secs(10);
/// A print this similar (cosine) to one already collected is the same view of the face and does
/// not count, so 16 frames of a face held still do not make 16 prints. Consecutive lit frames of
/// a still sitter score 0.96–0.99 on this camera (`validation_frames`).
pub const DUPLICATE_PRINT: f32 = 0.98;

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
    /// Rotation that turns both cameras' frames upright (F10)
    pub rotation: Rotation,
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
        // IR first: alone, its stream delivers the first frame in ≈ 90 ms. The RGB stream's
        // first frame takes ≈ 245 ms when it starts first and ≈ 50 ms after the IR stream
        // (docs/capture-and-liveness.md §1).
        let mut ir = IrCamera::open(&setup.device, setup.ir_metadata.as_deref())?;
        ir.rotate(setup.rotation);
        let liveness =
            setup.liveness_device.as_deref().map(|d| Self::open_rgb(d, setup.rotation)).transpose()?.flatten();
        Ok(Self { ir, liveness, rgb_level: None })
    }

    /// The RGB camera after its first frame, or `None` if that frame did not come within
    /// [`RGB_FRAME_TIMEOUT`].
    fn open_rgb(device: &str, rotation: Rotation) -> Result<Option<Camera>> {
        let mut camera = Camera::open_with(device, CameraType::Rgb, 0, LIVENESS_BUFFERS)?;
        camera.rotate(rotation);
        camera.set_timeout(RGB_FRAME_TIMEOUT);
        without_rgb_on_timeout(camera.skip(1).map(|()| camera), "no first RGB frame")
    }

    pub fn capture(&mut self) -> Result<Capture> {
        let pair = self.ir.capture_pair()?;
        let rgb = self.capture_liveness()?;
        Ok(Capture { pair, rgb })
    }

    /// Meter the IR exposure on the face found in the last pair ([`IrCamera::expose_on`]).
    pub fn expose_on(&mut self, face: Option<&Face>) {
        if let Some(face) = face {
            self.ir.expose_on(face.bbox);
        }
    }

    /// The RGB camera underexposes 2-3 frames about every 5 s, and the liveness models reject
    /// those. Skip frames much darker than the recent average (up to a few in a row).
    /// A camera that stops delivering frames ([`RGB_FRAME_TIMEOUT`]) is dropped for the session.
    fn capture_liveness(&mut self) -> Result<Option<RgbImage>> {
        let Some(cam) = self.liveness.as_mut() else { return Ok(None) };
        let frame = (|| {
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
            Ok(frame)
        })();
        let frame = without_rgb_on_timeout(frame, "RGB frame stalled")?;
        if frame.is_none() {
            self.liveness = None;
        }
        Ok(frame)
    }
}

/// `Ok(None)` with a warning when the RGB capture timed out, so that the session goes on without
/// the RGB camera; any other error is passed on.
fn without_rgb_on_timeout<T>(result: Result<T>, what: &str) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if camera::timed_out(&e) => {
            warn!(timeout = ?RGB_FRAME_TIMEOUT, "{what}: continuing without the RGB camera (no colour-net veto)");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Frame pairs read per authentication before giving up. Windows stops after 48 frames or when
/// its counter passes 16 (F25); one pair is two camera frames.
pub const MAX_PAIRS: u32 = 16;
/// Consecutive occluded frames that end an attempt still identifying the face (F46,
/// `IdentifyAsGuest(3)`): a mask or a hand will not go away within the pair budget.
pub const OCCLUDED_GIVE_UP: u32 = 4;

/// One authentication: up to [`MAX_PAIRS`] frame pairs, once more after a re-attempt (F49). The
/// largest face is tracked across pairs and each track gets its own [`Attempt`] (F37, F47), so the
/// prints and votes of one face never count for another (the owner's face, then a photo held up
/// in its place).
#[derive(Default)]
pub struct Session {
    tracker: FaceTracker,
    /// Track of the current attempt
    track: Option<u32>,
    attempt: Attempt,
    /// What the tracked face did over the attempt's frames
    motion: FaceMotion,
    /// Consecutive occluded frames of the track
    occluded: u32,
    /// Pairs since the session, or the re-attempt, started
    pairs: u32,
    reattempted: bool,
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
            self.motion = FaceMotion::default();
            self.occluded = 0;
        }
        if let Some(face) = &check.analysis.face {
            self.motion.add(face, check.analysis.frame, check.analysis.lighting.map(|l| l.lit));
        }
        self.occluded = if check.analysis.problem == Some(FaceProblem::Occluded) { self.occluded + 1 } else { 0 };
        if self.occluded >= OCCLUDED_GIVE_UP && !self.attempt.recognised() {
            return Some(Verdict::NoFace { problem: FaceProblem::Occluded });
        }
        let verdict =
            self.attempt.add(check).or_else(|| (self.pairs >= MAX_PAIRS).then(|| self.attempt.give_up(&self.problems)));
        match verdict {
            Some(Verdict::NoMatch { .. } | Verdict::NotLive) if !self.reattempted && self.motion.changed() => {
                info!(?verdict, moved = self.motion.moved(), exposure = self.motion.exposure_changed(), "re-attempt");
                self.reattempted = true;
                self.pairs = 0;
                self.attempt = Attempt::default();
                None
            }
            verdict => verdict,
        }
    }

    /// Whether the re-attempt (F49) has been used.
    pub fn reattempted(&self) -> bool {
        self.reattempted
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

/// Re-attempt thresholds (`sub_1800575E0`, F49): a face whose normalised centre ranged over
/// ((Δx)² + (Δy)²) / 2 > 0.01 (≈ 0.14 of the frame) or whose box width changed by more than 20 %
/// has moved; a face whose last 10 lit brightness values (F43) have σ > 12 saw its exposure change.
const REATTEMPT_CENTRE_RANGE: f32 = 0.01;
const REATTEMPT_SIZE_CHANGE: f32 = 0.2;
const REATTEMPT_BRIGHTNESS_SIGMA: f32 = 12.0;
const BRIGHTNESS_WINDOW: usize = 10;

/// Movement and exposure of the tracked face over an attempt, for the one re-attempt a session
/// gets before it ends in `NOMATCH` or `NOTLIVE` (F49): the face may have been recognised had
/// it held still, or the camera may have been adjusting its exposure. Windows also skips the
/// re-attempt after too many saturated frames; linux_hello has no saturated-pixel count (F44
/// status 6/7 are not implemented), so that condition is left out.
#[derive(Default)]
struct FaceMotion {
    /// Range of the box centre, normalised by the frame width and height
    centre: Option<Range<[f32; 2]>>,
    width: Option<Range<f32>>,
    /// The last [`BRIGHTNESS_WINDOW`] lit face brightness values
    brightness: Vec<f32>,
}

struct Range<T> {
    min: T,
    max: T,
}

impl FaceMotion {
    fn add(&mut self, face: &Face, frame: [u32; 2], lit: Option<f32>) {
        let [x1, y1, x2, y2] = face.bbox;
        let centre = [(x1 + x2) / 2.0 / frame[0] as f32, (y1 + y2) / 2.0 / frame[1] as f32];
        match &mut self.centre {
            Some(r) => {
                r.min = [r.min[0].min(centre[0]), r.min[1].min(centre[1])];
                r.max = [r.max[0].max(centre[0]), r.max[1].max(centre[1])];
            }
            None => self.centre = Some(Range { min: centre, max: centre }),
        }
        let width = x2 - x1;
        match &mut self.width {
            Some(r) => {
                r.min = r.min.min(width);
                r.max = r.max.max(width);
            }
            None => self.width = Some(Range { min: width, max: width }),
        }
        if let Some(lit) = lit {
            if self.brightness.len() == BRIGHTNESS_WINDOW {
                self.brightness.remove(0);
            }
            self.brightness.push(lit);
        }
    }

    fn changed(&self) -> bool {
        self.moved() || self.exposure_changed()
    }

    /// `ReAttemptRecognitionByMovement`
    fn moved(&self) -> bool {
        let centre = self.centre.as_ref().is_some_and(|r| {
            let (dx, dy) = (r.max[0] - r.min[0], r.max[1] - r.min[1]);
            (dx * dx + dy * dy) / 2.0 > REATTEMPT_CENTRE_RANGE
        });
        let size = self.width.as_ref().is_some_and(|r| r.max > r.min * (1.0 + REATTEMPT_SIZE_CHANGE));
        centre || size
    }

    /// `ReAttemptRecognitionByExposure`
    fn exposure_changed(&self) -> bool {
        let n = self.brightness.len() as f32;
        if n < 2.0 {
            return false;
        }
        let mean = self.brightness.iter().sum::<f32>() / n;
        let variance = self.brightness.iter().map(|b| (b - mean) * (b - mean)).sum::<f32>() / n;
        variance.sqrt() > REATTEMPT_BRIGHTNESS_SIGMA
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
            cams.expose_on(check.analysis.face.as_ref());
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

    /// Enroll up to `wanted` prints from frame pairs that pass every frame gate with good lighting,
    /// collected over a few seconds while the user looks at the camera and moves naturally (F54:
    /// Windows takes 16 prints per enrolment). A print within [`DUPLICATE_PRINT`] of one already
    /// collected does not count. Collection stops at `wanted` or after [`ENROLL_TIMEOUT`]; a new
    /// template needs at least [`MIN_PRINTS`] prints, `add` (Windows' "improve recognition") at
    /// least one, on top of the existing template up to [`crate::matcher::MAX_PRINTS`]. Unless
    /// `force`, refuse a face that matches another enrolled user. `progress` is called with
    /// (collected, wanted) after every new print.
    pub fn enroll(
        &mut self,
        user: &str,
        wanted: usize,
        force: bool,
        add: bool,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Enrolled> {
        let (mut collected, wanted, needed) = if add {
            let Some(template) = self.pipeline.template(user) else {
                bail!("{user} is not enrolled; enroll without --add first")
            };
            if template.room() == 0 {
                bail!("{user}'s template already holds {} prints; enroll again without --add", template.prints.len());
            }
            let existing: Vec<Embedding> =
                template.prints.iter().map(|p| Embedding { features: p.features.clone() }).collect();
            (existing, wanted.min(template.room()), 1)
        } else {
            (Vec::new(), wanted, MIN_PRINTS as usize)
        };
        let existing = collected.len();
        let mut skipped: BTreeMap<&'static str, u32> = BTreeMap::new();
        let mut ir = IrCamera::open(&self.cameras.device, self.cameras.ir_metadata.as_deref())?;
        ir.rotate(self.cameras.rotation);
        self.pipeline.set_enrolling(true);
        let start = Instant::now();
        while collected.len() - existing < wanted && start.elapsed() < ENROLL_TIMEOUT {
            let analysis = self.pipeline.analyze(&ir.capture_pair()?)?;
            if let Some(face) = &analysis.face {
                ir.expose_on(face.bbox);
            }
            let lighting = analysis.lighting;
            match take_print(&mut collected, analysis) {
                None => progress(collected.len() - existing, wanted),
                Some(reason) => {
                    debug!(reason, ?lighting, "enrollment frame skipped");
                    *skipped.entry(reason).or_default() += 1;
                }
            }
        }
        let elapsed = start.elapsed();
        drop(ir);
        self.pipeline.set_enrolling(false);
        let prints: Vec<Embedding> = collected.drain(existing..).collect();
        if prints.len() < needed {
            bail!(
                "only {} of {wanted} prints collected in {:.1} s (need {needed}); skipped frames: {} \
                 (a usable face is ≥ 72 px, away from the edges, not covered, frontal, lit 35–205, \
                 lit − ambient ≥ 45, ambient ≤ 25; move your head a little between prints)",
                prints.len(),
                elapsed.as_secs_f32(),
                describe_skipped(&skipped),
            );
        }
        let (stored, total) = self.pipeline.enroll(user, prints, force, add)?;
        Ok(Enrolled { stored, wanted, total, skipped, elapsed })
    }
}

/// Add the print of one enrollment frame to `collected`, if the frame has good lighting and
/// shows a view of the face not yet collected ([`DUPLICATE_PRINT`]); otherwise the reason it was
/// skipped.
fn take_print(collected: &mut Vec<Embedding>, analysis: Analysis) -> Option<&'static str> {
    match (analysis.status(), analysis.print) {
        (Some(LightingStatus::Good), Some(print)) => {
            if collected.iter().any(|p| p.similarity(&print) > DUPLICATE_PRINT) {
                return Some("near_duplicate");
            }
            collected.push(print);
            None
        }
        (_, Some(_)) => Some("weak_lighting"),
        (_, None) => Some(analysis.problem.map_or("no_face", FaceProblem::as_str)),
    }
}

/// What one `enroll` run did.
pub struct Enrolled {
    /// Prints stored by this run
    pub stored: usize,
    pub wanted: usize,
    /// Prints in the template afterwards
    pub total: usize,
    /// Frames that gave no print, by reason: `near_duplicate`, `weak_lighting` or a
    /// [`FaceProblem`] word
    pub skipped: BTreeMap<&'static str, u32>,
    pub elapsed: Duration,
}

/// `near_duplicate 12, too_dark 3`, or `none`.
pub fn describe_skipped(skipped: &BTreeMap<&'static str, u32>) -> String {
    if skipped.is_empty() {
        return "none".to_string();
    }
    skipped.iter().map(|(reason, n)| format!("{reason} {n}")).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lighting::Lighting;
    use crate::Face;

    const FRAME: [u32; 2] = [640, 360];
    const OWNER: [f32; 4] = [200.0, 100.0, 300.0, 230.0];
    const PHOTO: [f32; 4] = [400.0, 120.0, 480.0, 220.0];

    fn face(bbox: [f32; 4]) -> Option<Face> {
        Some(Face { bbox, score: 1.0, landmarks: [[0.0; 2]; 5] })
    }

    /// A usable pair of the face at `bbox` with good (`live`) or weak lighting, matching at
    /// `similarity` against a 15-print template (FAR from the shipped table: 0.50 ↔ 1e-5).
    fn usable_at(bbox: [f32; 4], vote: Vote, similarity: f32) -> FrameCheck {
        let ambient = if vote == Vote::Abstain { 60.0 } else { 5.0 };
        let far = crate::calibration::lfw_gray().far(similarity, 15);
        FrameCheck {
            analysis: Analysis {
                frame: FRAME,
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

    /// The owner's box shifted right by `dx` pixels.
    fn shifted(dx: f32) -> [f32; 4] {
        [OWNER[0] + dx, OWNER[1], OWNER[2] + dx, OWNER[3]]
    }

    fn rejected_at(bbox: [f32; 4], problem: FaceProblem) -> FrameCheck {
        let face = if problem == FaceProblem::NoFace { None } else { face(bbox) };
        FrameCheck {
            analysis: Analysis {
                frame: FRAME,
                face,
                windows_face: None,
                lighting: None,
                problem: Some(problem),
                print: None,
            },
            liveness: None,
            windows: None,
            vote: Vote::Abstain,
            best: None,
        }
    }

    fn rejected(problem: FaceProblem) -> FrameCheck {
        rejected_at(OWNER, problem)
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
    fn reattempts_once_when_the_face_moved() {
        // A stranger drifting 10 px right per pair: 150 px over the session, 0.23 of the frame
        // width, so the centre range (0.23² / 2 = 0.027) is above 0.01. The track holds (box
        // overlap ≥ 0.3 from pair to pair).
        let mut s = Session::default();
        for i in 0..MAX_PAIRS {
            assert_eq!(s.add(&usable_at(shifted(10.0 * i as f32), Vote::Live, 0.3)), None);
        }
        assert!(s.reattempted());
        assert_eq!(s.attempt().prints, 0, "fresh counters");
        // The second budget ends the session, however much the face moves.
        for i in 1..MAX_PAIRS {
            assert_eq!(s.add(&usable_at(shifted(150.0 + 10.0 * i as f32), Vote::Live, 0.3)), None);
        }
        assert_eq!(s.add(&usable_at(shifted(310.0), Vote::Live, 0.3)), Some(Verdict::NoMatch { similarity: 0.3 }));

        // A face coming 25 % closer (box width 100 → 125) has moved too.
        let mut closer = Session::default();
        for i in 0..MAX_PAIRS {
            let grow = 25.0 * i as f32 / (MAX_PAIRS - 1) as f32;
            let bbox = [OWNER[0], OWNER[1], OWNER[2] + grow, OWNER[3] + grow];
            assert_eq!(closer.add(&usable_at(bbox, Vote::Live, 0.3)), None);
        }
        assert!(closer.reattempted());
    }

    #[test]
    fn reattempts_once_when_the_exposure_changed() {
        // Still face, but the lit brightness swings between 60 and 100 (σ = 20 > 12).
        let swinging = |i: u32| {
            let mut check = usable(Vote::Live, 0.3);
            check.analysis.lighting = Some(Lighting { lit: if i % 2 == 0 { 60.0 } else { 100.0 }, ambient: 5.0 });
            check
        };
        let mut s = Session::default();
        for i in 0..MAX_PAIRS {
            assert_eq!(s.add(&swinging(i)), None);
        }
        assert!(s.reattempted());
        // Only the last 10 values count: steady brightness since then is no exposure change.
        let mut settled = Session::default();
        for i in 0..5 {
            settled.add(&swinging(i));
        }
        for _ in 5..MAX_PAIRS - 1 {
            settled.add(&usable(Vote::Live, 0.3));
        }
        assert_eq!(settled.add(&usable(Vote::Live, 0.3)), Some(Verdict::NoMatch { similarity: 0.3 }));
    }

    #[test]
    fn reattempt_covers_notlive_but_not_noface() {
        // Recognised, then three spoof votes while the face moved: the track starts over instead
        // of NOTLIVE, and is not recognised any more.
        let mut s = Session::default();
        for i in 0..MIN_PRINTS {
            s.add(&usable_at(shifted(40.0 * i as f32), Vote::Abstain, 0.9));
        }
        assert!(s.attempt().recognised());
        for _ in 0..2 {
            assert_eq!(s.add(&usable_at(shifted(160.0), Vote::Spoof, 0.9)), None);
        }
        assert_eq!(s.add(&usable_at(shifted(160.0), Vote::Spoof, 0.9)), None);
        assert!(s.reattempted() && !s.attempt().recognised());
        // A second NOTLIVE is final.
        for _ in 0..MIN_PRINTS {
            s.add(&usable_at(shifted(160.0), Vote::Abstain, 0.9));
        }
        for _ in 0..2 {
            assert_eq!(s.add(&usable_at(shifted(160.0), Vote::Spoof, 0.9)), None);
        }
        assert_eq!(s.add(&usable_at(shifted(160.0), Vote::Spoof, 0.9)), Some(Verdict::NotLive));

        // Too dark all along, moving: no print, so no recognition to re-attempt.
        let mut dark = Session::default();
        for i in 1..MAX_PAIRS {
            assert_eq!(dark.add(&rejected_at(shifted(10.0 * i as f32), FaceProblem::TooDark)), None);
        }
        let last = dark.add(&rejected_at(shifted(160.0), FaceProblem::TooDark));
        assert_eq!(last, Some(Verdict::NoFace { problem: FaceProblem::TooDark }));
        assert!(!dark.reattempted());
    }

    #[test]
    fn enrollment_takes_good_frames_with_a_new_view_only() {
        let good = |features: &[f32]| {
            let mut check = usable(Vote::Live, 0.3);
            check.analysis.print = Some(Embedding { features: features.to_vec() });
            check.analysis
        };
        let mut collected = Vec::new();
        assert_eq!(take_print(&mut collected, good(&[1.0, 0.0])), None);
        // Cosine 0.99 to the first print: the same view again.
        assert_eq!(take_print(&mut collected, good(&[0.99, 0.141])), Some("near_duplicate"));
        // Cosine 0.97: a new view.
        assert_eq!(take_print(&mut collected, good(&[0.97, 0.243])), None);
        assert_eq!(collected.len(), 2);
        // Weak lighting has a print but is not enrolled from; rejected frames name their reason.
        assert_eq!(take_print(&mut collected, usable(Vote::Abstain, 0.3).analysis), Some("weak_lighting"));
        assert_eq!(take_print(&mut collected, rejected(FaceProblem::TooDark).analysis), Some("too_dark"));
        assert_eq!(take_print(&mut collected, rejected(FaceProblem::NoFace).analysis), Some("no_face"));
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn four_consecutive_occluded_frames_give_up_while_identifying() {
        let occluded = || rejected(FaceProblem::Occluded);
        let mut s = Session::default();
        for _ in 1..OCCLUDED_GIVE_UP {
            assert_eq!(s.add(&occluded()), None);
        }
        assert_eq!(s.add(&occluded()), Some(Verdict::NoFace { problem: FaceProblem::Occluded }));

        // A clear frame in between starts the count over.
        let mut broken = Session::default();
        for _ in 1..OCCLUDED_GIVE_UP {
            broken.add(&occluded());
        }
        assert_eq!(broken.add(&usable(Vote::Live, 0.3)), None);
        for _ in 1..OCCLUDED_GIVE_UP {
            assert_eq!(broken.add(&occluded()), None);
        }

        // Once recognised, occluded frames only run out the budget (no vote).
        let mut recognised = Session::default();
        for _ in 0..MIN_PRINTS {
            recognised.add(&usable(Vote::Abstain, 0.9));
        }
        for _ in 0..OCCLUDED_GIVE_UP + 2 {
            assert_eq!(recognised.add(&occluded()), None);
        }
        assert!(recognised.attempt().recognised());
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

    #[test]
    fn rgb_timeout_drops_the_camera_other_errors_fail() {
        use anyhow::Context;
        let timeout = || Err::<(), _>(std::io::Error::from(std::io::ErrorKind::TimedOut)).context("VIDIOC_DQBUF");
        assert!(matches!(without_rgb_on_timeout(timeout(), "test"), Ok(None)));
        assert!(matches!(without_rgb_on_timeout(Ok(1), "test"), Ok(Some(1))));
        let other = Err::<(), _>(std::io::Error::from(std::io::ErrorKind::NotFound)).context("VIDIOC_DQBUF");
        assert!(without_rgb_on_timeout(other, "test").is_err());
    }
}
