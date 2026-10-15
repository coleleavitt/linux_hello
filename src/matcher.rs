//! Face templates: several prints per user, matched by the closest print (F28), and updated
//! after successful unlocks (F31).

use serde::{Deserialize, Serialize};

use crate::Embedding;

/// Cosine similarity a probe must exceed to match a print. Calibrated on grayscale LFW (see
/// docs/calibration.md): one comparison FAR ≈ 4e-8 (tail fit), simulated per-attempt FAR ≈ 1.8e-6
/// for 15 prints × 16 frames (target 1e-5, as Windows' F27); FRR 0.23% per probe frame against
/// 15 prints.
pub const MATCH_THRESHOLD: f32 = 0.50;
/// Highest generation of an augmented print: an augmented print may itself seed another one,
/// at most this many times in a row (F31).
pub const MAX_GENERATION: u8 = 3;
/// Augmented prints kept per user; the oldest is dropped first.
pub const MAX_AUGMENTED: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrintSource {
    Enroll,
    Augment,
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
        self.prints
            .iter()
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
        let augmented = self.prints.iter().filter(|p| p.source == PrintSource::Augment).count();
        if augmented >= MAX_AUGMENTED {
            let oldest = self.prints.iter().position(|p| p.source == PrintSource::Augment).expect("counted above");
            self.prints.remove(oldest);
        }
        self.prints.push(Print { features: probe.features.clone(), generation, source: PrintSource::Augment });
        true
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
}
