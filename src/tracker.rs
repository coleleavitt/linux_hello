//! Follows the largest face across frame pairs, so that the prints and liveness votes of one
//! face never count for another (F37, F47): Windows keeps a state per tracked face, and a face
//! that leaves for more than 1000 ms is dropped (F37).

use crate::Face;

/// A face continues the track when its box overlaps the last box by at least this IoU.
const MIN_IOU: f32 = 0.3;
/// Pairs without a face before the track is dropped: Windows purges a face after 1000 ms
/// without updates (F37), and one pair is two frames at 15 fps (133 ms).
const MAX_MISSED_PAIRS: u32 = 7;

/// The track of the largest face.
#[derive(Debug, Default)]
pub struct FaceTracker {
    /// Last box of the current track
    last: Option<[f32; 4]>,
    missed: u32,
    /// Id of the current track; the first track is 1
    id: u32,
}

impl FaceTracker {
    /// Follow `face`, the largest face of a pair. Returns its track id (a new one when the face
    /// does not overlap the last tracked box, or the track was dropped), or `None` without a face.
    pub fn update(&mut self, face: Option<&Face>) -> Option<u32> {
        let Some(face) = face else {
            self.missed += 1;
            if self.missed > MAX_MISSED_PAIRS {
                self.last = None;
            }
            return None;
        };
        if self.last.is_none_or(|last| iou(&last, &face.bbox) < MIN_IOU) {
            self.id += 1;
        }
        self.last = Some(face.bbox);
        self.missed = 0;
        Some(self.id)
    }
}

/// Intersection over union of two `[x1, y1, x2, y2]` boxes.
fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
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

    #[test]
    fn box_overlap() {
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&[0.0, 0.0, 10.0, 10.0], &[20.0, 20.0, 30.0, 30.0]), 0.0);
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[5.0, 0.0, 15.0, 10.0]) - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn moving_face_keeps_its_track() {
        let mut t = FaceTracker::default();
        assert_eq!(t.update(Some(&face([100.0, 100.0, 200.0, 200.0]))), Some(1));
        assert_eq!(t.update(Some(&face([110.0, 105.0, 210.0, 205.0]))), Some(1));
        // A short dropout (blink, detector miss) keeps the track.
        assert_eq!(t.update(None), None);
        assert_eq!(t.update(Some(&face([115.0, 105.0, 215.0, 205.0]))), Some(1));
    }

    #[test]
    fn other_face_or_long_absence_starts_a_new_track() {
        let mut t = FaceTracker::default();
        let owner = face([100.0, 100.0, 200.0, 200.0]);
        assert_eq!(t.update(Some(&owner)), Some(1));
        assert_eq!(t.update(Some(&face([400.0, 100.0, 500.0, 200.0]))), Some(2));
        assert_eq!(t.update(Some(&owner)), Some(3));
        for _ in 0..=MAX_MISSED_PAIRS {
            assert_eq!(t.update(None), None);
        }
        assert_eq!(t.update(Some(&owner)), Some(4));
    }
}
