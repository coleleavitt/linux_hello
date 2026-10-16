//! Liveness voting, after Windows' `AntiSpoofingDecisionEngine` (F24, F25, F48).
//!
//! The IR evidence of a frame is the lighting gate (a face that reflects the emitter like skin,
//! with little ambient IR, F14) and, when the converted Windows Hello models are installed, their
//! frame vote (F25), which includes Windows' colour-net veto. Without those models the open RGB
//! models (FaceNox + MiniFASNetV2) veto instead. Votes are counted only from the frame the face is
//! recognised (F48).

pub mod check;
pub mod physics;
pub mod recording;

pub use check::{Builtin, Check, Frame, Observed};
pub use recording::{Label, Meta};

/// RGB liveness: mean P(real) below this vetoes the frame.
pub const RGB_REAL_THRESHOLD: f32 = 0.5;
/// Live frames needed to decide "live" (F25).
pub const LIVE_FRAMES: u32 = 2;
/// Spoof frames that decide "spoof" (F25).
pub const SPOOF_FRAMES: u32 = 3;

/// One frame pair's liveness vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    Live,
    Spoof,
    /// Not a good-lighting frame, or the evidence cancels out
    Abstain,
}

impl Vote {
    /// Sum the evidence of a frame pair. Only good-lighting frames (F44 status 1) vote. Their IR
    /// evidence is +1, or the Windows Hello frame total (`ms_total`, F25) when those models run,
    /// so a frame is live only when the emitter lighting and the Windows Hello nets agree. The RGB
    /// veto (`rgb_real` below [`RGB_REAL_THRESHOLD`]) gives −2.
    ///
    /// Windows' colour veto counts −1 against two IR CNN votes. With one IR vote here, the veto
    /// weighs −2 (the weight Windows gives its multi-task IR vetoes in strict mode) so that a
    /// vetoed frame is a spoof frame and three of them reject the session.
    pub fn from_evidence(good_lighting: bool, rgb_real: Option<f32>, ms_total: Option<i32>) -> Self {
        if !good_lighting {
            return Vote::Abstain;
        }
        let veto = rgb_real.is_some_and(|p| p < RGB_REAL_THRESHOLD);
        let total = ms_total.unwrap_or(1) - if veto { 2 } else { 0 };
        match total.cmp(&0) {
            std::cmp::Ordering::Greater => Vote::Live,
            std::cmp::Ordering::Less => Vote::Spoof,
            std::cmp::Ordering::Equal => Vote::Abstain,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessState {
    Pending,
    Live,
    Spoof,
}

/// Counts votes (`MakeFinalDecision`, F25).
#[derive(Debug, Default, Clone)]
pub struct LivenessSession {
    live: u32,
    spoof: u32,
}

impl LivenessSession {
    pub fn add(&mut self, vote: Vote) -> LivenessState {
        match vote {
            Vote::Live => self.live += 1,
            Vote::Spoof => self.spoof += 1,
            Vote::Abstain => {}
        }
        self.state()
    }

    /// Live wins over spoof, as in Windows: the counters only grow, so "live" is final.
    pub fn state(&self) -> LivenessState {
        if self.live >= LIVE_FRAMES {
            LivenessState::Live
        } else if self.spoof >= SPOOF_FRAMES {
            LivenessState::Spoof
        } else {
            LivenessState::Pending
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_votes() {
        assert_eq!(Vote::from_evidence(true, None, None), Vote::Live);
        assert_eq!(Vote::from_evidence(true, Some(0.9), None), Vote::Live);
        assert_eq!(Vote::from_evidence(true, Some(0.2), None), Vote::Spoof);
        assert_eq!(Vote::from_evidence(false, Some(0.9), None), Vote::Abstain);
        assert_eq!(Vote::from_evidence(false, Some(0.1), None), Vote::Abstain);
        // The Windows Hello nets must agree with the lighting gate.
        assert_eq!(Vote::from_evidence(true, Some(0.9), Some(1)), Vote::Live);
        assert_eq!(Vote::from_evidence(true, Some(0.9), Some(0)), Vote::Abstain);
        assert_eq!(Vote::from_evidence(true, Some(0.9), Some(-1)), Vote::Spoof);
        assert_eq!(Vote::from_evidence(false, None, Some(1)), Vote::Abstain);
    }

    #[test]
    fn two_live_frames_decide_live() {
        let mut s = LivenessSession::default();
        assert_eq!(s.add(Vote::Live), LivenessState::Pending);
        assert_eq!(s.add(Vote::Abstain), LivenessState::Pending);
        assert_eq!(s.add(Vote::Spoof), LivenessState::Pending);
        assert_eq!(s.add(Vote::Live), LivenessState::Live);
    }

    #[test]
    fn three_spoof_frames_decide_spoof() {
        let mut s = LivenessSession::default();
        assert_eq!(s.add(Vote::Spoof), LivenessState::Pending);
        assert_eq!(s.add(Vote::Live), LivenessState::Pending);
        assert_eq!(s.add(Vote::Spoof), LivenessState::Pending);
        assert_eq!(s.add(Vote::Spoof), LivenessState::Spoof);
    }
}
