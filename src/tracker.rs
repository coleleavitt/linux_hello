//! Follows faces across frame pairs, so that the prints and liveness votes of one face never
//! count for another (F37, F47): Windows keeps a state per tracked face, and a face that leaves
//! for more than 1000 ms is dropped (F37). Windows tracks by geometry alone; here a track also
//! ends when its face's print no longer matches the previous print of the track
//! ([`SAME_FACE_SIMILARITY`]), so a photo held exactly where the face was gets a track of its own.

use crate::{Embedding, Face};

/// A face continues the track when its box overlaps the last box by at least this IoU.
pub(crate) const MIN_IOU: f32 = 0.3;
/// Pairs without a face before the track is dropped: Windows purges a face after 1000 ms
/// without updates (F37), and one pair is two frames at 15 fps (133 ms).
const MAX_MISSED_PAIRS: u32 = 7;
/// A print below this cosine similarity to the track's last print is another face. 0.50 is the
/// calibration table's 1e-5-per-attempt point (per-comparison FAR 4e-8 on grayscale LFW, where
/// 2 % of genuine pairs, photos from different occasions, fall below it). Prints of one sitter
/// 67 ms apart score 0.957–0.986 on the saved frames, seconds apart 0.96–0.98, and the same
/// sitter in another sitting with the chin cut off 0.60; different people score ≤ 0.19
/// (`examples/embed_dir` on `validation_frames`).
pub const SAME_FACE_SIMILARITY: f32 = 0.5;

/// The track of the largest face.
#[derive(Debug, Default)]
pub struct FaceTracker {
    /// Last box of the current track
    last: Option<[f32; 4]>,
    /// Last print of the current track; stays while frames are rejected
    print: Option<Embedding>,
    missed: u32,
    /// Id of the current track; the first track is 1
    id: u32,
}

impl FaceTracker {
    /// Follow `face`, the largest face of a pair, with its print if the frame gave one. Returns
    /// its track id: a new one when the face does not overlap the last tracked box, the print does
    /// not match the track's last print, or the track was dropped; `None` without a face.
    pub fn update(&mut self, face: Option<&Face>, print: Option<&Embedding>) -> Option<u32> {
        let Some(face) = face else {
            self.missed += 1;
            if self.missed > MAX_MISSED_PAIRS {
                self.last = None;
                self.print = None;
            }
            return None;
        };
        let same_place = self.last.is_some_and(|last| iou(&last, &face.bbox) >= MIN_IOU);
        let same_face = match (&self.print, print) {
            (Some(last), Some(print)) => last.similarity(print) >= SAME_FACE_SIMILARITY,
            _ => true,
        };
        if !(same_place && same_face) {
            self.id += 1;
            self.print = None;
        }
        self.last = Some(face.bbox);
        if let Some(print) = print {
            self.print = Some(print.clone());
        }
        self.missed = 0;
        Some(self.id)
    }
}

/// Intersection over union of two `[x1, y1, x2, y2]` boxes.
pub(crate) fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let h = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = w * h;
    let union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter;
    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(bbox: [f32; 4]) -> Face {
        Face { bbox, score: 0.9, landmarks: [[0.0; 2]; 5] }
    }

    fn print(features: &[f32]) -> Embedding {
        Embedding { features: features.to_vec() }
    }

    #[test]
    fn box_overlap() {
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&[0.0, 0.0, 10.0, 10.0], &[20.0, 20.0, 30.0, 30.0]), 0.0);
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[5.0, 0.0, 15.0, 10.0]) - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn moving_face_keeps_its_track() {
        let mut t = FaceTracker::default();
        assert_eq!(t.update(Some(&face([100.0, 100.0, 200.0, 200.0])), None), Some(1));
        assert_eq!(t.update(Some(&face([110.0, 105.0, 210.0, 205.0])), None), Some(1));
        // A short dropout (blink, detector miss) keeps the track.
        assert_eq!(t.update(None, None), None);
        assert_eq!(t.update(Some(&face([115.0, 105.0, 215.0, 205.0])), None), Some(1));
    }

    #[test]
    fn other_face_or_long_absence_starts_a_new_track() {
        let mut t = FaceTracker::default();
        let owner = face([100.0, 100.0, 200.0, 200.0]);
        assert_eq!(t.update(Some(&owner), None), Some(1));
        assert_eq!(t.update(Some(&face([400.0, 100.0, 500.0, 200.0])), None), Some(2));
        assert_eq!(t.update(Some(&owner), None), Some(3));
        for _ in 0..=MAX_MISSED_PAIRS {
            assert_eq!(t.update(None, None), None);
        }
        assert_eq!(t.update(Some(&owner), None), Some(4));
    }

    #[test]
    fn another_print_in_the_same_place_starts_a_new_track() {
        let mut t = FaceTracker::default();
        let bbox = face([100.0, 100.0, 200.0, 200.0]);
        let (owner, photo) = (print(&[1.0, 0.0]), print(&[0.0, 1.0]));
        assert_eq!(t.update(Some(&bbox), Some(&owner)), Some(1));
        // The same face again, slightly different print (cosine 0.98); then frames without a
        // print (rejected) keep the track by geometry.
        assert_eq!(t.update(Some(&bbox), Some(&print(&[0.98, 0.199]))), Some(1));
        assert_eq!(t.update(Some(&bbox), None), Some(1));
        // A photo of someone else held where the face was: same box, cosine 0.2 to the last print.
        assert_eq!(t.update(Some(&bbox), Some(&photo)), Some(2));
        assert_eq!(t.update(Some(&bbox), Some(&photo)), Some(2));
        // The owner back in place: a new track again, not the old one.
        assert_eq!(t.update(Some(&bbox), Some(&owner)), Some(3));
    }
}
