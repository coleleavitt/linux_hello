//! Camera handling around the pipeline: open the cameras, try frames, return one verdict.

use anyhow::Result;
use image::RgbImage;
use tracing::info;

use crate::{Camera, CameraType, FacePipeline, FrameCheck, Verdict};

const LIVENESS_BUFFERS: u32 = 2;

pub struct CameraSetup {
    pub device: String,
    pub kind: CameraType,
    /// RGB camera for the liveness models when `kind` is IR (they only work on visible light)
    pub liveness_device: Option<String>,
}

/// Cameras opened for one authentication. The emitter and privacy LED stay off between requests.
pub struct OpenCameras {
    main: Camera,
    liveness: Option<Camera>,
}

impl OpenCameras {
    pub fn open(setup: &CameraSetup) -> Result<Self> {
        // Open RGB first: starting it after the IR stream stalls it. Reading one frame starts
        // the stream (v4l starts streaming on the first read); its auto-exposure then keeps
        // adjusting while the IR camera warms up, so afterwards we only drop the frames it
        // buffered meanwhile.
        let mut liveness = setup
            .liveness_device
            .as_ref()
            .map(|d| Camera::open_with(d, CameraType::Rgb, 1, LIVENESS_BUFFERS))
            .transpose()?;
        let main = Camera::open(&setup.device, setup.kind)?;
        if let Some(cam) = liveness.as_mut() {
            cam.skip(LIVENESS_BUFFERS as usize)?;
        }
        Ok(Self { main, liveness })
    }

    /// One recognition frame plus, if configured, the RGB frame for liveness.
    pub fn capture(&mut self) -> Result<(RgbImage, Option<RgbImage>)> {
        let frame = self.main.capture()?;
        let live = self.liveness.as_mut().map(|c| c.capture()).transpose()?;
        Ok((frame, live))
    }
}

pub struct Authenticator {
    pub pipeline: FacePipeline,
    pub cameras: CameraSetup,
}

impl Authenticator {
    /// Try up to `attempts` frames; return on the first match, otherwise the most informative failure.
    pub fn verify(&mut self, user: &str, attempts: usize) -> Result<Verdict> {
        if !self.pipeline.is_enrolled(user) {
            return Ok(Verdict::NotEnrolled);
        }
        let mut cams = OpenCameras::open(&self.cameras)?;
        let mut best = Verdict::NoFace;
        for _ in 0..attempts {
            let (frame, live) = cams.capture()?;
            let check: FrameCheck = self.pipeline.check(user, &frame, live.as_ref())?;
            let verdict = self.pipeline.verdict(&check);
            info!(user, %verdict, liveness = ?check.liveness, similarity = ?check.similarity, "frame");
            if matches!(verdict, Verdict::Match { .. }) {
                return Ok(verdict);
            }
            best = more_informative(best, verdict);
        }
        Ok(best)
    }
}

/// Rank failures: a non-matching live face says more than a spoof, which says more than nothing.
fn more_informative(a: Verdict, b: Verdict) -> Verdict {
    let rank = |v: &Verdict| match v {
        Verdict::NoMatch { similarity } => 3.0 + similarity,
        Verdict::NotLive => 2.0,
        _ => 0.0,
    };
    if rank(&b) > rank(&a) { b } else { a }
}
