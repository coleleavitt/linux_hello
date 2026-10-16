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
use crate::pipeline::{Analysis, FaceCheck};
use crate::{
    Camera, CameraType, Embedding, Face, FacePipeline, FaceProblem, FaceTracker, FrameCheck, IrCamera, IrPair,
    Lighting, LightingStatus, LivenessSession, LivenessState, PrintMatch, Verdict, Vote,
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

/// One authentication: up to [`MAX_PAIRS`] frame pairs, once more after a re-attempt (F49, once per
/// track). Every
/// analysed face is tracked across pairs by place and by print ([`FaceTracker`]) and each track
/// gets its own [`Attempt`] (F37, F47), so the prints and votes of one face never count for
/// another (the owner's face, then a photo held up next to it or in its place). The verdict comes
/// from the track of the user: the one recognised, or at the pair limit the one matching best.
#[derive(Default)]
pub struct Session {
    tracker: FaceTracker,
    /// Every track of the session by id
    tracks: BTreeMap<u32, Track>,
    /// Track ids of the faces of the last pair, in face order
    current: Vec<u32>,
    /// The track whose attempt gave the verdict
    decided: Option<u32>,
    /// Pairs since the session, or the last re-attempt, started
    pairs: u32,
    /// Rejection reasons of the largest face over the whole session, for `NOFACE <reason>`
    problems: HashMap<FaceProblem, u32>,
}

/// One tracked face's state over the session.
#[derive(Default)]
struct Track {
    attempt: Attempt,
    /// What the face did over the attempt's frames
    motion: FaceMotion,
    /// Consecutive occluded frames
    occluded: u32,
    /// Whether the track has had its re-attempt (F49: once per track)
    reattempted: bool,
}

impl Session {
    /// Count one frame pair; returns the verdict once there is one.
    pub fn add(&mut self, check: &FrameCheck) -> Option<Verdict> {
        self.pairs += 1;
        if let Some(problem) = check.largest().map_or(Some(FaceProblem::NoFace), |f| f.analysis.problem) {
            *self.problems.entry(problem).or_default() += 1;
        }
        let faces: Vec<_> = check.faces.iter().map(|f| (&f.analysis.face, f.analysis.print.as_ref())).collect();
        self.current = self.tracker.update(&faces);
        for (id, face) in self.current.iter().zip(&check.faces) {
            let track = self.tracks.entry(*id).or_insert_with(|| {
                debug!(id, "new face track");
                Track::default()
            });
            track.motion.add(&face.analysis.face, check.frame, face.analysis.lighting.as_ref());
            track.occluded = if face.analysis.problem == Some(FaceProblem::Occluded) { track.occluded + 1 } else { 0 };
            track.attempt.add(face);
        }
        // Every face of the pair occluded for OCCLUDED_GIVE_UP pairs while no face is recognised (F46).
        let occluded = self.current.iter().all(|id| self.tracks[id].occluded >= OCCLUDED_GIVE_UP);
        if !self.current.is_empty() && occluded && self.tracks.values().all(|t| !t.attempt.recognised()) {
            return Some(Verdict::NoFace { problem: FaceProblem::Occluded });
        }
        // A face is recognised only if no other face of the pair matches the user better (F48).
        let fars: Vec<Option<f64>> = self.current.iter().map(|id| self.tracks[id].attempt.far()).collect();
        let mut verdict = None;
        for (i, (id, face)) in self.current.iter().zip(&check.faces).enumerate() {
            let contested = fars[i].is_some_and(|mine| {
                fars.iter().enumerate().any(|(j, other)| j != i && other.is_some_and(|other| other < mine))
            });
            let track = self.tracks.get_mut(id).expect("inserted above");
            if let Some(v) = track.attempt.decide(face.vote, contested) {
                verdict = Some((*id, v));
                break;
            }
        }
        let verdict = verdict.or_else(|| (self.pairs >= MAX_PAIRS).then(|| self.give_up()));
        match verdict {
            Some((id, verdict @ (Verdict::NoMatch { .. } | Verdict::NotLive)))
                if !self.tracks[&id].reattempted && self.tracks[&id].motion.changed() =>
            {
                let track = self.tracks.get_mut(&id).expect("decided");
                let motion = &track.motion;
                info!(?verdict, track = id, moved = motion.moved(), exposure = motion.exposure_changed(), "re-attempt");
                track.reattempted = true;
                track.attempt = Attempt::default();
                self.pairs = 0;
                None
            }
            Some((id, verdict)) => {
                self.decided = Some(id);
                Some(verdict)
            }
            None => None,
        }
    }

    /// The verdict when the session runs out of frame pairs, from the track of the user: the
    /// recognised one, else the best match among the tracks with enough prints; without one,
    /// `NOFACE` with the most frequent rejection reason.
    fn give_up(&self) -> (u32, Verdict) {
        let deciding = self.tracks.iter().filter(|(_, t)| t.attempt.prints >= MIN_PRINTS).max_by(|(_, a), (_, b)| {
            a.attempt.rank().0.cmp(&b.attempt.rank().0).then(a.attempt.rank().1.total_cmp(&b.attempt.rank().1))
        });
        match deciding {
            Some((id, track)) => (*id, track.attempt.give_up()),
            None => {
                let problem = self.problems.iter().max_by_key(|(_, n)| **n).map_or(FaceProblem::NoFace, |(p, _)| *p);
                (*self.current.first().unwrap_or(&0), Verdict::NoFace { problem })
            }
        }
    }

    /// Whether any track has had its re-attempt (F49).
    pub fn reattempted(&self) -> bool {
        self.tracks.values().any(|t| t.reattempted)
    }

    /// The track of each face of the last pair, in face order, with its attempt.
    pub fn attempts(&self) -> impl Iterator<Item = (u32, &Attempt)> {
        self.current.iter().map(|id| (*id, &self.tracks[id].attempt))
    }

    /// The attempt of the track that gave the verdict.
    pub fn decided(&self) -> Option<&Attempt> {
        self.decided.map(|id| &self.tracks[&id].attempt)
    }

    /// The enroll-on-failure candidate (F31) of a session that saw only one face: Windows aborts
    /// enroll-on-failure when other faces were seen (`AbortingSinceOtherFacesWereSeenDuringTrackedSession`).
    pub fn failed_print(&self) -> Option<&Embedding> {
        match self.tracks.values().collect::<Vec<_>>()[..] {
            [only] => only.attempt.failed_print(),
            _ => None,
        }
    }
}

/// Re-attempt thresholds (`sub_1800575E0`, F49): a face whose normalised centre ranged over
/// ((Δx)² + (Δy)²) / 2 > 0.01 (≈ 0.14 of the frame) or whose box width changed by more than 20 %
/// has moved; a face whose last 10 lit brightness values (F43) have σ > 12 saw its exposure change.
const REATTEMPT_CENTRE_RANGE: f32 = 0.01;
const REATTEMPT_SIZE_CHANGE: f32 = 0.2;
const REATTEMPT_BRIGHTNESS_SIGMA: f32 = 12.0;
const BRIGHTNESS_WINDOW: usize = 10;
/// Clipped-face frames (F44 status 6) a track may have and still get its re-attempt (F49: "not
/// if the too-many-saturated counter > 1").
const MAX_CLIPPED_FRAMES: u32 = 1;

/// Movement and exposure of the tracked face over an attempt, for the one re-attempt a track
/// gets before it ends the session in `NOMATCH` or `NOTLIVE` (F49): the face may have been
/// recognised had it held still, or the camera may have been adjusting its exposure. Not after
/// more than [`MAX_CLIPPED_FRAMES`] frames with a clipped face (F44 status 6): the face is too
/// close or lit by a lamp, and that will not change by itself.
#[derive(Default)]
struct FaceMotion {
    /// Range of the box centre, normalised by the frame width and height
    centre: Option<Range<[f32; 2]>>,
    width: Option<Range<f32>>,
    /// The last [`BRIGHTNESS_WINDOW`] lit face brightness values
    brightness: Vec<f32>,
    /// Frames whose face ellipse was clipped (F44 status 6)
    clipped: u32,
}

struct Range<T> {
    min: T,
    max: T,
}

impl FaceMotion {
    fn add(&mut self, face: &Face, frame: [u32; 2], lighting: Option<&Lighting>) {
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
        if let Some(lighting) = lighting {
            if self.brightness.len() == BRIGHTNESS_WINDOW {
                self.brightness.remove(0);
            }
            self.brightness.push(lighting.lit);
            self.clipped += u32::from(lighting.status() == LightingStatus::Clipped);
        }
    }

    fn changed(&self) -> bool {
        self.clipped <= MAX_CLIPPED_FRAMES && (self.moved() || self.exposure_changed())
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
    /// Record one frame pair of this face; [`Self::decide`] then draws the conclusions.
    pub fn add(&mut self, check: &FaceCheck) {
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
    }

    /// `MakeDecisions` for this face after [`Self::add`]: the user is recognised once the face has
    /// [`MIN_PRINTS`] prints and its best print matches, unless `contested` (another tracked face
    /// matches the user better, F48); the frame's `vote` counts from the recognising frame on.
    /// Returns the verdict once liveness decides.
    pub fn decide(&mut self, vote: Vote, contested: bool) -> Option<Verdict> {
        if !self.recognised {
            self.recognised = !contested && self.prints >= MIN_PRINTS && self.best.is_some_and(|m| m.recognised());
        }
        let state = if self.recognised { self.session.add(vote) } else { LivenessState::Pending };
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

    /// Estimated FAR of the face's best print so far.
    pub fn far(&self) -> Option<f64> {
        self.best.map(|m| m.far)
    }

    /// Which face is the user's when none has unlocked: recognised first, then the best similarity.
    fn rank(&self) -> (bool, f32) {
        (self.recognised, self.best.map_or(f32::MIN, |m| m.similarity))
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

    /// The verdict when the session runs out of frame pairs with this face having
    /// [`MIN_PRINTS`] prints.
    fn give_up(&self) -> Verdict {
        if self.recognised {
            return Verdict::NotLive;
        }
        self.best.map_or(Verdict::NotEnrolled, |m| Verdict::NoMatch { similarity: m.similarity })
    }
}

/// One line per frame pair, one part per face: its track, face brightness lit/ambient, emitter
/// gain, lighting status or rejection reason, RGB P(real), vote, similarity and its estimated
/// FAR, Microsoft's distance and FAR when its recogniser gave a second opinion, whether the
/// track is recognised and its liveness state; `no_face` without a face.
pub fn describe(check: &FrameCheck, session: &Session) -> String {
    if check.faces.is_empty() {
        return "no_face".to_string();
    }
    let num = |v: Option<f32>, digits: usize| v.map_or("-".to_string(), |v| format!("{v:.digits$}"));
    let face = |check: &FaceCheck, (track, attempt): (u32, &Attempt)| {
        let a = &check.analysis;
        let l = a.lighting;
        let status = match (a.problem, a.status()) {
            (Some(p), _) => p.as_str().to_string(),
            (None, Some(s)) => format!("{s:?}").to_lowercase(),
            (None, None) => "-".to_string(),
        };
        format!(
            "track={track} lit={} ambient={} d={} status={status} rgb_real={} windows={} vote={:?} similarity={} \
             far={} ms={} ms_far={} recognised={} liveness={:?}",
            num(l.map(|l| l.lit), 1),
            num(l.map(|l| l.ambient), 1),
            num(l.map(|l| l.gain()), 1),
            num(check.liveness, 3),
            check.windows.as_ref().map_or("-".to_string(), |w| w.total.to_string()),
            check.vote,
            num(check.best.map(|b| b.similarity), 3),
            check.best.map_or("-".to_string(), |b| format!("{:.1e}", b.far)),
            num(check.best.and_then(|b| b.windows).map(|w| w.distance), 3),
            check.best.and_then(|b| b.windows).map_or("-".to_string(), |w| format!("{:.1e}", w.far)),
            attempt.recognised(),
            attempt.liveness(),
        )
    };
    check.faces.iter().zip(session.attempts()).map(|(c, a)| face(c, a)).collect::<Vec<_>>().join(" | ")
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
            cams.expose_on(check.largest().map(|f| &f.analysis.face));
            let verdict = session.add(&check);
            info!(user, "{}", describe(&check, &session));
            if let Some(verdict) = verdict {
                break verdict;
            }
        };
        drop(cams);
        let augmentation = session.decided().and_then(Attempt::augmentation);
        if let (Verdict::Match { .. }, Some((print, matched))) = (verdict, augmentation) {
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
                template.prints.iter().map(|p| Embedding { features: p.features.clone(), windows: None }).collect();
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
            let analysis = self.pipeline.analyze(&ir.capture_pair()?)?.into_iter().next();
            if let Some(a) = &analysis {
                ir.expose_on(a.face.bbox);
            }
            let lighting = analysis.as_ref().and_then(|a| a.lighting);
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

/// Add the print of one enrollment frame's largest face to `collected`, if the frame has good
/// lighting and shows a view of the face not yet collected ([`DUPLICATE_PRINT`]); otherwise the
/// reason it was skipped.
fn take_print(collected: &mut Vec<Embedding>, analysis: Option<Analysis>) -> Option<&'static str> {
    let Some(analysis) = analysis else { return Some("no_face") };
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
    use crate::Face;

    const FRAME: [u32; 2] = [640, 360];
    const OWNER: [f32; 4] = [200.0, 100.0, 300.0, 230.0];
    const PHOTO: [f32; 4] = [400.0, 120.0, 480.0, 220.0];

    fn face(bbox: [f32; 4]) -> Face {
        Face { bbox, score: 1.0, landmarks: [[0.0; 2]; 5] }
    }

    /// A pair of the faces given, largest first.
    fn pair(faces: Vec<FaceCheck>) -> FrameCheck {
        FrameCheck { frame: FRAME, faces }
    }

    /// A usable face at `bbox` with good (`live`) or weak lighting, matching at `similarity`
    /// against a 15-print template (FAR from the shipped table: 0.50 ↔ 1e-5). The print's first
    /// component tells the owner (1) from another person (−1): prints of one person score > 1 to
    /// each other, of two people < 0, around the tracker's 0.5; the second keeps the frame's
    /// similarity so a test can tell which print was picked.
    fn person(identity: f32, bbox: [f32; 4], vote: Vote, similarity: f32) -> FaceCheck {
        let ambient = if vote == Vote::Abstain { 60.0 } else { 5.0 };
        let far = crate::calibration::lfw_gray().far(similarity, 15);
        FaceCheck {
            analysis: Analysis {
                face: face(bbox),
                windows_face: None,
                lighting: Some(Lighting { lit: 80.0, ambient, clipped_face: 0.0, clipped_around: 0.0 }),
                problem: None,
                print: Some(Embedding { features: vec![identity, similarity], windows: None }),
            },
            liveness: None,
            windows: None,
            vote,
            best: Some(PrintMatch { similarity, far, generation: 1, windows: None }),
        }
    }

    /// The owner's face at `bbox`.
    fn owner(bbox: [f32; 4], vote: Vote, similarity: f32) -> FaceCheck {
        person(1.0, bbox, vote, similarity)
    }

    /// Someone else's face at `bbox`.
    fn stranger(bbox: [f32; 4], vote: Vote, similarity: f32) -> FaceCheck {
        person(-1.0, bbox, vote, similarity)
    }

    /// A pair with the owner's face at `bbox` alone.
    fn usable_at(bbox: [f32; 4], vote: Vote, similarity: f32) -> FrameCheck {
        pair(vec![owner(bbox, vote, similarity)])
    }

    fn usable(vote: Vote, similarity: f32) -> FrameCheck {
        usable_at(OWNER, vote, similarity)
    }

    /// A pair with someone else's face at `bbox` alone.
    fn stranger_at(bbox: [f32; 4], vote: Vote, similarity: f32) -> FrameCheck {
        pair(vec![stranger(bbox, vote, similarity)])
    }

    /// The owner's box shifted right by `dx` pixels.
    fn shifted(dx: f32) -> [f32; 4] {
        [OWNER[0] + dx, OWNER[1], OWNER[2] + dx, OWNER[3]]
    }

    /// A face at `bbox` that gave no print.
    fn rejected_face(bbox: [f32; 4], problem: FaceProblem) -> FaceCheck {
        FaceCheck {
            analysis: Analysis {
                face: face(bbox),
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

    /// A pair whose only face, at `bbox`, gave no print; no face at all for `NoFace`.
    fn rejected_at(bbox: [f32; 4], problem: FaceProblem) -> FrameCheck {
        let faces = if problem == FaceProblem::NoFace { vec![] } else { vec![rejected_face(bbox, problem)] };
        pair(faces)
    }

    fn rejected(problem: FaceProblem) -> FrameCheck {
        rejected_at(OWNER, problem)
    }

    /// The attempt of the largest face of the last pair.
    fn attempt(s: &Session) -> &Attempt {
        s.attempts().next().expect("a face in the last pair").1
    }

    /// The track of the largest face of the last pair.
    fn track(s: &Session) -> Option<u32> {
        s.attempts().next().map(|(id, _)| id)
    }

    #[test]
    fn recognises_after_min_prints_then_counts_live_votes() {
        let mut s = Session::default();
        for _ in 1..MIN_PRINTS {
            assert_eq!(s.add(&usable(Vote::Live, 0.9)), None);
        }
        assert!(!attempt(&s).recognised());
        // Votes before recognition do not count: this frame recognises and is the first live vote.
        assert_eq!(s.add(&usable(Vote::Live, 0.7)), None);
        assert!(attempt(&s).recognised());
        assert_eq!(s.add(&usable(Vote::Live, 0.6)), Some(Verdict::Match { similarity: 0.9 }));
    }

    #[test]
    fn three_spoof_votes_after_recognition_reject() {
        let mut s = Session::default();
        for _ in 0..MIN_PRINTS {
            s.add(&usable(Vote::Abstain, 0.9));
        }
        assert!(attempt(&s).recognised());
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
        assert_eq!(attempt(&s).prints, 0, "fresh counters");
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
            check.faces[0].analysis.lighting = Some(Lighting {
                lit: if i % 2 == 0 { 60.0 } else { 100.0 },
                ambient: 5.0,
                clipped_face: 0.0,
                clipped_around: 0.0,
            });
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
    fn clipped_faces_are_rejected_and_block_the_reattempt() {
        // The lamp on the face: no print, and the session names the reason.
        let clipped = |bbox| {
            let mut check = rejected_at(bbox, FaceProblem::Clipped);
            check.faces[0].analysis.lighting =
                Some(Lighting { lit: 120.0, ambient: 5.0, clipped_face: 0.05, clipped_around: 0.0 });
            check
        };
        let mut s = Session::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(s.add(&clipped(OWNER)), None);
        }
        assert_eq!(s.add(&clipped(OWNER)), Some(Verdict::NoFace { problem: FaceProblem::Clipped }));
        // A moving stranger with two clipped frames gets no re-attempt (F49).
        let mut moving = Session::default();
        for i in 0..2 {
            assert_eq!(moving.add(&clipped(shifted(10.0 * i as f32))), None);
        }
        for i in 2..MAX_PAIRS - 1 {
            assert_eq!(moving.add(&usable_at(shifted(10.0 * i as f32), Vote::Live, 0.3)), None);
        }
        let last = moving.add(&usable_at(shifted(160.0), Vote::Live, 0.3));
        assert_eq!(last, Some(Verdict::NoMatch { similarity: 0.3 }));
        assert!(!moving.reattempted());
    }

    #[test]
    fn each_track_reattempts_once() {
        // The owner's track re-attempts at the pair limit; a photo held up next (its own track,
        // moving) gets a re-attempt of its own at the next limit, and none after that.
        let mut s = Session::default();
        for i in 0..MAX_PAIRS {
            assert_eq!(s.add(&usable_at(shifted(10.0 * i as f32), Vote::Live, 0.3)), None);
        }
        assert!(s.reattempted());
        let photo = |i: u32| {
            let dx = 10.0 * i as f32;
            stranger_at([PHOTO[0] + dx, PHOTO[1], PHOTO[2] + dx, PHOTO[3]], Vote::Live, 0.35)
        };
        for i in 0..MAX_PAIRS {
            assert_eq!(s.add(&photo(i)), None);
        }
        assert_eq!(attempt(&s).prints, 0, "the photo's track re-attempted");
        for i in 1..MAX_PAIRS {
            assert_eq!(s.add(&photo(MAX_PAIRS + i)), None);
        }
        assert_eq!(s.add(&photo(2 * MAX_PAIRS)), Some(Verdict::NoMatch { similarity: 0.35 }));
    }

    #[test]
    fn reattempt_covers_notlive_but_not_noface() {
        // Recognised, then three spoof votes while the face moved: the track starts over instead
        // of NOTLIVE, and is not recognised any more.
        let mut s = Session::default();
        for i in 0..MIN_PRINTS {
            s.add(&usable_at(shifted(40.0 * i as f32), Vote::Abstain, 0.9));
        }
        assert!(attempt(&s).recognised());
        for _ in 0..2 {
            assert_eq!(s.add(&usable_at(shifted(160.0), Vote::Spoof, 0.9)), None);
        }
        assert_eq!(s.add(&usable_at(shifted(160.0), Vote::Spoof, 0.9)), None);
        assert!(s.reattempted() && !attempt(&s).recognised());
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
            let mut check = owner(OWNER, Vote::Live, 0.3);
            check.analysis.print = Some(Embedding { features: features.to_vec(), windows: None });
            Some(check.analysis)
        };
        let mut collected = Vec::new();
        assert_eq!(take_print(&mut collected, good(&[1.0, 0.0])), None);
        // Cosine 0.99 to the first print: the same view again.
        assert_eq!(take_print(&mut collected, good(&[0.99, 0.141])), Some("near_duplicate"));
        // Cosine 0.97: a new view.
        assert_eq!(take_print(&mut collected, good(&[0.97, 0.243])), None);
        assert_eq!(collected.len(), 2);
        // Weak lighting has a print but is not enrolled from; rejected frames name their reason.
        let largest = |check: FrameCheck| check.faces.into_iter().next().map(|f| f.analysis);
        assert_eq!(take_print(&mut collected, largest(usable(Vote::Abstain, 0.3))), Some("weak_lighting"));
        assert_eq!(take_print(&mut collected, largest(rejected(FaceProblem::TooDark))), Some("too_dark"));
        assert_eq!(take_print(&mut collected, largest(rejected(FaceProblem::NoFace))), Some("no_face"));
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
        assert!(attempt(&recognised).recognised());
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
        let owner = track(&s);
        for _ in 0..MIN_PRINTS + 2 {
            assert_eq!(s.add(&usable_at(PHOTO, Vote::Live, 0.1)), None);
        }
        assert_ne!(track(&s), owner);
        assert!(!attempt(&s).recognised());
    }

    #[test]
    fn another_face_in_the_same_place_starts_a_new_track() {
        // The owner is recognised and has one live vote; a photo of someone else is then held
        // exactly where the face was. By place alone it would continue the track and its live
        // vote would unlock; its print does not match the owner's, so it starts a new track.
        let mut s = Session::default();
        for _ in 0..MIN_PRINTS {
            s.add(&usable(Vote::Abstain, 0.9));
        }
        assert_eq!(s.add(&usable(Vote::Live, 0.9)), None);
        let owner = track(&s);
        assert_eq!(s.add(&stranger_at(OWNER, Vote::Live, 0.2)), None);
        assert_ne!(track(&s), owner);
        assert!(!attempt(&s).recognised());
        // A rejected frame (no print) continues the photo's track by place. The owner's face
        // back in place within a second continues the owner's track (F37), whose second live
        // vote unlocks.
        let photo = track(&s);
        assert_eq!(s.add(&rejected(FaceProblem::TooDark)), None);
        assert_eq!(track(&s), photo);
        assert_eq!(s.add(&usable(Vote::Live, 0.9)), Some(Verdict::Match { similarity: 0.9 }));
        assert_eq!(track(&s), owner);
    }

    #[test]
    fn votes_of_one_face_do_not_count_for_another() {
        // The owner is recognised and casts one live vote; a different face then votes live.
        let mut s = Session::default();
        for _ in 0..MIN_PRINTS {
            assert_eq!(s.add(&usable(Vote::Live, 0.9)), None);
        }
        assert!(attempt(&s).recognised());
        assert_eq!(attempt(&s).liveness(), LivenessState::Pending);
        assert_eq!(s.add(&usable_at(PHOTO, Vote::Live, 0.3)), None);
        assert!(!attempt(&s).recognised());
        // The pair limit counts the whole session; the verdict is the recognised face's.
        let rest: Vec<_> = (MIN_PRINTS + 2..MAX_PAIRS).map(|_| s.add(&usable_at(PHOTO, Vote::Live, 0.3))).collect();
        assert!(rest.iter().all(Option::is_none));
        assert_eq!(s.add(&usable_at(PHOTO, Vote::Live, 0.3)), Some(Verdict::NotLive));
    }

    #[test]
    fn two_faces_are_judged_apart_and_the_user_unlocks() {
        // The owner and a stranger side by side: the stranger's prints and votes never count for
        // the owner, and the owner's track unlocks on its own.
        let mut s = Session::default();
        for _ in 1..MIN_PRINTS {
            assert_eq!(s.add(&pair(vec![owner(OWNER, Vote::Live, 0.9), stranger(PHOTO, Vote::Live, 0.1)])), None);
        }
        assert_eq!(s.attempts().map(|(id, a)| (id, a.recognised())).collect::<Vec<_>>(), vec![(1, false), (2, false)]);
        assert_eq!(s.add(&pair(vec![owner(OWNER, Vote::Live, 0.9), stranger(PHOTO, Vote::Live, 0.1)])), None);
        assert_eq!(s.attempts().map(|(id, a)| (id, a.recognised())).collect::<Vec<_>>(), vec![(1, true), (2, false)]);
        // The stranger alone in a pair casts no vote for the owner; the owner's next live vote unlocks.
        assert_eq!(s.add(&pair(vec![stranger(PHOTO, Vote::Live, 0.1)])), None);
        assert_eq!(
            s.add(&pair(vec![stranger(PHOTO, Vote::Live, 0.1), owner(OWNER, Vote::Live, 0.9)])),
            Some(Verdict::Match { similarity: 0.9 })
        );
        assert!(s.decided().is_some_and(Attempt::recognised));
        // Two faces seen: no enroll-on-failure candidate.
        assert!(s.failed_print().is_none());
    }

    #[test]
    fn the_better_matching_face_is_the_user() {
        // Two faces both match the user (the owner and a photo of the owner, or a look-alike,
        // identity 2: close enough to the owner's prints for the tracker, apart by place):
        // only the one with the lower FAR is recognised (F48 `MakeDecisions`).
        let mut s = Session::default();
        for _ in 0..MIN_PRINTS {
            assert_eq!(
                s.add(&pair(vec![owner(OWNER, Vote::Abstain, 0.7), person(2.0, PHOTO, Vote::Abstain, 0.9)])),
                None
            );
        }
        assert_eq!(s.attempts().map(|(id, a)| (id, a.recognised())).collect::<Vec<_>>(), vec![(1, false), (2, true)]);
        // Once the better face is gone from the pair, the other one is recognised.
        assert_eq!(s.add(&pair(vec![owner(OWNER, Vote::Abstain, 0.7)])), None);
        assert!(attempt(&s).recognised());
        // At the pair limit the verdict comes from the best-matching track with enough prints.
        let mut limit = Session::default();
        for _ in 1..MAX_PAIRS {
            limit.add(&pair(vec![stranger(OWNER, Vote::Live, 0.2), person(2.0, PHOTO, Vote::Live, 0.4)]));
        }
        let last = limit.add(&pair(vec![stranger(OWNER, Vote::Live, 0.2), person(2.0, PHOTO, Vote::Live, 0.4)]));
        assert_eq!(last, Some(Verdict::NoMatch { similarity: 0.4 }));
    }

    #[test]
    fn occlusion_gives_up_only_when_every_face_is_occluded() {
        let mut s = Session::default();
        for _ in 0..OCCLUDED_GIVE_UP + 1 {
            let faces = vec![rejected_face(OWNER, FaceProblem::Occluded), stranger(PHOTO, Vote::Live, 0.1)];
            assert_eq!(s.add(&pair(faces)), None);
        }
        for _ in 1..OCCLUDED_GIVE_UP {
            let faces = vec![rejected_face(OWNER, FaceProblem::Occluded), rejected_face(PHOTO, FaceProblem::Occluded)];
            assert_eq!(s.add(&pair(faces)), None);
        }
        let faces = vec![rejected_face(OWNER, FaceProblem::Occluded), rejected_face(PHOTO, FaceProblem::Occluded)];
        assert_eq!(s.add(&pair(faces)), Some(Verdict::NoFace { problem: FaceProblem::Occluded }));
    }

    #[test]
    fn failed_print_needs_one_real_unrecognised_face() {
        // Not recognised, two live votes: the best live print is the candidate.
        let mut s = Session::default();
        for similarity in [0.30, 0.45, 0.42, 0.35] {
            s.add(&usable(Vote::Live, similarity));
        }
        assert_eq!(s.failed_print().map(|p| p.features[1]), Some(0.45));

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
        assert!(attempt(&two).failed_print().is_some());
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
        assert!(attempt(&session(0.51)).augmentation().is_some());
        assert!(attempt(&session(0.60)).augmentation().is_none());
        assert!(attempt(&session(0.45)).augmentation().is_none());
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
