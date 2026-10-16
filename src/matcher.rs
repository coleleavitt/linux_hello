//! Face templates: several prints per user, matched by the closest print (F28) and recognised by
//! its estimated FAR (F27, F51), with Microsoft's recogniser as a second opinion when its
//! features are there, and updated after successful unlocks and, opt-in, after a failed face
//! attempt confirmed by the password (F31).

use serde::{Deserialize, Serialize};

use crate::calibration::{CalibrationTable, TARGET_FAR};
use crate::lighting::LightingStatus;
use crate::ms_models::recognizer::distance;
use crate::Embedding;

/// A user is recognised once the session has this many prints and the best one matches (F48, F54).
pub const MIN_PRINTS: u32 = 4;
/// Prints a user's template holds at most, enrolled and added together (F54: 100 in the v5
/// parameter block).
pub const MAX_PRINTS: usize = 100;
/// Highest generation of an augmented print: an augmented print may itself seed another one,
/// at most this many times in a row (F31).
pub const MAX_GENERATION: u8 = 3;
/// Augmented prints kept per user; the oldest is dropped first.
pub const MAX_AUGMENTED: usize = 10;
/// Enroll-on-failure (F31, `EnrollOnFailureRelaxedThresholdToMatch`): a print from a failed face
/// attempt that the user then confirmed with the password is added only if it is at least this
/// similar to the user's enrolled and augmented prints. Windows' value is policy data passed in at
/// runtime and not in the engine binary; 0.40 is the simulated per-attempt FAR ≈ 1e-5 point of
/// docs/calibration.md (9.1e-6 for 15 prints × 16 frames on grayscale LFW), Windows' own unlock
/// target (F27); the union bound that [`TARGET_FAR`] is checked against gives 0.50. So someone
/// who knows the password gets their face added about as rarely as Windows would accept a
/// stranger's face, while the owner's hard cases (0.40–0.50: glasses, sunlight) are kept.
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
    /// Microsoft's recogniser's feature of the same frame ([`Embedding::windows`]); prints stored
    /// without the Windows Hello models have none
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<f32>>,
    /// 0 for enrolled prints; an augmented print is one more than the print it matched
    pub generation: u8,
    pub source: PrintSource,
}

impl Print {
    fn new(embedding: Embedding, generation: u8, source: PrintSource) -> Self {
        Self { features: embedding.features, windows: embedding.windows, generation, source }
    }
}

/// Verification condition of a comparison (F67): Windows derives it from the quality flags of
/// the probe frame and of the matched print — weak lighting (F44 status 4/5), a clipped band
/// (status 7), a non-frontal pose or an edge cut make the condition worse than clean — and
/// takes the calibration table of that condition (the five tables of the v5 model, identical
/// in the shipped build) and the best GFP per condition. Print sources (enrolled, on-failure,
/// on-success) play no part in it. Here every stored print comes from a good-lighting frame
/// (enrollment, augmentation and enroll-on-failure take live frames only), so the condition
/// is the probe frame's; non-frontal and edge-cut frames give no print (F46).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Condition {
    /// Good lighting (Windows condition 0)
    Clean,
    /// Weak lighting or a clipped band around the face: print kept, no vote (Windows
    /// condition 1, print flags 0x1 and 0x20)
    Weak,
}

impl Condition {
    /// The condition of a print taken with this lighting status (F45: statuses 4, 5 and 7
    /// tag the print; the others give a clean print or none).
    pub fn of(status: LightingStatus) -> Self {
        match status {
            LightingStatus::Weak | LightingStatus::ClippedAround => Condition::Weak,
            _ => Condition::Clean,
        }
    }
}

/// Microsoft's recogniser's opinion of a probe: its engine distance to the closest of the
/// compared prints that carry a Windows feature (F28, F50) and Microsoft's FAR at that distance
/// (F51).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowsMatch {
    pub distance: f32,
    pub far: f64,
}

/// The closest print of a template to a probe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrintMatch {
    pub similarity: f32,
    /// Estimated FAR of an attempt against the compared prints whose best comparison is this one
    /// ([`CalibrationTable::far`])
    pub far: f64,
    /// The verification condition the FAR was read under (F67); with one calibration table per
    /// camera it changes no number, as in the shipped Windows build
    pub condition: Condition,
    pub generation: u8,
    /// The second opinion, when both the probe and a compared print have a Windows feature
    pub windows: Option<WindowsMatch>,
}

impl PrintMatch {
    /// Windows recognises a face whose estimated FAR is below the target (F27); with a second
    /// opinion, Microsoft's FAR must be below the target too.
    pub fn recognised(&self) -> bool {
        self.far < TARGET_FAR && self.windows.is_none_or(|w| w.far < TARGET_FAR)
    }

    /// A match just under the target, FAR in [target/2, target): the only kind that is added to
    /// the template (F53).
    pub fn is_borderline(&self) -> bool {
        self.far >= TARGET_FAR / 2.0 && self.recognised()
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
        let prints = embeddings.into_iter().map(|e| Print::new(e, 0, PrintSource::Enroll)).collect();
        Self { prints }
    }

    /// Append enrolled prints (`enroll --add`, Windows' "improve recognition"), up to [`MAX_PRINTS`]
    /// in all; returns how many were taken.
    pub fn add_enrolled(&mut self, embeddings: impl IntoIterator<Item = Embedding>) -> usize {
        let before = self.prints.len();
        self.prints.extend(embeddings.into_iter().take(self.room()).map(|e| Print::new(e, 0, PrintSource::Enroll)));
        self.prints.len() - before
    }

    /// Prints that can still be added before [`MAX_PRINTS`].
    pub fn room(&self) -> usize {
        MAX_PRINTS.saturating_sub(self.prints.len())
    }

    /// The highest cosine similarity over all prints (the minimum distance, F28), with the FAR of
    /// an attempt against this many prints from `table`, under `condition` (F67). With `windows`,
    /// Microsoft's table, and a probe with a Windows feature, also the second opinion: the
    /// minimum engine distance over the prints with one (Windows' own rule, F28) and Microsoft's
    /// FAR for it. That FAR is the table's value as Windows uses it (`GFP`, F51), not scaled by
    /// the comparison count: Windows' × 1…4 is the number of enrolled users, capped (F67), and
    /// there is one here.
    pub fn best_match(
        &self,
        probe: &Embedding,
        condition: Condition,
        table: &CalibrationTable,
        windows: Option<&CalibrationTable>,
    ) -> Option<PrintMatch> {
        self.best_match_where(probe, condition, table, windows, |_| true)
    }

    /// The highest cosine similarity over the prints that `keep` selects.
    pub fn best_match_where(
        &self,
        probe: &Embedding,
        condition: Condition,
        table: &CalibrationTable,
        windows: Option<&CalibrationTable>,
        keep: impl Fn(&Print) -> bool,
    ) -> Option<PrintMatch> {
        let compared: Vec<&Print> = self.prints.iter().filter(|p| keep(p)).collect();
        let (similarity, generation) = compared
            .iter()
            .map(|p| (crate::cosine(&p.features, &probe.features), p.generation))
            .max_by(|a, b| a.0.total_cmp(&b.0))?;
        let windows = match (windows, &probe.windows) {
            (Some(table), Some(feature)) => compared
                .iter()
                .filter_map(|p| p.windows.as_ref())
                .map(|w| distance(w, feature))
                .min_by(f32::total_cmp)
                .map(|distance| WindowsMatch { distance, far: table.pair_far(1.0 - distance) }),
            _ => None,
        };
        Some(PrintMatch { similarity, far: table.far(similarity, compared.len()), condition, generation, windows })
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
        self.prints.push(Print::new(probe.clone(), generation, source));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emb(features: &[f32]) -> Embedding {
        Embedding { features: features.to_vec(), windows: None }
    }

    fn matched(similarity: f32, generation: u8) -> PrintMatch {
        let far = crate::calibration::lfw_gray().far(similarity, 15);
        PrintMatch { similarity, far, condition: Condition::Clean, generation, windows: None }
    }

    /// Microsoft's v5 table around its FAR 1e-5 point (distance 0.350).
    fn windows_table() -> CalibrationTable {
        let rows = [(2.03519e-06, 0.995666, 0.305), (1e-05, 0.997664, 0.350), (3.08245e-05, 0.998230, 0.381)];
        CalibrationTable::new("v5", rows.map(|(fpr, tar, d): (f64, f64, f32)| (fpr, tar, 1.0 - d))).unwrap()
    }

    #[test]
    fn recognised_and_borderline_follow_the_far() {
        // The shipped table: 0.50 ↔ FAR 1e-5, FAR 5e-6 at about 0.52.
        assert!(!matched(0.5, 0).recognised());
        assert!(matched(0.51, 0).recognised() && matched(0.51, 0).is_borderline());
        assert!(matched(0.53, 0).recognised() && !matched(0.53, 0).is_borderline());
        assert!(matched(0.9, 0).recognised() && !matched(0.9, 0).is_borderline());
        assert!(!matched(0.45, 0).is_borderline());
    }

    #[test]
    fn condition_follows_the_probe_frame_and_is_recorded() {
        assert_eq!(Condition::of(LightingStatus::Good), Condition::Clean);
        assert_eq!(Condition::of(LightingStatus::Weak), Condition::Weak);
        assert_eq!(Condition::of(LightingStatus::ClippedAround), Condition::Weak);
        let table = crate::calibration::lfw_gray();
        let template = Template::enrolled([emb(&[1.0, 0.0])]);
        let weak = template.best_match(&emb(&[0.8, 0.6]), Condition::Weak, &table, None).unwrap();
        let clean = template.best_match(&emb(&[0.8, 0.6]), Condition::Clean, &table, None).unwrap();
        assert_eq!(weak.condition, Condition::Weak);
        // One table per camera, as the five identical tables of the v5 model: the same FAR.
        assert_eq!((weak.similarity, weak.far), (clean.similarity, clean.far));
    }

    #[test]
    fn best_match_is_the_max_over_prints() {
        let table = crate::calibration::lfw_gray();
        let template = Template::enrolled([emb(&[1.0, 0.0]), emb(&[0.0, 1.0]), emb(&[0.6, 0.8])]);
        let m = template.best_match(&emb(&[0.0, 1.0]), Condition::Clean, &table, None).unwrap();
        assert!((m.similarity - 1.0).abs() < 1e-6);
        assert_eq!(m.far, table.far(m.similarity, 3));
        assert_eq!(m.generation, 0);
        assert_eq!(m.windows, None);
        assert!(Template::default().best_match(&emb(&[1.0, 0.0]), Condition::Clean, &table, None).is_none());
    }

    #[test]
    fn windows_opinion_is_the_min_distance_and_must_agree() {
        let table = crate::calibration::lfw_gray();
        let windows = windows_table();
        let with = |features: &[f32], w: &[f32]| Embedding { features: features.to_vec(), windows: Some(w.to_vec()) };
        // ArcFace is sure (0.9); the Windows features of the two prints are at distance 0.5 and 0.3.
        let template = Template::enrolled([with(&[0.9, 0.436], &[0.5, 0.866]), with(&[1.0, 0.0], &[0.7, 0.714])]);
        let probe = with(&[1.0, 0.0], &[1.0, 0.0]);
        let m = template.best_match(&probe, Condition::Clean, &table, Some(&windows)).unwrap();
        let w = m.windows.unwrap();
        assert!((w.distance - 0.3).abs() < 1e-3, "closest Windows feature, not the ArcFace-best print's");
        assert!(w.far < TARGET_FAR && m.far < TARGET_FAR && m.recognised());
        // Distance 0.5 alone: Microsoft's table says FAR > 1e-5, so the face is not recognised.
        let far = Template::enrolled([with(&[1.0, 0.0], &[0.5, 0.866])]);
        let m = far.best_match(&probe, Condition::Clean, &table, Some(&windows)).unwrap();
        assert!(m.far < TARGET_FAR && m.windows.unwrap().far > TARGET_FAR && !m.recognised());
        assert!(!m.is_borderline());
        // No second opinion without Microsoft's table, without a probe feature, or against prints
        // without one; then ArcFace decides alone.
        assert!(far.best_match(&probe, Condition::Clean, &table, None).unwrap().recognised());
        assert!(far.best_match(&emb(&[1.0, 0.0]), Condition::Clean, &table, Some(&windows)).unwrap().recognised());
        let old = Template::enrolled([emb(&[1.0, 0.0])]);
        let m = old.best_match(&probe, Condition::Clean, &table, Some(&windows)).unwrap();
        assert!(m.windows.is_none() && m.recognised());
    }

    #[test]
    fn prints_keep_the_windows_feature() {
        let with = |w: Option<&[f32]>| Embedding { features: vec![1.0, 0.0], windows: w.map(<[f32]>::to_vec) };
        let mut template = Template::enrolled([with(Some(&[0.0, 1.0])), with(None)]);
        template.add_enrolled([with(Some(&[1.0, 0.0]))]);
        template.augment(&with(Some(&[0.6, 0.8])), matched(0.9, 0));
        template.add_on_failure(&with(None));
        let windows: Vec<Option<f32>> = template.prints.iter().map(|p| p.windows.as_ref().map(|w| w[0])).collect();
        assert_eq!(windows, [Some(0.0), None, Some(1.0), Some(0.6), None]);
        // Serialised, a print without the feature has no `windows` key, as stores from before it.
        let json = serde_json::to_string(&template).unwrap();
        assert_eq!(json.matches("\"windows\"").count(), 3);
        let back: Template = serde_json::from_str(&json).unwrap();
        assert_eq!(back.prints[1].windows, None);
        assert_eq!(back.prints[3].windows.as_deref(), Some(&[0.6, 0.8][..]));
    }

    #[test]
    fn added_prints_stop_at_the_per_user_maximum() {
        let mut template = Template::enrolled((0..MAX_PRINTS - 2).map(|i| emb(&[i as f32, 1.0])));
        assert_eq!(template.room(), 2);
        assert_eq!(template.add_enrolled([emb(&[1.0, 0.0]), emb(&[0.0, 1.0]), emb(&[0.5, 0.5])]), 2);
        assert_eq!(template.prints.len(), MAX_PRINTS);
        assert_eq!(template.room(), 0);
        assert_eq!(template.add_enrolled([emb(&[1.0, 0.0])]), 0);
        let last = template.prints.last().unwrap();
        assert_eq!((last.features[1], last.generation, last.source), (1.0, 0, PrintSource::Enroll));
    }

    #[test]
    fn augmentation_caps_generation() {
        let mut template = Template::enrolled([emb(&[1.0, 0.0])]);
        let probe = emb(&[0.8, 0.6]);
        for generation in 0..MAX_GENERATION {
            assert!(template.augment(&probe, matched(0.9, generation)));
            assert_eq!(template.prints.last().unwrap().generation, generation + 1);
        }
        assert!(!template.augment(&probe, matched(0.9, MAX_GENERATION)));
        assert_eq!(template.prints.len(), 1 + MAX_GENERATION as usize);
    }

    #[test]
    fn augmentation_evicts_the_oldest_augmented_print() {
        let mut template = Template::enrolled([emb(&[1.0, 0.0]), emb(&[0.0, 1.0])]);
        for i in 0..MAX_AUGMENTED + 2 {
            assert!(template.augment(&emb(&[i as f32, 0.0]), matched(0.9, 0)));
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
        let table = crate::calibration::lfw_gray();
        let best = template.best_match(&emb(&[0.0, 1.0]), Condition::Clean, &table, None).unwrap();
        assert!(!template.augment(&emb(&[0.0, 1.0]), best));
        // The relaxed check can leave them out.
        let enrolled = template.best_match_where(&emb(&[0.0, 1.0]), Condition::Clean, &table, None, |p| {
            p.source != PrintSource::OnFailure
        });
        assert_eq!(enrolled.unwrap().similarity, 0.0);
    }
}
