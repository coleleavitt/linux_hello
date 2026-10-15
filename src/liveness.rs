//! Liveness voting over frame pairs, after Windows' `AntiSpoofingDecisionEngine` (F24, F25).
//!
//! Windows' live votes come from two IR anti-spoofing CNNs; their models are proprietary, so
//! here the IR evidence is the lighting gate: a face that reflects the emitter like skin, with
//! little ambient IR (F14). The RGB models (FaceNox + MiniFASNetV2) can only veto, like
//! Windows' colour CNN.

/// RGB liveness: mean P(real) below this vetoes the frame.
pub const RGB_REAL_THRESHOLD: f32 = 0.5;
/// Live frames needed to decide "live" (F25).
pub const LIVE_FRAMES: u32 = 2;
/// Spoof frames that decide "spoof" (F25).
pub const SPOOF_FRAMES: u32 = 3;
/// Frame pairs before giving up. Windows stops after 48 frames or when its counter passes 16;
/// one pair here is two camera frames.
pub const MAX_PAIRS: u32 = 16;

/// One frame pair's liveness vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    Live,
    Spoof,
    /// Bad lighting, no face, or the evidence cancels out
    Abstain,
}

impl Vote {
    /// Sum the evidence of a frame pair: the lighting gate gives +1; the RGB veto (`rgb_real`
    /// below [`RGB_REAL_THRESHOLD`]) gives −2. A frame that fails the gate casts no vote.
    ///
    /// Windows' colour veto counts −1 against two IR CNN votes, so it can cancel a live frame but
    /// the spoof votes that reject a session come from the IR CNNs. With a single IR vote here,
    /// the veto weighs −2 (the weight Windows gives its multi-task IR vetoes in strict mode) so
    /// that a vetoed frame is a spoof frame and three of them reject the session.
    pub fn from_evidence(good_lighting: bool, rgb_real: Option<f32>) -> Self {
        if !good_lighting {
            return Vote::Abstain;
        }
        let veto = rgb_real.is_some_and(|p| p < RGB_REAL_THRESHOLD);
        let total = 1 - if veto { 2 } else { 0 };
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
    GaveUp,
}

/// Counts votes for one authentication (`MakeFinalDecision`, F25).
#[derive(Debug, Default, Clone)]
pub struct LivenessSession {
    live: u32,
    spoof: u32,
    pairs: u32,
}

impl LivenessSession {
    pub fn add(&mut self, vote: Vote) -> LivenessState {
        self.pairs += 1;
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
        } else if self.pairs >= MAX_PAIRS {
            LivenessState::GaveUp
        } else {
            LivenessState::Pending
        }
    }

    pub fn pairs(&self) -> u32 {
        self.pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_votes() {
        assert_eq!(Vote::from_evidence(true, None), Vote::Live);
        assert_eq!(Vote::from_evidence(true, Some(0.9)), Vote::Live);
        assert_eq!(Vote::from_evidence(true, Some(0.2)), Vote::Spoof);
        assert_eq!(Vote::from_evidence(false, Some(0.9)), Vote::Abstain);
        assert_eq!(Vote::from_evidence(false, Some(0.1)), Vote::Abstain);
    }

    #[test]
    fn two_live_frames_accept() {
        let mut s = LivenessSession::default();
        assert_eq!(s.add(Vote::Live), LivenessState::Pending);
        assert_eq!(s.add(Vote::Abstain), LivenessState::Pending);
        assert_eq!(s.add(Vote::Spoof), LivenessState::Pending);
        assert_eq!(s.add(Vote::Live), LivenessState::Live);
    }

    #[test]
    fn three_spoof_frames_reject() {
        let mut s = LivenessSession::default();
        assert_eq!(s.add(Vote::Spoof), LivenessState::Pending);
        assert_eq!(s.add(Vote::Live), LivenessState::Pending);
        assert_eq!(s.add(Vote::Spoof), LivenessState::Pending);
        assert_eq!(s.add(Vote::Spoof), LivenessState::Spoof);
    }

    #[test]
    fn gives_up_after_max_pairs() {
        let mut s = LivenessSession::default();
        for _ in 1..MAX_PAIRS {
            assert_eq!(s.add(Vote::Abstain), LivenessState::Pending);
        }
        assert_eq!(s.add(Vote::Live), LivenessState::GaveUp);
    }
}
