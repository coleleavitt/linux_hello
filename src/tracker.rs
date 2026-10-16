//! Simple IoU-based face tracker for persistent face IDs across frames.
//!
//! Needed for:
//! - Pairing ambient and lit frames for the same face
//! - Counting prints per track (need >= 4 before deciding)
//! - Associating liveness votes with the correct face

use crate::Face;

/// Minimum IoU to match a detection to an existing track.
const MATCH_IOU_THRESHOLD: f32 = 0.3;
/// Consecutive frames to confirm a tentative track.
const CONFIRM_FRAMES: u32 = 3;
/// Frames without a match before a track is marked lost.
const LOST_FRAMES: u32 = 5;

/// State of a face track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackState {
    /// New track, not yet confirmed (needs CONFIRM_FRAMES consecutive detections).
    Tentative,
    /// Confirmed track with stable identity.
    Confirmed,
    /// Track lost (not seen for LOST_FRAMES).
    Lost,
}

/// A tracked face with persistent identity across frames.
#[derive(Debug, Clone)]
pub struct Track {
    /// Unique track ID (monotonically increasing).
    pub id: u64,
    /// Last observed bounding box [x1, y1, x2, y2].
    pub last_box: [f32; 4],
    /// Last observed detection score.
    pub last_score: f32,
    /// Last observed landmarks.
    pub last_landmarks: [[f32; 2]; 5],
    /// Frame number when last seen.
    pub last_seen_frame: u64,
    /// Current state of the track.
    pub state: TrackState,
    /// Consecutive frames this track has been seen (resets on miss).
    consecutive_hits: u32,
    /// Frames since last seen (0 when just seen).
    frames_since_seen: u32,
}

impl Track {
    /// Create a new tentative track from a detection.
    fn new(id: u64, face: &Face, frame: u64) -> Self {
        Self {
            id,
            last_box: face.bbox,
            last_score: face.score,
            last_landmarks: face.landmarks,
            last_seen_frame: frame,
            state: TrackState::Tentative,
            consecutive_hits: 1,
            frames_since_seen: 0,
        }
    }

    /// Update the track with a new detection.
    fn update(&mut self, face: &Face, frame: u64) {
        self.last_box = face.bbox;
        self.last_score = face.score;
        self.last_landmarks = face.landmarks;
        self.last_seen_frame = frame;
        self.frames_since_seen = 0;
        self.consecutive_hits += 1;

        // Promote to confirmed after enough consecutive hits
        if self.state == TrackState::Tentative && self.consecutive_hits >= CONFIRM_FRAMES {
            self.state = TrackState::Confirmed;
        }
    }

    /// Mark the track as not seen this frame.
    fn mark_missed(&mut self) {
        self.frames_since_seen += 1;
        self.consecutive_hits = 0;

        if self.frames_since_seen >= LOST_FRAMES {
            self.state = TrackState::Lost;
        }
    }

    /// Convert this track back to a Face (using last known state).
    pub fn to_face(&self) -> Face {
        Face {
            bbox: self.last_box,
            score: self.last_score,
            landmarks: self.last_landmarks,
        }
    }

    /// Whether this track is active (not lost).
    pub fn is_active(&self) -> bool {
        self.state != TrackState::Lost
    }

    /// Whether this track is confirmed (stable identity).
    pub fn is_confirmed(&self) -> bool {
        self.state == TrackState::Confirmed
    }
}

/// Simple IoU-based face tracker.
///
/// Maintains face tracks across frames, assigning persistent IDs to faces.
#[derive(Debug)]
pub struct FaceTracker {
    /// Active and recent tracks.
    tracks: Vec<Track>,
    /// Next track ID to assign.
    next_id: u64,
    /// Current frame number.
    current_frame: u64,
}

impl Default for FaceTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FaceTracker {
    /// Create a new tracker.
    pub fn new() -> Self {
        Self {
            tracks: Vec::new(),
            next_id: 1,
            current_frame: 0,
        }
    }

    /// Update tracks with new detections for the current frame.
    ///
    /// Returns the tracks that matched (or were created from) the input faces,
    /// in the same order as the input faces.
    pub fn update(&mut self, faces: &[Face]) -> Vec<&Track> {
        self.current_frame += 1;
        let frame = self.current_frame;

        // Compute IoU matrix between detections and active tracks
        let active_tracks: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_active())
            .map(|(i, _)| i)
            .collect();

        // Greedy matching: for each detection, find best matching track
        let mut matched_tracks: Vec<Option<usize>> = vec![None; faces.len()];
        let mut used_tracks: Vec<bool> = vec![false; active_tracks.len()];

        // Build IoU scores
        let mut scores: Vec<(usize, usize, f32)> = Vec::new();
        for (di, face) in faces.iter().enumerate() {
            for (ti_idx, &ti) in active_tracks.iter().enumerate() {
                let iou = box_iou(&face.bbox, &self.tracks[ti].last_box);
                if iou >= MATCH_IOU_THRESHOLD {
                    scores.push((di, ti_idx, iou));
                }
            }
        }

        // Sort by IoU descending for greedy matching
        scores.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

        // Greedy assignment
        for (di, ti_idx, _iou) in scores {
            if matched_tracks[di].is_none() && !used_tracks[ti_idx] {
                matched_tracks[di] = Some(active_tracks[ti_idx]);
                used_tracks[ti_idx] = true;
            }
        }

        // Update matched tracks, mark unmatched as missed
        for (ti_idx, &ti) in active_tracks.iter().enumerate() {
            if !used_tracks[ti_idx] {
                self.tracks[ti].mark_missed();
            }
        }

        // Update matched tracks and create new tracks for unmatched detections
        let mut result_indices: Vec<usize> = Vec::with_capacity(faces.len());
        for (di, face) in faces.iter().enumerate() {
            match matched_tracks[di] {
                Some(ti) => {
                    self.tracks[ti].update(face, frame);
                    result_indices.push(ti);
                }
                None => {
                    // New track
                    let id = self.next_id;
                    self.next_id += 1;
                    self.tracks.push(Track::new(id, face, frame));
                    result_indices.push(self.tracks.len() - 1);
                }
            }
        }

        // Prune lost tracks to keep memory bounded
        self.prune_lost_tracks();

        // Return references to the matched/created tracks
        result_indices
            .iter()
            .map(|&i| &self.tracks[i])
            .collect()
    }

    /// Get all active (non-lost) tracks.
    pub fn active_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.is_active())
    }

    /// Get all confirmed tracks.
    pub fn confirmed_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.is_confirmed())
    }

    /// Get a track by ID.
    pub fn get_track(&self, id: u64) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Get a mutable track by ID.
    pub fn get_track_mut(&mut self, id: u64) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Current frame number.
    pub fn frame_count(&self) -> u64 {
        self.current_frame
    }

    /// Reset the tracker (clear all tracks).
    pub fn reset(&mut self) {
        self.tracks.clear();
        self.next_id = 1;
        self.current_frame = 0;
    }

    /// Remove tracks that have been lost for a while.
    fn prune_lost_tracks(&mut self) {
        // Keep lost tracks for a few more frames in case they reappear,
        // but eventually remove them to bound memory
        const PRUNE_AFTER_LOST: u32 = 10;
        self.tracks.retain(|t| {
            t.state != TrackState::Lost || t.frames_since_seen < PRUNE_AFTER_LOST
        });
    }
}

/// Compute IoU (Intersection over Union) between two bounding boxes.
fn box_iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x1 = a[0].max(b[0]);
    let y1 = a[1].max(b[1]);
    let x2 = a[2].min(b[2]);
    let y2 = a[3].min(b[3]);

    let w = (x2 - x1).max(0.0);
    let h = (y2 - y1).max(0.0);
    let inter = w * h;

    let area_a = (a[2] - a[0]) * (a[3] - a[1]);
    let area_b = (b[2] - b[0]) * (b[3] - b[1]);
    let union = area_a + area_b - inter;

    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_face(bbox: [f32; 4]) -> Face {
        Face {
            bbox,
            score: 0.9,
            landmarks: [[0.0; 2]; 5],
        }
    }

    #[test]
    fn test_box_iou() {
        // Perfect overlap
        assert!((box_iou(&[0.0, 0.0, 10.0, 10.0], &[0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-6);

        // No overlap
        assert!(box_iou(&[0.0, 0.0, 10.0, 10.0], &[20.0, 20.0, 30.0, 30.0]).abs() < 1e-6);

        // Partial overlap (50% intersection)
        let iou = box_iou(&[0.0, 0.0, 10.0, 10.0], &[5.0, 0.0, 15.0, 10.0]);
        assert!((iou - 1.0 / 3.0).abs() < 1e-6); // inter=50, union=150
    }

    #[test]
    fn test_new_track_tentative() {
        let mut tracker = FaceTracker::new();
        let faces = [make_face([100.0, 100.0, 200.0, 200.0])];

        let tracks = tracker.update(&faces);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].state, TrackState::Tentative);
        assert_eq!(tracks[0].id, 1);
    }

    #[test]
    fn test_track_confirmation() {
        let mut tracker = FaceTracker::new();
        let face = make_face([100.0, 100.0, 200.0, 200.0]);

        // Frame 1: tentative
        let tracks = tracker.update(std::slice::from_ref(&face));
        assert_eq!(tracks[0].state, TrackState::Tentative);
        let id = tracks[0].id;

        // Frame 2: still tentative
        let tracks = tracker.update(std::slice::from_ref(&face));
        assert_eq!(tracks[0].id, id);
        assert_eq!(tracks[0].state, TrackState::Tentative);

        // Frame 3: confirmed (3 consecutive hits)
        let tracks = tracker.update(std::slice::from_ref(&face));
        assert_eq!(tracks[0].id, id);
        assert_eq!(tracks[0].state, TrackState::Confirmed);
    }

    #[test]
    fn test_track_lost() {
        let mut tracker = FaceTracker::new();
        let face = make_face([100.0, 100.0, 200.0, 200.0]);

        // Establish a track
        for _ in 0..3 {
            tracker.update(std::slice::from_ref(&face));
        }
        let track = &tracker.tracks[0];
        assert_eq!(track.state, TrackState::Confirmed);

        // 5 frames without detection -> lost
        for i in 0..5 {
            tracker.update(&[]);
            let track = &tracker.tracks[0];
            if i < 4 {
                assert!(track.is_active(), "should still be active at frame {}", i);
            }
        }
        let track = &tracker.tracks[0];
        assert_eq!(track.state, TrackState::Lost);
    }

    #[test]
    fn test_iou_matching() {
        let mut tracker = FaceTracker::new();

        // First frame: face at position A
        let face_a = make_face([100.0, 100.0, 200.0, 200.0]);
        let tracks = tracker.update(&[face_a]);
        let id_a = tracks[0].id;

        // Second frame: face at slightly different position (high IoU)
        let face_a_moved = make_face([110.0, 105.0, 210.0, 205.0]);
        let tracks = tracker.update(&[face_a_moved]);
        assert_eq!(tracks[0].id, id_a, "should match same track");

        // Third frame: face at completely different position (new track)
        let face_b = make_face([500.0, 500.0, 600.0, 600.0]);
        let tracks = tracker.update(&[face_b]);
        assert_ne!(tracks[0].id, id_a, "should be new track");
    }

    #[test]
    fn test_multiple_faces() {
        let mut tracker = FaceTracker::new();

        // Two faces
        let face_a = make_face([100.0, 100.0, 200.0, 200.0]);
        let face_b = make_face([300.0, 300.0, 400.0, 400.0]);

        let tracks = tracker.update(&[face_a.clone(), face_b.clone()]);
        assert_eq!(tracks.len(), 2);
        let id_a = tracks[0].id;
        let id_b = tracks[1].id;
        assert_ne!(id_a, id_b);

        // Both still present (slightly moved)
        let face_a_moved = make_face([105.0, 105.0, 205.0, 205.0]);
        let face_b_moved = make_face([305.0, 305.0, 405.0, 405.0]);

        let tracks = tracker.update(&[face_a_moved, face_b_moved]);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, id_a);
        assert_eq!(tracks[1].id, id_b);
    }

    #[test]
    fn test_reset() {
        let mut tracker = FaceTracker::new();
        let face = make_face([100.0, 100.0, 200.0, 200.0]);

        tracker.update(&[face]);
        assert_eq!(tracker.tracks.len(), 1);
        assert_eq!(tracker.frame_count(), 1);

        tracker.reset();
        assert_eq!(tracker.tracks.len(), 0);
        assert_eq!(tracker.frame_count(), 0);
    }
}
