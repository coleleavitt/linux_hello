//! Face pipeline for one IR frame pair: detection on the lit frame, frame gates, lighting
//! status, embedding, RGB liveness veto and the liveness vote.

use anyhow::{bail, Context, Result};
use image::{GrayImage, RgbImage};
use std::borrow::Cow;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info};

use crate::calibration::CalibrationTable;
use crate::camera::MAX_PAIR_GAP;
use crate::lighting::{self, subtract_ambient, Lighting, LightingStatus};
use crate::tracker::{iou, MIN_IOU};
use crate::{
    gray_to_rgb, AntiSpoof, Embedding, Face, FaceDetector, FaceEmbedder, FaceProblem, FaceStore, IrPair, MsFrame,
    MsRetinaDetector, MsRetinaFace, OnFailure, PrintMatch, Template, Vote, WindowsHelloModels,
};

/// Minimum face box width in pixels (F46, F54).
const MIN_FACE_PX: f32 = 72.0;
/// Minimum face box width as a fraction of the shorter frame side (F46, F54).
const MIN_FACE_RATIO: f32 = 0.12;
/// The face box must stay this fraction of the eye distance away from every frame edge (F46).
const EDGE_MARGIN_EYES: f32 = 0.2;

/// Faces of one lit frame that are analysed, largest first: Windows tracks every face it detects
/// (F47, F48); the bound keeps the time per pair under control (one ArcFace run per face).
pub const MAX_FACES: usize = 3;

/// What a frame pair gives for recognition of one face.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The face as SCRFD detects it on the lit frame
    pub face: Face,
    /// The same face as the Windows IR detector reports it (box and five points), when the
    /// Windows Hello models run: Windows' crops and landmarks come from it (F23, F43, F58)
    pub windows_face: Option<Face>,
    pub lighting: Option<Lighting>,
    /// Why there is no print; `None` when there is one
    pub problem: Option<FaceProblem>,
    /// Embedding of the face: from `lit − min(lit, ambient)` with good lighting, from the lit
    /// frame with weak lighting (F40); with the Windows Hello models also Microsoft's feature of
    /// the same image
    pub print: Option<Embedding>,
}

impl Analysis {
    pub fn status(&self) -> Option<LightingStatus> {
        self.lighting.map(|l| l.status())
    }
}

/// Scores of one face of a frame pair; `None` where a stage did not run.
#[derive(Debug, Clone)]
pub struct FaceCheck {
    pub analysis: Analysis,
    /// Mean P(real) of the RGB liveness models; only for good-lighting frames
    pub liveness: Option<f32>,
    /// Windows Hello anti-spoofing frame vote; only for good-lighting frames
    pub windows: Option<MsFrame>,
    pub vote: Vote,
    /// Closest print of the user's template, with its estimated FAR and, with the Windows Hello
    /// models, Microsoft's second opinion
    pub best: Option<PrintMatch>,
}

/// One frame pair through every stage.
#[derive(Debug, Clone)]
pub struct FrameCheck {
    /// Width and height of the IR frame, to normalise face positions (F49)
    pub frame: [u32; 2],
    /// The analysed faces, largest first (at most [`MAX_FACES`]); empty without a face
    pub faces: Vec<FaceCheck>,
}

impl FrameCheck {
    /// The largest face of the pair.
    pub fn largest(&self) -> Option<&FaceCheck> {
        self.faces.first()
    }
}

pub struct FacePipeline {
    detector: FaceDetector,
    embedder: FaceEmbedder,
    liveness: Vec<AntiSpoof>,
    windows: Option<WindowsHelloModels>,
    store: FaceStore,
    /// Similarity → FAR for this camera (F51)
    calibration: CalibrationTable,
    /// The faces of the last lit frame, for the ambient warp of the next pair (F13)
    previous: Vec<LitFace>,
}

/// Where a lit frame's face was, for the motion compensation of the ambient frame (F13).
struct LitFace {
    at: Duration,
    bbox: [f32; 4],
    anchors: [[f32; 2]; 3],
}

impl FacePipeline {
    /// `liveness` may be empty to skip the RGB veto; the IR lighting status always applies.
    pub fn new(
        detector_path: impl AsRef<Path>,
        embedder_path: impl AsRef<Path>,
        liveness: Vec<AntiSpoof>,
        store_path: impl AsRef<Path>,
        calibration: CalibrationTable,
    ) -> Result<Self> {
        info!("initializing face pipeline");
        let detector = FaceDetector::load(detector_path)?;
        let embedder = FaceEmbedder::load(embedder_path)?;
        let store = FaceStore::load(store_path)?;
        Ok(Self { detector, embedder, liveness, windows: None, store, calibration, previous: Vec::new() })
    }

    /// Add the Windows Hello anti-spoofing vote and the occlusion and frontal gates.
    pub fn with_windows_hello(mut self, models: WindowsHelloModels) -> Self {
        self.windows = Some(models);
        self
    }

    /// While enrolling, Windows rejects a face that is occluded with probability above 0.5
    /// instead of 0.9 (F46).
    pub fn set_enrolling(&mut self, enrolling: bool) {
        if let Some(w) = &mut self.windows {
            w.occlusion.set_enrollment_mode(enrolling);
        }
    }

    pub fn is_enrolled(&self, user: &str) -> bool {
        self.store.get(user).is_some()
    }

    /// Re-read the face store, so enrollments made by another process are seen.
    pub fn reload_store(&mut self) -> Result<()> {
        self.store.reload()
    }

    /// Find the faces on the lit frame, largest first, and take each of the [`MAX_FACES`] largest
    /// through the size and edge gates (F46), the lighting status (F44, F45) and, with the Windows
    /// Hello models, the occlusion and frontal gates (F46, F59); a face that passes is embedded.
    ///
    /// With the Windows Hello models the lighting status, the occlusion crop and the frontal check use
    /// the face of the Windows IR detector that overlaps the SCRFD face (the detector Windows tracks
    /// with, F58); without them SCRFD's box and points stand in. The ambient frame is moved onto the
    /// lit one before it is measured and subtracted (F13, F40).
    pub fn analyze(&mut self, pair: &IrPair) -> Result<Vec<Analysis>> {
        let mut faces = self.detector.detect_gray(&pair.lit)?;
        faces.sort_by(|a, b| area(b).total_cmp(&area(a)));
        faces.truncate(MAX_FACES);
        // The Windows IR detector runs once per frame, as Windows' tracker does (F58).
        let retina = match &mut self.windows {
            Some(w) if !faces.is_empty() => w.retina.detect(&pair.lit)?,
            _ => Vec::new(),
        };
        let previous = std::mem::take(&mut self.previous);
        let mut analyses = Vec::with_capacity(faces.len());
        for face in faces {
            let overlap = |r: &&MsRetinaFace| iou(&r.face.bbox, &face.bbox);
            let retina = retina
                .iter()
                .max_by(|a, b| overlap(a).total_cmp(&overlap(b)))
                .filter(|r| overlap(r) >= MsRetinaDetector::MATCH_IOU);
            analyses.push(self.analyze_face(pair, face, retina, &previous)?);
        }
        Ok(analyses)
    }

    /// One face through the gates; `retina` is the Windows IR detector's face overlapping it,
    /// `previous` the faces of the previous lit frame.
    fn analyze_face(
        &mut self,
        pair: &IrPair,
        face: Face,
        retina: Option<&MsRetinaFace>,
        previous: &[LitFace],
    ) -> Result<Analysis> {
        let rejected = |face, windows_face, lighting, problem| {
            debug!(?problem, ?lighting, "face rejected");
            Ok(Analysis { face, windows_face, lighting, problem: Some(problem), print: None })
        };
        if let Some(problem) = geometry_problem(&face, pair.lit.width(), pair.lit.height()) {
            return rejected(face, None, None, problem);
        }
        let windows_face = retina.map(|f| f.face.clone());
        let tracked = windows_face.as_ref().unwrap_or(&face);
        let (points, bbox) = (tracked.landmarks, tracked.bbox);
        let ambient = self.compensated_ambient(pair, &face.bbox, &points, previous);
        let lighting = Lighting::measure(&pair.lit, &ambient, &points, &bbox);
        let problem = match lighting.status() {
            LightingStatus::TooDark => Some(FaceProblem::TooDark),
            LightingStatus::TooBright => Some(FaceProblem::TooBright),
            LightingStatus::Clipped => Some(FaceProblem::Clipped),
            LightingStatus::Good | LightingStatus::Weak | LightingStatus::ClippedAround => None,
        };
        let problem = match (problem, &mut self.windows) {
            (None, Some(w)) => windows_problem(w, &pair.lit, retina)?,
            (problem, _) => problem,
        };
        if let Some(problem) = problem {
            return rejected(face, windows_face, Some(lighting), problem);
        }
        let image = match lighting.status() {
            LightingStatus::Good => subtract_ambient(&pair.lit, &ambient),
            _ => pair.lit.clone(),
        };
        let mut print = self.embedder.embed_face(&gray_to_rgb(&image), &face)?;
        // Microsoft's recogniser takes the same image (F40 a) at Windows' points (F23).
        if let Some(w) = &mut self.windows {
            print.windows = Some(w.recognizer.embed(&image, &points)?.to_vec());
        }
        Ok(Analysis { face, windows_face, lighting: Some(lighting), problem: None, print: Some(print) })
    }

    /// The ambient frame moved onto the lit one (`AdjustFaceForAmbientLight`, F13): the face's anchors
    /// in the ambient frame are taken by linear motion from where they were in the previous lit frame,
    /// if that frame is less than 300 ms old and shows the same face (box overlap as the tracker). The
    /// first pair of a face is subtracted as captured.
    fn compensated_ambient<'a>(
        &mut self,
        pair: &'a IrPair,
        bbox: &[f32; 4],
        points: &[[f32; 2]; 5],
        previous: &[LitFace],
    ) -> Cow<'a, GrayImage> {
        let anchors = lighting::anchors(points);
        self.previous.push(LitFace { at: pair.lit_at, bbox: *bbox, anchors });
        let Some(previous) = previous
            .iter()
            .filter(|p| p.at < pair.lit_at && pair.lit_at - p.at < MAX_PAIR_GAP)
            .max_by(|a, b| iou(&a.bbox, bbox).total_cmp(&iou(&b.bbox, bbox)))
            .filter(|p| iou(&p.bbox, bbox) >= MIN_IOU)
        else {
            return Cow::Borrowed(&pair.ambient);
        };
        let nanos = |d: Duration| d.as_nanos() as f64;
        let t = (nanos(pair.ambient_at) - nanos(pair.lit_at)) / (nanos(previous.at) - nanos(pair.lit_at));
        let in_ambient = lighting::interpolate(&anchors, &previous.anchors, t as f32);
        match lighting::affine_from_anchors(&anchors, &in_ambient) {
            Some(m) => Cow::Owned(lighting::warp(&pair.ambient, &m)),
            None => Cow::Borrowed(&pair.ambient),
        }
    }

    /// Run every stage on one frame pair, for every analysed face. Faces are compared only with
    /// `user`'s template. The liveness models run on good-lighting faces only (the others cannot
    /// vote live); the RGB ones on the face of `rgb`, the frame captured alongside the pair, that
    /// lies nearest (normalised centre) to the IR face.
    pub fn check(&mut self, user: &str, pair: &IrPair, rgb: Option<&RgbImage>) -> Result<FrameCheck> {
        let frame = [pair.lit.width(), pair.lit.height()];
        let analyses = self.analyze(pair)?;
        let good = analyses.iter().any(|a| a.print.is_some() && a.status() == Some(LightingStatus::Good));
        let rgb_faces = match rgb {
            Some(image) if good => self.detector.detect(image)?,
            _ => Vec::new(),
        };
        let mut faces = Vec::with_capacity(analyses.len());
        for analysis in analyses {
            let Some(print) = &analysis.print else {
                faces.push(FaceCheck { analysis, liveness: None, windows: None, vote: Vote::Abstain, best: None });
                continue;
            };
            let windows_table = self.windows.as_ref().map(|w| &w.recognizer.calibration);
            let best = self.store.get(user).and_then(|t| t.best_match(print, &self.calibration, windows_table));
            if analysis.status() != Some(LightingStatus::Good) {
                let vote = Vote::from_evidence(false, None, None);
                faces.push(FaceCheck { analysis, liveness: None, windows: None, vote, best });
                continue;
            }
            let rgb_face = rgb.and_then(|image| nearest(&rgb_faces, image, &analysis.face, frame).map(|f| (image, f)));
            // The open RGB models stand in for Windows' colour net (slot 13). With the Windows Hello
            // models that net vetoes instead, so the RGB evidence is not counted twice (F24, F25).
            let open_rgb = self.windows.is_none() && !self.liveness.is_empty();
            let liveness = open_rgb.then(|| self.rgb_liveness(rgb_face)).transpose()?;
            // The Windows nets take Windows' landmarks for their crop (F23).
            let face = analysis.windows_face.as_ref().unwrap_or(&analysis.face);
            let windows = match &mut self.windows {
                Some(w) => Some(w.liveness.frame(&pair.lit, Some(&pair.ambient), face, rgb_face)?),
                None => None,
            };
            let vote = Vote::from_evidence(true, liveness, windows.as_ref().map(|w| w.total));
            faces.push(FaceCheck { analysis, liveness, windows, vote, best });
        }
        Ok(FrameCheck { frame, faces })
    }

    /// Mean "real" probability over the RGB liveness models; 0 (a veto) when there is no RGB
    /// frame or no face in it.
    fn rgb_liveness(&mut self, rgb: Option<(&RgbImage, &Face)>) -> Result<f32> {
        let Some((image, face)) = rgb else { return Ok(0.0) };
        let mut total = 0.0;
        for model in &mut self.liveness {
            total += model.check(image, face)?.score;
        }
        Ok(total / self.liveness.len() as f32)
    }

    /// The user's template.
    pub fn template(&self, user: &str) -> Option<&Template> {
        self.store.get(user)
    }

    /// Store one print per enrollment frame (no averaging, F28) and save: a new template, or with
    /// `add` more prints on the existing one (up to [`crate::matcher::MAX_PRINTS`]). Unless `force`,
    /// refuse prints that match another enrolled user (`CheckForDuplicateEnrollment`). Returns the
    /// prints stored and the prints in the template afterwards.
    pub fn enroll(&mut self, user_id: &str, prints: Vec<Embedding>, force: bool, add: bool) -> Result<(usize, usize)> {
        if prints.is_empty() {
            bail!("no enrollment prints");
        }
        if let (false, Some((other, m))) = (force, self.store.duplicate(user_id, &prints, &self.calibration)) {
            bail!(
                "this face matches enrolled user {other} (similarity {:.3}, FAR {:.1e}); use --force to enroll it anyway",
                m.similarity,
                m.far
            );
        }
        let (stored, total) = if add {
            let Some(template) = self.store.get_mut(user_id) else { bail!("{user_id} is not enrolled") };
            let stored = template.add_enrolled(prints);
            (stored, template.prints.len())
        } else {
            let count = prints.len();
            self.store.add(user_id.to_string(), Template::enrolled(prints));
            (count, count)
        };
        self.store.save().context("saving face store")?;
        info!(user_id, prints = stored, total, add, "enrolled");
        Ok((stored, total))
    }

    /// Add a print from a successful unlock to the user's template and save the store (F31).
    /// Returns whether the print was added.
    pub fn augment(&mut self, user_id: &str, print: &Embedding, matched: PrintMatch) -> Result<bool> {
        let Some(template) = self.store.get_mut(user_id) else { return Ok(false) };
        if !template.augment(print, matched) {
            return Ok(false);
        }
        self.store.save().context("saving face store")?;
        Ok(true)
    }

    /// Enroll-on-failure (F31): add `print` to the user's template if [`FaceStore::add_on_failure`]
    /// accepts it, and save the store.
    pub fn add_on_failure(&mut self, user_id: &str, print: &Embedding) -> Result<OnFailure> {
        let result = self.store.add_on_failure(user_id, print, &self.calibration);
        if let OnFailure::Added { .. } = result {
            self.store.save().context("saving face store")?;
        }
        Ok(result)
    }

    pub fn unenroll(&mut self, user_id: &str) -> Result<bool> {
        let removed = self.store.remove(user_id);
        if removed {
            self.store.save()?;
        }
        Ok(removed)
    }

    pub fn list_enrolled(&self) -> Vec<&str> {
        self.store.list_users()
    }
}

fn area(f: &Face) -> f32 {
    (f.bbox[2] - f.bbox[0]) * (f.bbox[3] - f.bbox[1])
}

/// The face of the RGB frame nearest to `ir_face` by box centre, each normalised by its frame
/// size: the two cameras sit side by side with a similar field of view, and the frames are not
/// registered to each other (`EvaluateColorFaceQuality` is not ported).
fn nearest<'a>(rgb_faces: &'a [Face], rgb: &RgbImage, ir_face: &Face, ir_frame: [u32; 2]) -> Option<&'a Face> {
    let centre =
        |f: &Face, w: u32, h: u32| [(f.bbox[0] + f.bbox[2]) / 2.0 / w as f32, (f.bbox[1] + f.bbox[3]) / 2.0 / h as f32];
    let [ix, iy] = centre(ir_face, ir_frame[0], ir_frame[1]);
    let distance = |f: &Face| {
        let [x, y] = centre(f, rgb.width(), rgb.height());
        (x - ix) * (x - ix) + (y - iy) * (y - iy)
    };
    rgb_faces.iter().min_by(|a, b| distance(a).total_cmp(&distance(b)))
}

/// Gates that need the Windows Hello models, on `retina`, the Windows IR detector's face that overlaps
/// the SCRFD face: occlusion on its box, then the frontal check (F46, F59) with its pose
/// (`RetinaEngine::ComputeOrientation`). When that detector has no such face, Windows tracks no face
/// and gets no orientation, so the frame is not frontal either.
fn windows_problem(
    w: &mut WindowsHelloModels,
    lit: &GrayImage,
    retina: Option<&MsRetinaFace>,
) -> Result<Option<FaceProblem>> {
    let Some(retina) = retina else { return Ok(Some(FaceProblem::NotFrontal)) };
    if w.occlusion.check(lit, &retina.face)?.is_occluded {
        return Ok(Some(FaceProblem::Occluded));
    }
    debug!(orientation = ?retina.orientation, "IR detector pose");
    Ok((!retina.orientation.is_some_and(|o| o.is_frontal())).then_some(FaceProblem::NotFrontal))
}

/// Size and edge gates of Windows' per-frame check (F46). Occlusion and pose are checked in
/// `windows_problem` when the Windows Hello models are loaded.
fn geometry_problem(face: &Face, width: u32, height: u32) -> Option<FaceProblem> {
    let [x1, y1, x2, y2] = face.bbox;
    if x2 - x1 < MIN_FACE_PX.max(MIN_FACE_RATIO * width.min(height) as f32) {
        return Some(FaceProblem::TooFar);
    }
    let [le, re, ..] = face.landmarks;
    let margin = EDGE_MARGIN_EYES * (re[0] - le[0]).hypot(re[1] - le[1]);
    let (w, h) = (width as f32, height as f32);
    if x1 < margin || y1 < margin || x2 > w - margin || y2 > h - margin {
        return Some(FaceProblem::Edge);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(bbox: [f32; 4], eye_distance: f32) -> Face {
        let cx = (bbox[0] + bbox[2]) / 2.0;
        let eyes = [[cx - eye_distance / 2.0, bbox[1] + 40.0], [cx + eye_distance / 2.0, bbox[1] + 40.0]];
        Face { bbox, score: 1.0, landmarks: [eyes[0], eyes[1], [cx, 0.0], [cx, 0.0], [cx, 0.0]] }
    }

    #[test]
    fn frame_gates() {
        assert_eq!(geometry_problem(&face([200.0, 100.0, 300.0, 230.0], 40.0), 640, 360), None);
        assert_eq!(geometry_problem(&face([200.0, 100.0, 270.0, 230.0], 30.0), 640, 360), Some(FaceProblem::TooFar));
        // 8 px margin needed (40 / 5): 5 px from the top is too close, 10 px is enough.
        assert_eq!(geometry_problem(&face([200.0, 5.0, 300.0, 230.0], 40.0), 640, 360), Some(FaceProblem::Edge));
        assert_eq!(geometry_problem(&face([200.0, 10.0, 300.0, 350.0], 40.0), 640, 360), None);
        assert_eq!(geometry_problem(&face([545.0, 100.0, 635.0, 230.0], 40.0), 640, 360), Some(FaceProblem::Edge));
    }
}
