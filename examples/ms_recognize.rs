//! Microsoft's face recogniser as the second opinion on saved frames: one JSON line per face with
//! its feature, then one per probe with both opinions of the real matching code.
//!
//! Owner frames (`ir_*_lit.png` with their `ir_(NN-1)_dark.png`, `watch_*_ir_lit.png`) and every
//! face of any other `.jpg`/`.png` (strangers) get the ArcFace print and Microsoft's feature from
//! the image the pipeline uses (`lit − min(lit, ambient)` when there is an ambient frame) at the
//! Windows IR detector's points (SCRFD's when it has no face). Then each owner frame is matched as
//! a probe against the other frames of its session as the template, the other session's frames
//! against the full-face `watch_*` template, and the strangers against both. The template, the
//! calibration tables and the decision are the library's (`Template::best_match`). With
//! `crops_dir`, the 128×128 recognition crops are written there as `crop_<frame>.png`, for the
//! cross-check against `tools/winhello_models/cefa/align.py`.
//! Usage: cargo run --release --example ms_recognize -- <frames_dir> [models_dir [crops_dir]]

use anyhow::{Context, Result};
use image::GrayImage;
use linux_hello::calibration;
use linux_hello::lighting::subtract_ambient;
use linux_hello::ms_models::{MsRecognizer, MsRetinaDetector, RetinaModelType};
use linux_hello::{gray_to_rgb, Embedding, Face, FaceDetector, FaceEmbedder, Template};
use serde_json::json;
use std::path::PathBuf;
use std::time::Instant;

struct Probe {
    name: String,
    session: &'static str,
    print: Embedding,
}

fn largest(faces: Vec<Face>) -> Option<Face> {
    faces.into_iter().max_by(|a, b| (a.bbox[2] - a.bbox[0]).total_cmp(&(b.bbox[2] - b.bbox[0])))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let frames = PathBuf::from(args.next().context("usage: ms_recognize <frames_dir> [models_dir]")?);
    let models = PathBuf::from(args.next().unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()));
    let crops = args.next().map(PathBuf::from);
    let ms = models.join("windows_hello/converted");

    let mut scrfd = FaceDetector::load(models.join("det_10g.onnx"))?;
    let mut retina = MsRetinaDetector::load(ms.join("ldmf/ort/MODEL_FILES_110.ort"), RetinaModelType::Standard)?;
    let mut recognizer = MsRecognizer::load(ms.join("cefa"))?;
    let mut embedder = FaceEmbedder::load(models.join("w600k_r50.onnx"))?;
    let arcface = calibration::load(models.join("calibration.json"), Some("30c9:00f3"))?;

    let mut names: Vec<String> = std::fs::read_dir(&frames)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            (n.ends_with(".png") || n.ends_with(".jpg")) && !n.ends_with("_dark.png") && !n.ends_with("_rgb.png")
        })
        .collect();
    names.sort();

    let mut probes = Vec::new();
    for name in names {
        let lit = image::open(frames.join(&name))?.to_luma8();
        let ambient: Option<GrayImage> = name
            .strip_prefix("ir_")
            .and_then(|r| r.strip_suffix("_lit.png"))
            .and_then(|n| n.parse::<u32>().ok())
            .and_then(|n| image::open(frames.join(format!("ir_{}_dark.png", n - 1))).ok())
            .map(|i| i.to_luma8());
        let image = match &ambient {
            Some(ambient) => subtract_ambient(&lit, ambient),
            None => lit.clone(),
        };
        let (session, faces) = if name.ends_with("_lit.png") {
            let session = if name.starts_with("watch_") { "B" } else { "A" };
            (session, largest(scrfd.detect_gray(&lit)?).into_iter().collect())
        } else {
            ("stranger", scrfd.detect_gray(&lit)?)
        };
        for (i, face) in faces.into_iter().enumerate() {
            let name = if session == "stranger" { format!("{name}#{i}") } else { name.clone() };
            let windows_face = retina.matching(&lit, &face)?.map(|(f, _)| f.face);
            let points = windows_face.as_ref().unwrap_or(&face).landmarks;
            let mut print = embedder.embed_face(&gray_to_rgb(&image), &face)?;
            let start = Instant::now();
            let feature = recognizer.embed(&image, &points)?;
            let elapsed = start.elapsed().as_secs_f64() * 1e3;
            print.windows = Some(feature.to_vec());
            if let Some(dir) = &crops {
                std::fs::create_dir_all(dir)?;
                MsRecognizer::crop(&image, &points).save(dir.join(format!("crop_{}.png", name.replace('#', "_"))))?;
            }
            println!(
                "{}",
                json!({
                    "frame": name, "session": session, "subtracted": ambient.is_some(),
                    "points": if windows_face.is_some() { "retina" } else { "scrfd" }, "landmarks": points,
                    "windows": &feature[..], "ms": elapsed,
                })
            );
            probes.push(Probe { name, session, print });
        }
    }

    let windows = &recognizer.calibration;
    let template = |s: &str, except: &str| {
        Template::enrolled(probes.iter().filter(|p| p.session == s && p.name != except).map(|p| p.print.clone()))
    };
    // (probe, template name, template)
    let mut trials = Vec::new();
    for probe in &probes {
        match probe.session {
            "stranger" => {
                trials.push((probe, "A", template("A", "")));
                trials.push((probe, "B", template("B", "")));
            }
            s => {
                trials.push((probe, s, template(s, &probe.name)));
                if s == "A" {
                    trials.push((probe, "B", template("B", "")));
                }
            }
        }
    }
    for (probe, name, template) in trials {
        let m = template.best_match(&probe.print, &arcface, Some(windows)).context("empty template")?;
        println!(
            "{}",
            json!({
                "probe": probe.name, "template": name, "prints": template.prints.len(),
                "similarity": m.similarity, "far": m.far,
                "ms": m.windows.map(|w| w.distance), "ms_far": m.windows.map(|w| w.far),
                "arcface_recognised": m.far < linux_hello::TARGET_FAR,
                "windows_recognised": m.windows.map(|w| w.far < linux_hello::TARGET_FAR),
                "recognised": m.recognised(),
            })
        );
    }
    Ok(())
}
