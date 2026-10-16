//! Follows every analysed face across frame pairs, so that the prints and liveness votes of one
//! face never count for another (F37, F47): Windows keeps a state per tracked face, and a face
//! that leaves for more than 1000 ms is dropped (F37). Windows tracks by geometry alone; here a
//! face also leaves its track when its print no longer matches the track's last print
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

/// The tracks of the faces seen in the last pairs.
#[derive(Debug, Default)]
pub struct FaceTracker {
    tracks: Vec<Track>,
    /// Id of the last track started; the first track is 1
    last_id: u32,
}

#[derive(Debug)]
struct Track {
    id: u32,
    /// Last box of the track
    bbox: [f32; 4],
    /// Last print of the track; stays while its frames are rejected
    print: Option<Embedding>,
    /// Pairs since the track's face was last seen
    missed: u32,
}

impl Track {
    /// Whether `face` with `print` continues this track: same place, and the same face when both
    /// have a print.
    fn accepts(&self, face: &Face, print: Option<&Embedding>) -> bool {
        let same_face = match (&self.print, print) {
            (Some(last), Some(print)) => last.similarity(print) >= SAME_FACE_SIMILARITY,
            _ => true,
        };
        same_face && iou(&self.bbox, &face.bbox) >= MIN_IOU
    }
}

impl FaceTracker {
    /// Follow the faces of a pair, largest first, each with its print if the frame gave one.
    /// Returns the track id of each face, in order. A face continues the track whose last box
    /// it overlaps best (IoU ≥ [`MIN_IOU`]) among the tracks not yet continued this pair whose
    /// last print it matches ([`SAME_FACE_SIMILARITY`]); otherwise it starts a new track. A track
    /// without a face for more than [`MAX_MISSED_PAIRS`] pairs is dropped.
    pub fn update(&mut self, faces: &[(&Face, Option<&Embedding>)]) -> Vec<u32> {
        let mut taken: Vec<usize> = Vec::with_capacity(faces.len());
        let mut ids = Vec::with_capacity(faces.len());
        for &(face, print) in faces {
            let best =
                (0..self.tracks.len()).filter(|i| !taken.contains(i) && self.tracks[*i].accepts(face, print)).max_by(
                    |a, b| iou(&self.tracks[*a].bbox, &face.bbox).total_cmp(&iou(&self.tracks[*b].bbox, &face.bbox)),
                );
            let i = best.unwrap_or_else(|| {
                self.last_id += 1;
                self.tracks.push(Track { id: self.last_id, bbox: face.bbox, print: None, missed: 0 });
                self.tracks.len() - 1
            });
            let track = &mut self.tracks[i];
            track.bbox = face.bbox;
            if let Some(print) = print {
                track.print = Some(print.clone());
            }
            track.missed = 0;
            taken.push(i);
            ids.push(track.id);
        }
        for (i, track) in self.tracks.iter_mut().enumerate() {
            if !taken.contains(&i) {
                track.missed += 1;
            }
        }
        self.tracks.retain(|t| t.missed <= MAX_MISSED_PAIRS);
        ids
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

    /// One face without a print.
    fn one(t: &mut FaceTracker, face: Option<&Face>) -> Option<u32> {
        t.update(&face.map(|f| (f, None)).into_iter().collect::<Vec<_>>()).pop()
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
        assert_eq!(one(&mut t, Some(&face([100.0, 100.0, 200.0, 200.0]))), Some(1));
        assert_eq!(one(&mut t, Some(&face([110.0, 105.0, 210.0, 205.0]))), Some(1));
        // A short dropout (blink, detector miss) keeps the track.
        assert_eq!(one(&mut t, None), None);
        assert_eq!(one(&mut t, Some(&face([115.0, 105.0, 215.0, 205.0]))), Some(1));
    }

    #[test]
    fn other_face_or_long_absence_starts_a_new_track() {
        let mut t = FaceTracker::default();
        let owner = face([100.0, 100.0, 200.0, 200.0]);
        assert_eq!(one(&mut t, Some(&owner)), Some(1));
        // Elsewhere in the frame: a new track; the first one lives on for a while, so the owner
        // back in place continues it.
        assert_eq!(one(&mut t, Some(&face([400.0, 100.0, 500.0, 200.0]))), Some(2));
        assert_eq!(one(&mut t, Some(&owner)), Some(1));
        for _ in 0..=MAX_MISSED_PAIRS {
            assert_eq!(one(&mut t, None), None);
        }
        assert_eq!(one(&mut t, Some(&owner)), Some(3));
    }

    #[test]
    fn another_print_in_the_same_place_starts_a_new_track() {
        let mut t = FaceTracker::default();
        let bbox = face([100.0, 100.0, 200.0, 200.0]);
        let (owner, photo) = (print(&[1.0, 0.0]), print(&[0.0, 1.0]));
        assert_eq!(t.update(&[(&bbox, Some(&owner))]), vec![1]);
        // The same face again, slightly different print (cosine 0.98); then frames without a
        // print (rejected) keep the track by place.
        assert_eq!(t.update(&[(&bbox, Some(&print(&[0.98, 0.199])))]), vec![1]);
        assert_eq!(t.update(&[(&bbox, None)]), vec![1]);
        // A photo of someone else held where the face was: same box, cosine 0.2 to the last print.
        assert_eq!(t.update(&[(&bbox, Some(&photo))]), vec![2]);
        assert_eq!(t.update(&[(&bbox, Some(&photo))]), vec![2]);
        // The owner back in place continues the owner's track, which has not been dropped yet.
        assert_eq!(t.update(&[(&bbox, Some(&owner))]), vec![1]);
    }

    #[test]
    fn two_faces_keep_two_tracks() {
        let mut t = FaceTracker::default();
        let (a, b) = (face([100.0, 100.0, 200.0, 200.0]), face([400.0, 100.0, 480.0, 180.0]));
        let (pa, pb) = (print(&[1.0, 0.0]), print(&[0.0, 1.0]));
        assert_eq!(t.update(&[(&a, Some(&pa)), (&b, Some(&pb))]), vec![1, 2]);
        // The second face grows past the first: the order changes, the ids do not.
        let b2 = face([390.0, 90.0, 500.0, 200.0]);
        assert_eq!(t.update(&[(&b2, Some(&pb)), (&a, Some(&pa))]), vec![2, 1]);
        // Face b moves onto a's place while a is gone: a's track is taken only by a's print.
        let b3 = face([105.0, 100.0, 205.0, 200.0]);
        assert_eq!(t.update(&[(&b3, Some(&pb))]), vec![3]);
        assert_eq!(t.update(&[(&b3, None)]), vec![3]);
        // Without prints on either side, place decides: the best overlap among the free tracks.
        let mut g = FaceTracker::default();
        assert_eq!(g.update(&[(&a, None), (&b, None)]), vec![1, 2]);
        assert_eq!(
            g.update(&[(&face([101.0, 100.0, 201.0, 200.0]), None), (&face([102.0, 102.0, 202.0, 202.0]), None)]),
            vec![1, 3]
        );
    }
}
