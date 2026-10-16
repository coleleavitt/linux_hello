//! Face pipeline for one IR frame pair: detection on the lit frame, frame gates, lighting
//! status, embedding, RGB liveness veto and the liveness vote.

use anyhow::{bail, Context, Result};
use image::{GrayImage, RgbImage};
use std::path::Path;
use tracing::{debug, info};

use crate::lighting::{subtract_ambient, Lighting, LightingStatus};
use crate::{
    gray_to_rgb, AntiSpoof, Embedding, Face, FaceDetector, FaceEmbedder, FaceProblem, FaceStore, IrPair, MsFrame,
    OnFailure, PrintMatch, Template, Vote, WindowsHelloModels,
};

/// Minimum face box width in pixels (F46, F54).
const MIN_FACE_PX: f32 = 72.0;
/// Minimum face box width as a fraction of the shorter frame side (F46, F54).
const MIN_FACE_RATIO: f32 = 0.12;
/// The face box must stay this fraction of the eye distance away from every frame edge (F46).
const EDGE_MARGIN_EYES: f32 = 0.2;

/// What a frame pair gives for recognition.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// Largest face, detected on the lit frame
    pub face: Option<Face>,
    pub lighting: Option<Lighting>,
    /// Why there is no print; `None` when there is one
    pub problem: Option<FaceProblem>,
    /// Embedding of the face: from `lit − min(lit, ambient)` with good lighting, from the lit
    /// frame with weak lighting (F40)
    pub print: Option<Embedding>,
}

impl Analysis {
    pub fn status(&self) -> Option<LightingStatus> {
        self.lighting.map(|l| l.status())
    }
}

/// Scores from one frame pair; `None` where a stage did not run.
#[derive(Debug, Clone)]
pub struct FrameCheck {
    pub analysis: Analysis,
    /// Mean P(real) of the RGB liveness models; only for good-lighting frames
    pub liveness: Option<f32>,
    /// Windows Hello anti-spoofing frame vote; only for good-lighting frames
    pub windows: Option<MsFrame>,
    pub vote: Vote,
    /// Closest print of the user's template
    pub best: Option<PrintMatch>,
}

pub struct FacePipeline {
    detector: FaceDetector,
    embedder: FaceEmbedder,
    liveness: Vec<AntiSpoof>,
    windows: Option<WindowsHelloModels>,
    store: FaceStore,
}

impl FacePipeline {
    /// `liveness` may be empty to skip the RGB veto; the IR lighting status always applies.
    pub fn new(
        detector_path: impl AsRef<Path>,
        embedder_path: impl AsRef<Path>,
        liveness: Vec<AntiSpoof>,
        store_path: impl AsRef<Path>,
    ) -> Result<Self> {
        info!("initializing face pipeline");
        let detector = FaceDetector::load(detector_path)?;
        let embedder = FaceEmbedder::load(embedder_path)?;
        let store = FaceStore::load(store_path)?;
        Ok(Self { detector, embedder, liveness, windows: None, store })
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

    /// Detect the largest face in the image.
    pub fn largest_face(&mut self, image: &RgbImage) -> Result<Option<Face>> {
        let faces = self.detector.detect(image)?;
        Ok(faces.into_iter().max_by(|a, b| area(a).total_cmp(&area(b))))
    }

    pub fn is_enrolled(&self, user: &str) -> bool {
        self.store.get(user).is_some()
    }

    /// Re-read the face store, so enrollments made by another process are seen.
    pub fn reload_store(&mut self) -> Result<()> {
        self.store.reload()
    }

    /// Find the face on the lit frame, apply the size and edge gates (F46), the lighting status
    /// (F44, F45) and, with the Windows Hello models, the occlusion and frontal gates (F46, F59), and
    /// embed the face if the frame is usable.
    pub fn analyze(&mut self, pair: &IrPair) -> Result<Analysis> {
        let faces = self.detector.detect_gray(&pair.lit)?;
        let Some(face) = faces.into_iter().max_by(|a, b| area(a).total_cmp(&area(b))) else {
            return Ok(Analysis { face: None, lighting: None, problem: Some(FaceProblem::NoFace), print: None });
        };
        let lighting = Lighting::measure(&pair.lit, &pair.ambient, &face);
        let problem = geometry_problem(&face, pair.lit.width(), pair.lit.height()).or(match lighting.status() {
            LightingStatus::TooDark => Some(FaceProblem::TooDark),
            LightingStatus::TooBright => Some(FaceProblem::TooBright),
            LightingStatus::Good | LightingStatus::Weak => None,
        });
        let problem = match (problem, &mut self.windows) {
            (None, Some(w)) => windows_problem(w, &pair.lit, &face)?,
            (problem, _) => problem,
        };
        if let Some(problem) = problem {
            debug!(?problem, lit = lighting.lit, ambient = lighting.ambient, "frame rejected");
            return Ok(Analysis { face: Some(face), lighting: Some(lighting), problem: Some(problem), print: None });
        }
        let image = match lighting.status() {
            LightingStatus::Good => subtract_ambient(&pair.lit, &pair.ambient),
            _ => pair.lit.clone(),
        };
        let print = self.embedder.embed_face(&gray_to_rgb(&image), &face)?;
        Ok(Analysis { face: Some(face), lighting: Some(lighting), problem: None, print: Some(print) })
    }

    /// Run every stage on one frame pair. The face is compared only with `user`'s template.
    /// The liveness models run on good-lighting frames only (the others cannot vote live); the
    /// RGB ones on `rgb`, the frame captured alongside the pair.
    pub fn check(&mut self, user: &str, pair: &IrPair, rgb: Option<&RgbImage>) -> Result<FrameCheck> {
        let analysis = self.analyze(pair)?;
        let (Some(print), Some(face)) = (&analysis.print, &analysis.face) else {
            return Ok(FrameCheck { analysis, liveness: None, windows: None, vote: Vote::Abstain, best: None });
        };
        let best = self.store.get(user).and_then(|t| t.best_match(print));
        if analysis.status() != Some(LightingStatus::Good) {
            let vote = Vote::from_evidence(false, None, None);
            return Ok(FrameCheck { analysis, liveness: None, windows: None, vote, best });
        }
        let rgb_face = match rgb {
            Some(image) => self.largest_face(image)?.map(|f| (image, f)),
            None => None,
        };
        // The open RGB models stand in for Windows' colour net (slot 13). With the Windows Hello models
        // that net vetoes instead, so the RGB evidence is not counted twice (F24, F25).
        let open_rgb = self.windows.is_none() && !self.liveness.is_empty();
        let liveness = open_rgb.then(|| self.rgb_liveness(rgb_face.as_ref())).transpose()?;
        let windows = match &mut self.windows {
            Some(w) => {
                Some(w.liveness.frame(&pair.lit, Some(&pair.ambient), face, rgb_face.as_ref().map(|(i, f)| (*i, f)))?)
            }
            None => None,
        };
        let vote = Vote::from_evidence(true, liveness, windows.as_ref().map(|w| w.total));
        Ok(FrameCheck { analysis, liveness, windows, vote, best })
    }

    /// Mean "real" probability over the RGB liveness models; 0 (a veto) when there is no RGB
    /// frame or no face in it.
    fn rgb_liveness(&mut self, rgb: Option<&(&RgbImage, Face)>) -> Result<f32> {
        let Some((image, face)) = rgb else { return Ok(0.0) };
        let mut total = 0.0;
        for model in &mut self.liveness {
            total += model.check(image, face)?.score;
        }
        Ok(total / self.liveness.len() as f32)
    }

    /// Store one print per enrollment frame (no averaging, F28) and save. Unless `force`, refuse
    /// prints that match another enrolled user (`CheckForDuplicateEnrollment`).
    pub fn enroll(&mut self, user_id: &str, prints: Vec<Embedding>, force: bool) -> Result<()> {
        if prints.is_empty() {
            bail!("no enrollment prints");
        }
        if let (false, Some((other, similarity))) = (force, self.store.duplicate(user_id, &prints)) {
            bail!(
                "this face matches enrolled user {other} (similarity {similarity:.3}); use --force to enroll it anyway"
            );
        }
        let count = prints.len();
        self.store.add(user_id.to_string(), Template::enrolled(prints));
        self.store.save().context("saving face store")?;
        info!(user_id, prints = count, "enrolled");
        Ok(())
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
        let result = self.store.add_on_failure(user_id, print);
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

/// Gates that need the Windows Hello models: occlusion, then the frontal check (F46, F59) with the
/// pose of the Windows IR detector's face that overlaps the SCRFD face (`RetinaEngine::ComputeOrientation`).
/// When that detector has no such face, Windows tracks no face and gets no orientation, so the frame is
/// not frontal either.
fn windows_problem(w: &mut WindowsHelloModels, lit: &GrayImage, face: &Face) -> Result<Option<FaceProblem>> {
    if w.occlusion.check(lit, face)?.is_occluded {
        return Ok(Some(FaceProblem::Occluded));
    }
    let orientation = w.retina.matching(lit, face)?.and_then(|(f, _)| f.orientation);
    debug!(?orientation, "IR detector pose");
    Ok((!orientation.is_some_and(|o| o.is_frontal())).then_some(FaceProblem::NotFrontal))
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
