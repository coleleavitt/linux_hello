//! Every liveness check behind one interface ([`Check`]), so that the checks the pipeline runs
//! today ([`Builtin`]) and new ones are scored alike by `examples/liveness_bench` on the same
//! labelled recordings (docs/liveness-bench.md): per frame, per 16-pair session and over every
//! threshold, genuine acceptance against attack acceptance. The pipeline's own votes
//! ([`super::Vote`]) do not go through this interface.

use anyhow::Result;

use crate::lighting::{self, MIN_EMITTER_GAIN};
use crate::ms_models::ir_liveness::thresholds;
use crate::ms_models::occlusion::OCCLUSION_THRESHOLD;
use crate::ms_models::{PITCH_LIMIT, YAW_LIMIT};
use crate::pipeline::ColorNet;
use crate::{Capture, FaceCheck, FrameCheck};

/// One frame pair with what the pipeline found in it ([`crate::FacePipeline::check`]) and the
/// track of each face ([`crate::Session::attempts`]).
pub struct Observed {
    pub capture: Capture,
    pub check: FrameCheck,
    /// Track id of each face of `check.faces`, in the same order
    pub tracks: Vec<u32>,
}

/// What a check sees: the current pair and the pairs before it in the same recording or
/// session, oldest first (at most [`crate::authenticator::MAX_PAIRS`] in the bench).
pub struct Frame<'a> {
    pub current: &'a Observed,
    pub previous: &'a [Observed],
}

impl<'a> Frame<'a> {
    /// The face at `face` of the current pair.
    pub fn face(&self, face: usize) -> &'a FaceCheck {
        &self.current.check.faces[face]
    }

    /// The same face (by track) in the earlier pairs, newest first: each pair with the face's
    /// index in it.
    pub fn history(&self, face: usize) -> impl Iterator<Item = (&'a Observed, usize)> + '_ {
        let id = self.current.tracks[face];
        self.previous.iter().rev().filter_map(move |o| o.tracks.iter().position(|t| *t == id).map(|i| (o, i)))
    }
}

/// A liveness check: one score per face of a frame pair. `None` when the check cannot run on
/// that frame (its input is missing, or the face was rejected). A score passes a threshold `t`
/// when `score ≥ t`, or `score ≤ t` when [`Check::higher_is_live`] is false.
pub trait Check {
    fn name(&self) -> &str;

    /// Whether a higher score means more live (the default), or less.
    fn higher_is_live(&self) -> bool {
        true
    }

    /// The threshold the pipeline applies today, if it applies one; the bench marks it.
    fn threshold(&self) -> Option<f32> {
        None
    }

    /// Whether `score` passes `threshold` in this check's direction.
    fn passes(&self, score: f32, threshold: f32) -> bool {
        if self.higher_is_live() {
            score >= threshold
        } else {
            score <= threshold
        }
    }

    fn score(&mut self, frame: &Frame, face: usize) -> Result<Option<f32>>;
}

/// The scores the pipeline computes today, read off the [`FaceCheck`] (docs/capture-and-liveness.md
/// §2, §3, §5, §6). The liveness nets only run on good-lighting frames, so on a weak frame they
/// are `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// Lit − ambient face brightness (F14): good lighting needs ≥ 45
    Gain,
    /// Ambient-only face brightness (F14): good lighting needs ≤ 25 ([`lighting::max_ambient`])
    Ambient,
    /// Windows' diffusion IR net (slot 12): p(real) on the difference-image crop
    DiffusionIr,
    /// Windows' general IR net (slot 11): p(real) on the lit crop, run when the ambient frame
    /// is blank (F42)
    GeneralIr,
    /// Windows' colour net (slot 13): p(spoof) on the colour face
    Color,
    /// Windows' border net (slot 14): p(spoof) on the whole lit frame
    Border,
    /// Windows' HH net (slot 15): p(spoof) on the whole lit frame
    Hh,
    /// Windows' frame total (F25): live above 0
    WindowsTotal,
    /// The open RGB models' mean P(real) (FaceNox + MiniFASNetV2; without the Windows Hello models)
    RgbReal,
    /// Windows' occlusion model: 1 − p(none)
    Occlusion,
    /// |yaw| of the Windows IR detector's pose, degrees
    Yaw,
    /// |pitch| of the Windows IR detector's pose, degrees
    Pitch,
    /// ArcFace's best cosine similarity to the user's template (its threshold depends on the
    /// calibration table and the template size, docs/calibration.md)
    Similarity,
    /// Microsoft's recogniser's engine distance to the user's template (Microsoft's table puts
    /// FAR 1e-5 at 0.350)
    MsDistance,
}

impl Builtin {
    pub const ALL: [Builtin; 14] = [
        Builtin::Gain,
        Builtin::Ambient,
        Builtin::DiffusionIr,
        Builtin::GeneralIr,
        Builtin::Color,
        Builtin::Border,
        Builtin::Hh,
        Builtin::WindowsTotal,
        Builtin::RgbReal,
        Builtin::Occlusion,
        Builtin::Yaw,
        Builtin::Pitch,
        Builtin::Similarity,
        Builtin::MsDistance,
    ];
}

impl Check for Builtin {
    fn name(&self) -> &str {
        match self {
            Builtin::Gain => "gain",
            Builtin::Ambient => "ambient",
            Builtin::DiffusionIr => "diffusion_ir",
            Builtin::GeneralIr => "general_ir",
            Builtin::Color => "color",
            Builtin::Border => "border",
            Builtin::Hh => "hh",
            Builtin::WindowsTotal => "windows_total",
            Builtin::RgbReal => "rgb_real",
            Builtin::Occlusion => "occlusion",
            Builtin::Yaw => "yaw",
            Builtin::Pitch => "pitch",
            Builtin::Similarity => "similarity",
            Builtin::MsDistance => "ms_distance",
        }
    }

    fn higher_is_live(&self) -> bool {
        matches!(
            self,
            Builtin::Gain
                | Builtin::DiffusionIr
                | Builtin::GeneralIr
                | Builtin::WindowsTotal
                | Builtin::RgbReal
                | Builtin::Similarity
        )
    }

    fn threshold(&self) -> Option<f32> {
        Some(match self {
            Builtin::Gain => MIN_EMITTER_GAIN,
            Builtin::Ambient => lighting::max_ambient(),
            Builtin::DiffusionIr => thresholds::DIFFUSION_IR_REAL,
            Builtin::GeneralIr => thresholds::GENERAL_IR_REAL,
            Builtin::Color => thresholds::COLOR_SPOOF,
            Builtin::Border => thresholds::BORDER_SPOOF,
            Builtin::Hh => thresholds::HH_SPOOF,
            Builtin::WindowsTotal => 1.0,
            Builtin::RgbReal => super::RGB_REAL_THRESHOLD,
            Builtin::Occlusion => OCCLUSION_THRESHOLD,
            Builtin::Yaw => YAW_LIMIT,
            Builtin::Pitch => PITCH_LIMIT,
            Builtin::Similarity | Builtin::MsDistance => return None,
        })
    }

    fn score(&mut self, frame: &Frame, face: usize) -> Result<Option<f32>> {
        let f = frame.face(face);
        let lighting = f.analysis.lighting;
        let windows = f.windows.as_ref().map(|w| &w.scores);
        Ok(match self {
            Builtin::Gain => lighting.map(|l| l.gain()),
            Builtin::Ambient => lighting.map(|l| l.ambient),
            Builtin::DiffusionIr => windows.and_then(|s| s.diffusion_ir).map(|p| p[0]),
            Builtin::GeneralIr => windows.and_then(|s| s.general_ir).map(|p| p[0]),
            Builtin::Color => match f.color {
                ColorNet::Spoof(p) => Some(p),
                _ => None,
            },
            Builtin::Border => windows.and_then(|s| s.border).map(|p| p[1]),
            Builtin::Hh => windows.and_then(|s| s.hh).map(|p| p[1]),
            Builtin::WindowsTotal => f.windows.as_ref().map(|w| w.total as f32),
            Builtin::RgbReal => f.liveness,
            Builtin::Occlusion => f.analysis.occlusion,
            Builtin::Yaw => f.analysis.pose.map(|p| p.yaw.abs()),
            Builtin::Pitch => f.analysis.pose.map(|p| p.pitch.abs()),
            Builtin::Similarity => f.best.map(|b| b.similarity),
            Builtin::MsDistance => f.best.and_then(|b| b.windows).map(|w| w.distance),
        })
    }
}

/// Every check the bench scores: the built-in ones, then the checks added since. A check that
/// needs models takes them in its constructor and is added here with the models directory.
pub fn all() -> Vec<Box<dyn Check>> {
    Builtin::ALL.iter().map(|b| Box::new(*b) as Box<dyn Check>).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_pass_in_the_checks_direction() {
        assert!(Builtin::Gain.passes(45.0, 45.0));
        assert!(!Builtin::Gain.passes(44.9, 45.0));
        assert!(Builtin::Ambient.passes(25.0, 25.0));
        assert!(!Builtin::Ambient.passes(25.1, 25.0));
        assert!(Builtin::Color.passes(0.3, 0.5));
        assert!(!Builtin::Color.passes(0.7, 0.5));
        assert!(Builtin::WindowsTotal.passes(1.0, 1.0));
        assert!(!Builtin::WindowsTotal.passes(0.0, 1.0));
        let names: std::collections::HashSet<String> = all().iter().map(|c| c.name().to_string()).collect();
        assert_eq!(names.len(), Builtin::ALL.len());
    }
}
