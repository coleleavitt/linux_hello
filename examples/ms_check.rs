//! Run the converted Windows Hello models on saved frames and print one JSON line per frame.
//!
//! Used to check the Rust ports against the Python references in tools/winhello_models.
//! Usage: cargo run --release --example ms_check -- <frames_dir> [models_dir]

use anyhow::{Context, Result};
use image::GrayImage;
use linux_hello::ms_models::{MsIrLiveness, MsRetinaDetector, OcclusionDetector, PoseChecker, RetinaModelType};
use linux_hello::FaceDetector;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let frames = PathBuf::from(args.next().context("usage: ms_check <frames_dir> [models_dir]")?);
    let models = PathBuf::from(args.next().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()));
    let ms = models.join("windows_hello/converted");

    let mut scrfd = FaceDetector::load(models.join("det_10g.onnx"))?;
    let mut retina = MsRetinaDetector::load(ms.join("ldmf/ort/MODEL_FILES_110.ort"), RetinaModelType::Standard)?;
    let mut pose = PoseChecker::load(ms.join("lbf/headpose_lda_knn.onnx"))?;
    let mut occlusion = OcclusionDetector::load(ms.join("occlusion/MODEL_FILES_120.onnx"))?;
    let mut liveness = MsIrLiveness::load(ms.join("convnet"))?;

    let mut names: Vec<String> = std::fs::read_dir(&frames)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with("_lit.png"))
        .collect();
    names.sort();

    for name in names {
        let lit = image::open(frames.join(&name))?.to_luma8();
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
            println!("{}", serde_json::json!({ "frame": name, "face": null }));
            continue;
        };

        let ms_faces = retina.detect(&lit)?;
        let (head, frontal) = pose.check(&lit, &face)?;
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
            serde_json::json!({
                "frame": name,
                "face": { "bbox": face.bbox, "landmarks": face.landmarks, "score": face.score },
                "retina": ms_faces.iter().map(|f| serde_json::json!({
                    "bbox": f.face.bbox, "landmarks": f.face.landmarks, "score": f.face.score, "rotation": f.rotation,
                })).collect::<Vec<_>>(),
                "pose": { "pitch": head.pitch, "yaw": head.yaw, "frontal": frontal },
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
