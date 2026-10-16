//! Face templates: several prints per user, matched by the closest print (F28), and updated
//! after successful unlocks and, opt-in, after a failed face attempt confirmed by the password
//! (F31).

use serde::{Deserialize, Serialize};

use crate::Embedding;

/// Cosine similarity a probe must exceed to match a print. Calibrated on grayscale LFW (see
/// docs/calibration.md): one comparison FAR ≈ 4e-8 (tail fit), simulated per-attempt FAR ≈ 1.8e-6
/// for 15 prints × 16 frames (target 1e-5, as Windows' F27); FRR 0.23% per probe frame against
/// 15 prints.
pub const MATCH_THRESHOLD: f32 = 0.50;
/// A user is recognised once the session has this many prints and the best one matches (F48, F54).
pub const MIN_PRINTS: u32 = 4;
/// Augment only borderline matches: similarity in (MATCH_THRESHOLD, MATCH_THRESHOLD + this].
/// Windows augments when the estimated FAR is in [FAR/2, FAR] (F53); with a raw cosine
/// threshold, a fixed band above it stands in for that.
pub const AUGMENT_MARGIN: f32 = 0.05;
/// Highest generation of an augmented print: an augmented print may itself seed another one,
/// at most this many times in a row (F31).
pub const MAX_GENERATION: u8 = 3;
/// Augmented prints kept per user; the oldest is dropped first.
pub const MAX_AUGMENTED: usize = 10;
/// Enroll-on-failure (F31, `EnrollOnFailureRelaxedThresholdToMatch`): a print from a failed face
/// attempt that the user then confirmed with the password is added only if it is at least this
/// similar to the user's enrolled and augmented prints. Windows' value is policy data passed in at
/// runtime and not in the engine binary; 0.40 is the per-attempt FAR ≈ 1e-5 point of
/// docs/calibration.md (simulated 9.1e-6 for 15 prints × 16 frames on grayscale LFW), Windows'
/// own unlock target (F27). So someone who knows the password gets their face added about as
/// rarely as Windows would accept a stranger's face, while the owner's hard cases (0.40–0.50:
/// glasses, sunlight) are kept.
pub const ENROLL_ON_FAILURE_THRESHOLD: f32 = 0.40;
/// Enroll-on-failure prints kept per user; the oldest is dropped first.
pub const MAX_ON_FAILURE: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrintSource {
    Enroll,
    Augment,
    /// From a failed face attempt followed by the user's password (F31, flag 0x4)
    #[serde(rename = "on_failure")]
    OnFailure,
}

/// One stored face embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Print {
    pub features: Vec<f32>,
    /// 0 for enrolled prints; an augmented print is one more than the print it matched
    pub generation: u8,
    pub source: PrintSource,
}

/// The closest print of a template to a probe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrintMatch {
    pub similarity: f32,
    pub generation: u8,
}

impl PrintMatch {
    /// A match just above the threshold: the only kind that is added to the template (F53).
    pub fn is_borderline(&self) -> bool {
        self.similarity > MATCH_THRESHOLD && self.similarity <= MATCH_THRESHOLD + AUGMENT_MARGIN
    }
}

/// A user's prints.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Template {
    pub prints: Vec<Print>,
}

impl Template {
    /// One print per enrollment frame, without averaging.
    pub fn enrolled(embeddings: impl IntoIterator<Item = Embedding>) -> Self {
        let prints = embeddings
            .into_iter()
            .map(|e| Print { features: e.features, generation: 0, source: PrintSource::Enroll })
            .collect();
        Self { prints }
    }

    /// The highest cosine similarity over all prints (the minimum distance, F28).
    pub fn best_match(&self, probe: &Embedding) -> Option<PrintMatch> {
        self.best_match_where(probe, |_| true)
    }

    /// The highest cosine similarity over the prints that `keep` selects.
    pub fn best_match_where(&self, probe: &Embedding, keep: impl Fn(&Print) -> bool) -> Option<PrintMatch> {
        self.prints
            .iter()
            .filter(|p| keep(p))
            .map(|p| PrintMatch { similarity: crate::cosine(&p.features, &probe.features), generation: p.generation })
            .max_by(|a, b| a.similarity.total_cmp(&b.similarity))
    }

    /// Add `probe`, a live frame that matched a print of generation `matched.generation`
    /// (F31). Returns false when the new print would exceed [`MAX_GENERATION`].
    pub fn augment(&mut self, probe: &Embedding, matched: PrintMatch) -> bool {
        let generation = matched.generation + 1;
        if generation > MAX_GENERATION {
            return false;
        }
        self.push(probe, generation, PrintSource::Augment, MAX_AUGMENTED);
        true
    }

    /// Add `probe` from a failed face attempt confirmed by the password (F31). It gets the highest
    /// generation, so a later match against it never adds an augmented print: a face that only
    /// the password vouched for does not grow the template by itself.
    pub fn add_on_failure(&mut self, probe: &Embedding) {
        self.push(probe, MAX_GENERATION, PrintSource::OnFailure, MAX_ON_FAILURE);
    }

    /// Append a print, first dropping the oldest print of `source` if it already has `max`.
    fn push(&mut self, probe: &Embedding, generation: u8, source: PrintSource, max: usize) {
        if self.prints.iter().filter(|p| p.source == source).count() >= max {
            let oldest = self.prints.iter().position(|p| p.source == source).expect("counted above");
            self.prints.remove(oldest);
        }
        self.prints.push(Print { features: probe.features.clone(), generation, source });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emb(features: &[f32]) -> Embedding {
        Embedding { features: features.to_vec() }
    }

    #[test]
    fn best_match_is_the_max_over_prints() {
        let template = Template::enrolled([emb(&[1.0, 0.0]), emb(&[0.0, 1.0]), emb(&[0.6, 0.8])]);
        let m = template.best_match(&emb(&[0.0, 1.0])).unwrap();
        assert!((m.similarity - 1.0).abs() < 1e-6);
        assert_eq!(m.generation, 0);
        assert!(Template::default().best_match(&emb(&[1.0, 0.0])).is_none());
    }

    #[test]
    fn augmentation_caps_generation() {
        let mut template = Template::enrolled([emb(&[1.0, 0.0])]);
        let probe = emb(&[0.8, 0.6]);
        for generation in 0..MAX_GENERATION {
            assert!(template.augment(&probe, PrintMatch { similarity: 0.9, generation }));
            assert_eq!(template.prints.last().unwrap().generation, generation + 1);
        }
        assert!(!template.augment(&probe, PrintMatch { similarity: 0.9, generation: MAX_GENERATION }));
        assert_eq!(template.prints.len(), 1 + MAX_GENERATION as usize);
    }

    #[test]
    fn augmentation_evicts_the_oldest_augmented_print() {
        let mut template = Template::enrolled([emb(&[1.0, 0.0]), emb(&[0.0, 1.0])]);
        for i in 0..MAX_AUGMENTED + 2 {
            assert!(template.augment(&emb(&[i as f32, 0.0]), PrintMatch { similarity: 0.9, generation: 0 }));
        }
        let augmented: Vec<f32> =
            template.prints.iter().filter(|p| p.source == PrintSource::Augment).map(|p| p.features[0]).collect();
        assert_eq!(augmented, (2..MAX_AUGMENTED + 2).map(|i| i as f32).collect::<Vec<_>>());
        // Enrolled prints are never evicted.
        assert_eq!(template.prints.iter().filter(|p| p.source == PrintSource::Enroll).count(), 2);
    }

    #[test]
    fn on_failure_prints_are_capped_and_never_seed_augmentation() {
        let mut template = Template::enrolled([emb(&[1.0, 0.0])]);
        for i in 0..MAX_ON_FAILURE + 1 {
            template.add_on_failure(&emb(&[0.0, i as f32]));
        }
        let kept: Vec<f32> =
            template.prints.iter().filter(|p| p.source == PrintSource::OnFailure).map(|p| p.features[1]).collect();
        assert_eq!(kept, (1..=MAX_ON_FAILURE).map(|i| i as f32).collect::<Vec<_>>());
        let matched = template.best_match(&emb(&[0.0, 1.0])).unwrap();
        assert!(!template.augment(&emb(&[0.0, 1.0]), matched));
        // The relaxed check can leave them out.
        let enrolled = template.best_match_where(&emb(&[0.0, 1.0]), |p| p.source != PrintSource::OnFailure);
        assert_eq!(enrolled.unwrap().similarity, 0.0);
    }
}
