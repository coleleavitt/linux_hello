//! Face pipeline for one IR frame pair: detection on the lit frame, lighting gate, embedding of
//! the ambient-subtracted image, RGB liveness veto and the liveness vote.

use anyhow::{bail, Context, Result};
use image::RgbImage;
use std::path::Path;
use tracing::{debug, info};

use crate::lighting::{subtract_ambient, Lighting};
use crate::{
    gray_to_rgb, AntiSpoof, Embedding, Face, FaceDetector, FaceEmbedder, FaceStore, IrPair, PrintMatch, Template,
    Vote,
};

/// The parts of a frame pair that recognition uses.
pub struct Analysis {
    /// Largest face, detected on the lit frame
    pub face: Option<Face>,
    pub lighting: Option<Lighting>,
    /// Embedding of the ambient-subtracted face; only for faces that pass the lighting gate
    pub print: Option<Embedding>,
}

/// Scores from one frame pair; `None` where a stage did not run.
#[derive(Debug, Clone)]
pub struct FrameCheck {
    pub face: Option<Face>,
    pub lighting: Option<Lighting>,
    /// Mean P(real) of the RGB liveness models
    pub liveness: Option<f32>,
    pub vote: Vote,
    pub print: Option<Embedding>,
    /// Closest print of the user's template
    pub best: Option<PrintMatch>,
}

pub struct FacePipeline {
    detector: FaceDetector,
    embedder: FaceEmbedder,
    liveness: Vec<AntiSpoof>,
    store: FaceStore,
}

impl FacePipeline {
    /// `liveness` may be empty to skip the RGB veto; the IR lighting gate always applies.
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
        Ok(Self { detector, embedder, liveness, store })
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

    /// Find the face on the lit frame, apply the lighting gate (F14), and embed the face from
    /// `lit − min(lit, ambient)` (F13) when the lighting is good.
    pub fn analyze(&mut self, pair: &IrPair) -> Result<Analysis> {
        let faces = self.detector.detect_gray(&pair.lit)?;
        let Some(face) = faces.into_iter().max_by(|a, b| area(a).total_cmp(&area(b))) else {
            return Ok(Analysis { face: None, lighting: None, print: None });
        };
        let lighting = Lighting::measure(&pair.lit, &pair.ambient, &face);
        if !lighting.is_good() {
            debug!(lit = lighting.lit, ambient = lighting.ambient, gain = lighting.gain(), "bad lighting");
            return Ok(Analysis { face: Some(face), lighting: Some(lighting), print: None });
        }
        let diff = subtract_ambient(&pair.lit, &pair.ambient);
        let print = self.embedder.embed_face(&gray_to_rgb(&diff), &face)?;
        Ok(Analysis { face: Some(face), lighting: Some(lighting), print: Some(print) })
    }

    /// Run every stage on one frame pair. The face is compared only with `user`'s template.
    /// The RGB liveness models run on `rgb`, the frame captured alongside the pair.
    pub fn check(&mut self, user: &str, pair: &IrPair, rgb: Option<&RgbImage>) -> Result<FrameCheck> {
        let Analysis { face, lighting, print } = self.analyze(pair)?;
        let Some(print) = print else {
            return Ok(FrameCheck { face, lighting, liveness: None, vote: Vote::Abstain, print: None, best: None });
        };
        let liveness = if self.liveness.is_empty() { None } else { Some(self.rgb_liveness(rgb)?) };
        let vote = Vote::from_evidence(true, liveness);
        let best = self.store.get(user).and_then(|t| t.best_match(&print));
        Ok(FrameCheck { face, lighting, liveness, vote, print: Some(print), best })
    }

    /// Mean "real" probability over the RGB liveness models; 0 (a veto) when there is no RGB
    /// frame or no face in it.
    fn rgb_liveness(&mut self, rgb: Option<&RgbImage>) -> Result<f32> {
        let Some(image) = rgb else { return Ok(0.0) };
        let Some(face) = self.largest_face(image)? else { return Ok(0.0) };
        let mut total = 0.0;
        for model in &mut self.liveness {
            total += model.check(image, &face)?.score;
        }
        Ok(total / self.liveness.len() as f32)
    }

    /// Store one print per enrollment frame (no averaging, F28) and save.
    pub fn enroll(&mut self, user_id: &str, prints: Vec<Embedding>) -> Result<()> {
        if prints.is_empty() {
            bail!("no enrollment prints");
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
