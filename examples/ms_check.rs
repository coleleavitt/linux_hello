//! Run the converted Windows Hello models on saved frames and print one JSON line per frame.
//!
//! Used to check the Rust ports against the Python references in tools/winhello_models.
//! `*_lit.png` IR frames get every model on the largest face; other images except `*_dark.png` and
//! `*_rgb.png` (e.g. a visible-light photo) get the pose estimators on every SCRFD face of the
//! grayscale image.
//! Usage: cargo run --release --example ms_check -- <frames_dir> [models_dir]

use anyhow::{Context, Result};
use image::GrayImage;
use linux_hello::ms_models::{MsIrLiveness, MsRetinaDetector, OcclusionDetector, RetinaModelType};
use linux_hello::{Face, FaceDetector, FaceOrientation, IrFaceOrientation, LbfAligner};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

struct Pose {
    retina: MsRetinaDetector,
    aligner: LbfAligner,
    orientation: IrFaceOrientation,
}

fn orientation_json(o: &FaceOrientation) -> Value {
    json!({
        "pitch": o.pitch, "yaw": o.yaw, "roll": o.roll, "flags": o.flags, "frontal": o.is_frontal(),
        "margin": o.frontal_margin,
    })
}

impl Pose {
    /// The frontal gate's view of `face` (Retina match) and the legacy LBF aligner + IrFaceOrientation
    /// started from the SCRFD box, with their run times.
    fn check(&mut self, image: &GrayImage, face: &Face) -> Result<Value> {
        let start = Instant::now();
        let matched = self.retina.matching(image, face)?;
        let retina_ms = start.elapsed().as_secs_f64() * 1e3;
        let start = Instant::now();
        let rect = face.bbox.map(|v| v as i32);
        let alignment = self.aligner.align(image, rect);
        let legacy = self.orientation.estimate(image, &alignment.points)?;
        let legacy_ms = start.elapsed().as_secs_f64() * 1e3;
        Ok(json!({
            "retina": matched.as_ref().map(|(f, iou)| json!({
                "iou": iou, "bbox": f.face.bbox, "landmarks": f.face.landmarks, "score": f.face.score,
                "pose": f.orientation.as_ref().map(orientation_json),
            })),
            "frontal": matched.as_ref().and_then(|(f, _)| f.orientation).is_some_and(|o| o.is_frontal()),
            "legacy": {
                "rect": rect, "points": alignment.points, "score": alignment.score, "accepted": alignment.accepted(),
                "pose": orientation_json(&legacy),
            },
            "ms": { "retina": retina_ms, "legacy": legacy_ms },
        }))
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let frames = PathBuf::from(args.next().context("usage: ms_check <frames_dir> [models_dir]")?);
    let models = PathBuf::from(args.next().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()));
    let ms = models.join("windows_hello/converted");
    let resources = models.join("windows_hello/facerecognitionsensoradapterresources");

    let mut scrfd = FaceDetector::load(models.join("det_10g.onnx"))?;
    let mut pose = Pose {
        retina: MsRetinaDetector::load(ms.join("ldmf/ort/MODEL_FILES_110.ort"), RetinaModelType::Standard)?,
        aligner: LbfAligner::load(resources.join("MODEL_FILES_102.cefa"))?,
        orientation: IrFaceOrientation::load(&resources)?,
    };
    let mut occlusion = OcclusionDetector::load(ms.join("occlusion/MODEL_FILES_120.onnx"))?;
    let mut liveness = MsIrLiveness::load(ms.join("convnet"))?;

    let mut names: Vec<String> = std::fs::read_dir(&frames)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            (n.ends_with(".png") || n.ends_with(".jpg")) && !n.ends_with("_dark.png") && !n.ends_with("_rgb.png")
        })
        .collect();
    names.sort();

    for name in names {
        let image = image::open(frames.join(&name))?;
        let lit = image.to_luma8();
        if !name.ends_with("_lit.png") {
            let mut faces = Vec::new();
            for face in scrfd.detect_gray(&lit)? {
                faces.push(json!({ "bbox": face.bbox, "landmarks": face.landmarks, "pose": pose.check(&lit, &face)? }));
            }
            let gray_sum: u64 = lit.as_raw().iter().map(|&p| p as u64).sum();
            println!("{}", json!({ "frame": name, "gray_sum": gray_sum, "faces": faces }));
            continue;
        }
        // ir_NN_lit pairs with ir_(NN-1)_dark; watch_* frames pair with watch_*_rgb.
        let ambient: Option<GrayImage> = name
            .strip_prefix("ir_")
            .and_then(|r| r.strip_suffix("_lit.png"))
            .and_then(|n| n.parse::<u32>().ok())
            .and_then(|n| image::open(frames.join(format!("ir_{}_dark.png", n - 1))).ok())
            .map(|i| i.to_luma8());
        let rgb = name
            .strip_suffix("_ir_lit.png")
            .and_then(|s| image::open(frames.join(format!("{s}_rgb.png"))).ok())
            .map(|i| i.to_rgb8());

        let Some(face) = scrfd
            .detect_gray(&lit)?
            .into_iter()
            .max_by(|a, b| (a.bbox[2] - a.bbox[0]).total_cmp(&(b.bbox[2] - b.bbox[0])))
        else {
            println!("{}", json!({ "frame": name, "face": null }));
            continue;
        };

        let ms_faces = pose.retina.detect(&lit)?;
        let occ = occlusion.check(&lit, &face)?;
        let rgb_face = match &rgb {
            Some(image) => scrfd
                .detect(image)?
                .into_iter()
                .max_by(|a, b| (a.bbox[2] - a.bbox[0]).total_cmp(&(b.bbox[2] - b.bbox[0])))
                .map(|f| (image, f)),
            None => None,
        };
        let live = liveness.frame(&lit, ambient.as_ref(), &face, rgb_face.as_ref().map(|(i, f)| (*i, f)))?;
        let s = &live.scores;
        println!(
            "{}",
            json!({
                "frame": name,
                "face": { "bbox": face.bbox, "landmarks": face.landmarks, "score": face.score },
                "retina": ms_faces.iter().map(|f| json!({
                    "bbox": f.face.bbox, "landmarks": f.face.landmarks, "score": f.face.score,
                    "pose": f.orientation.as_ref().map(orientation_json),
                })).collect::<Vec<_>>(),
                "pose": pose.check(&lit, &face)?,
                "occlusion": { "probs": occ.probs, "occluded": occ.is_occluded },
                "liveness": {
                    "general_ir": s.general_ir, "diffusion_ir": s.diffusion_ir, "color": s.color,
                    "border": s.border, "hh": s.hh, "total": live.total,
                    "rgb_face": rgb_face.as_ref().map(|(_, f)| f.landmarks),
                },
            })
        );
    }
    Ok(())
}
