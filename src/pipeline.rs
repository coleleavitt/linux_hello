//! Complete face authentication pipeline.

use anyhow::{bail, Context, Result};
use image::RgbImage;
use std::path::Path;
use tracing::info;

use crate::{
    AntiSpoof, Embedding, Face, FaceDetector, FaceEmbedder, FaceMatcher, FaceStore, Verdict,
};

const LIVENESS_THRESHOLD: f32 = 0.5;

/// Scores from one frame; `None` where a stage did not run.
#[derive(Debug, Clone, Default)]
pub struct FrameCheck {
    pub face: Option<Face>,
    pub liveness: Option<f32>,
    pub similarity: Option<f32>,
}

pub struct FacePipeline {
    detector: FaceDetector,
    embedder: FaceEmbedder,
    liveness: Vec<AntiSpoof>,
    matcher: FaceMatcher,
    store: FaceStore,
}

impl FacePipeline {
    /// `liveness` may be empty to skip the anti-spoofing check.
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
        let matcher = FaceMatcher::new(0.4);
        Ok(Self { detector, embedder, liveness, matcher, store })
    }

    /// Detect the largest face in the image.
    pub fn largest_face(&mut self, image: &RgbImage) -> Result<Option<Face>> {
        let faces = self.detector.detect(image)?;
        let area = |f: &Face| (f.bbox[2] - f.bbox[0]) * (f.bbox[3] - f.bbox[1]);
        Ok(faces.into_iter().max_by(|a, b| area(a).total_cmp(&area(b))))
    }

    pub fn is_enrolled(&self, user: &str) -> bool {
        self.store.get(user).is_some()
    }

    /// Re-read the face store, so enrollments made by another process are seen.
    pub fn reload_store(&mut self) -> Result<()> {
        self.store = FaceStore::load(self.store.path())?;
        Ok(())
    }

    /// Run every stage on one frame and report the raw scores. The face in `image` is compared
    /// only with `user`'s template. Liveness runs on `liveness_image` when given (the RGB frame
    /// captured alongside an IR frame), otherwise on `image`.
    pub fn check(&mut self, user: &str, image: &RgbImage, liveness_image: Option<&RgbImage>) -> Result<FrameCheck> {
        let Some(face) = self.largest_face(image)? else {
            return Ok(FrameCheck::default());
        };
        let liveness = if self.liveness.is_empty() { None } else { Some(self.liveness_score(image, &face, liveness_image)?) };
        let similarity = match self.store.get(user).cloned() {
            Some(template) => Some(self.embedder.embed_face(image, &face)?.similarity(&template)),
            None => None,
        };
        Ok(FrameCheck { face: Some(face), liveness, similarity })
    }

    pub fn verdict(&self, check: &FrameCheck) -> Verdict {
        match (&check.face, check.liveness, check.similarity) {
            (None, _, _) => Verdict::NoFace,
            (_, _, None) => Verdict::NotEnrolled,
            (_, Some(live), _) if live < LIVENESS_THRESHOLD => Verdict::NotLive,
            (_, _, Some(s)) if s > self.matcher.threshold() => Verdict::Match { similarity: s },
            (_, _, Some(s)) => Verdict::NoMatch { similarity: s },
        }
    }

    /// Mean "real" probability over all liveness models; 0 when the liveness frame has no face.
    fn liveness_score(&mut self, image: &RgbImage, face: &Face, liveness_image: Option<&RgbImage>) -> Result<f32> {
        let (live_image, live_face) = match liveness_image {
            Some(other) => match self.largest_face(other)? {
                Some(f) => (other, f),
                None => return Ok(0.0),
            },
            None => (image, face.clone()),
        };
        let mut total = 0.0;
        for model in &mut self.liveness {
            total += model.check(live_image, &live_face)?.score;
        }
        Ok(total / self.liveness.len() as f32)
    }

    /// Enroll from several frames; the stored template is the mean of their embeddings.
    pub fn enroll(&mut self, user_id: &str, images: &[RgbImage]) -> Result<usize> {
        let mut sum: Vec<f32> = Vec::new();
        let mut used = 0;
        for image in images {
            let Some(face) = self.largest_face(image)? else { continue };
            let emb = self.embedder.embed_face(image, &face)?;
            if sum.is_empty() {
                sum = vec![0.0; emb.features.len()];
            }
            sum.iter_mut().zip(&emb.features).for_each(|(s, f)| *s += f);
            used += 1;
        }
        if used == 0 {
            bail!("no face detected in any of the {} enrollment frames", images.len());
        }

        let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
        let features = sum.iter().map(|x| x / norm).collect();
        self.store.add(user_id.to_string(), Embedding { features });
        self.store.save().context("saving face store")?;
        info!(user_id, frames = used, "enrolled");
        Ok(used)
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
