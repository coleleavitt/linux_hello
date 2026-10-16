//! Score every liveness model separately on the frames saved by `linux_hello watch`.
//!
//! Usage: cargo run --release --example watch_scores -- <watch_dir> [models_dir]
//! Prints one JSON line per frame pair (`NNNN_lit.png`, `NNNN_ambient.png`, `NNNN_rgb.png`).

use anyhow::{Context, Result};
use linux_hello::{AntiSpoof, Face, FaceDetector, LivenessModel, MsIrLiveness, MsRetinaDetector, RetinaModelType};
use std::path::PathBuf;

fn largest(faces: Vec<Face>) -> Option<Face> {
    faces.into_iter().max_by(|a, b| (a.bbox[2] - a.bbox[0]).total_cmp(&(b.bbox[2] - b.bbox[0])))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().context("usage: watch_scores <watch_dir> [models_dir]")?);
    let models = PathBuf::from(args.next().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()));
    let mut scrfd = FaceDetector::load(models.join("det_10g.onnx"))?;
    let mut facenox = AntiSpoof::load(models.join("antispoof/facenox_antispoof.onnx"), LivenessModel::FaceNox)?;
    let mut minifas = AntiSpoof::load(models.join("antispoof/MiniFASNetV2.onnx"), LivenessModel::MiniFasNetV2)?;
    let mut windows = MsIrLiveness::load(models.join("windows_hello/converted/convnet"))?;
    let mut retina = MsRetinaDetector::load(
        models.join("windows_hello/converted/ldmf/ort/MODEL_FILES_110.ort"),
        RetinaModelType::Standard,
    )?;

    let mut ids: Vec<String> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix("_lit.png").map(String::from))
        .collect();
    ids.sort();
    for id in ids {
        let lit = image::open(dir.join(format!("{id}_lit.png")))?.to_luma8();
        let ambient = image::open(dir.join(format!("{id}_ambient.png")))?.to_luma8();
        let rgb = image::open(dir.join(format!("{id}_rgb.png"))).ok().map(|i| i.to_rgb8());
        let Some(face) = largest(scrfd.detect_gray(&lit)?) else { continue };
        let rgb_face = match &rgb {
            Some(image) => largest(scrfd.detect(image)?).map(|f| (image, f)),
            None => None,
        };
        let (nox, fas) = match &rgb_face {
            Some((image, f)) => (Some(facenox.check(image, f)?.score), Some(minifas.check(image, f)?.score)),
            None => (None, None),
        };
        let general = windows.frame(&lit, None, &face, None)?;
        let diffusion = windows.frame(&lit, Some(&ambient), &face, None)?;
        // The same nets on the crop from the Windows IR detector's landmarks (F23: RetinaEngine).
        let retina_face = retina.matching(&lit, &face)?.map(|(r, _)| r.face);
        let (general_r, diffusion_r) = match &retina_face {
            Some(f) => (Some(windows.frame(&lit, None, f, None)?), Some(windows.frame(&lit, Some(&ambient), f, None)?)),
            None => (None, None),
        };
        let rgb_mean = rgb.as_ref().map(|i| {
            i.pixels().map(|p| p.0.iter().map(|&c| c as u32).sum::<u32>()).sum::<u32>() as f32
                / (3 * i.width() * i.height()) as f32
        });
        println!(
            "{}",
            serde_json::json!({
                "id": id, "facenox": nox, "minifas": fas, "rgb_face": rgb_face.is_some(), "rgb_mean": rgb_mean,
                "general_ir": general.scores.general_ir, "diffusion_ir": diffusion.scores.diffusion_ir,
                "general_ir_retina": general_r.and_then(|m| m.scores.general_ir),
                "diffusion_ir_retina": diffusion_r.and_then(|m| m.scores.diffusion_ir),
            })
        );
    }
    Ok(())
}
