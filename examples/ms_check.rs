//! Run the converted Windows Hello models on saved frames and print one JSON line per frame.
//!
//! Used to check the Rust ports against the Python references in tools/winhello_models.
//! `*_lit.png` IR frames get every model on the largest face; other images except `*_dark.png` and
//! `*_rgb.png` (e.g. a visible-light photo) get the pose estimators on every SCRFD face of the
//! grayscale image.
//!
//! For lit frames it also compares the landmark sources the pipeline can use: the face brightness
//! block (F43) and the liveness/occlusion crops from SCRFD's points, from the Windows IR detector's
//! points (what the pipeline uses) and from the legacy LBF aligner's eye contour; and for an
//! `ir_NN_lit` frame whose previous lit frame `ir_(NN-2)_lit` exists, the ambient warp (F13) from the
//! two lit frames' anchors against the ambient frame as captured. Each face also gets Microsoft's
//! recognition feature (`MsRecognizer`) of the recognition image, with its time.
//! Usage: cargo run --release --example ms_check -- <frames_dir> [models_dir]

use anyhow::{Context, Result};
use image::GrayImage;
use linux_hello::lighting::{affine_from_anchors, anchors, face_brightness, interpolate, subtract_ambient, warp};
use linux_hello::ms_models::{MsIrLiveness, MsRecognizer, MsRetinaDetector, OcclusionDetector, RetinaModelType};
use linux_hello::{gray_to_rgb, Face, FaceDetector, FaceEmbedder, FaceOrientation, IrFaceOrientation, LbfAligner};
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

/// The ambient warp (F13) from the anchors of the face in the current and the previous lit frame (the
/// Windows detector's points when it has the face, else SCRFD's), with the ambient frame at `t` on the
/// way from the current (0) to the previous (1) lit frame; against the ambient frame as captured: the
/// matrix, the anchor motion, the face brightness block (F43), the mean pixel change over the face box,
/// the residual `|lit − ambient|` over the box (meaningful when `ambient` is a lit stand-in) and, with
/// `embedder`, the cosine between the ArcFace prints of the two subtracted images.
#[allow(clippy::too_many_arguments)]
fn warp_check(
    embedder: Option<&mut FaceEmbedder>,
    lit: &GrayImage,
    ambient: &GrayImage,
    t: f32,
    face: &Face,
    cur: &[[f32; 2]; 5],
    prev: &[[f32; 2]; 5],
) -> Result<Value> {
    let points = cur;
    let (cur, prev) = (anchors(cur), anchors(prev));
    let moved: Vec<f32> = cur.iter().zip(&prev).map(|(c, p)| (c[0] - p[0]).hypot(c[1] - p[1])).collect();
    let in_ambient = interpolate(&cur, &prev, t);
    let Some(m) = affine_from_anchors(&cur, &in_ambient) else { return Ok(Value::Null) };
    let start = Instant::now();
    let warped = warp(ambient, &m);
    let warp_ms = start.elapsed().as_secs_f64() * 1e3;
    let [x1, y1, x2, y2] = face.bbox.map(|v| v.max(0.0) as u32);
    let (mut change, mut raw_residual, mut warped_residual, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in y1..y2.min(lit.height()) {
        for x in x1..x2.min(lit.width()) {
            let (l, a, w) = (lit.get_pixel(x, y).0[0], ambient.get_pixel(x, y).0[0], warped.get_pixel(x, y).0[0]);
            change += w.abs_diff(a) as u64;
            raw_residual += l.abs_diff(a) as u64;
            warped_residual += l.abs_diff(w) as u64;
            n += 1;
        }
    }
    let per_pixel = |sum: u64| sum as f64 / n.max(1) as f64;
    let cosine = match embedder {
        Some(embedder) => {
            let mut print = |a: &GrayImage| embedder.embed_face(&gray_to_rgb(&subtract_ambient(lit, a)), face);
            Some(print(ambient)?.similarity(&print(&warped)?))
        }
        None => None,
    };
    Ok(json!({
        "anchors_moved_px": moved,
        "matrix": m,
        "ambient_brightness": { "raw": face_brightness(ambient, points), "warped": face_brightness(&warped, points) },
        "mean_abs_change_in_box": per_pixel(change),
        "residual_in_box": { "raw": per_pixel(raw_residual), "warped": per_pixel(warped_residual) },
        "print_cosine": cosine,
        "ms": warp_ms,
    }))
}

fn largest(faces: Vec<Face>) -> Option<Face> {
    faces.into_iter().max_by(|a, b| (a.bbox[2] - a.bbox[0]).total_cmp(&(b.bbox[2] - b.bbox[0])))
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
    let mut recognizer = MsRecognizer::load(ms.join("cefa"))?;
    let mut embedder = FaceEmbedder::load(models.join("w600k_r50.onnx"))?;

    let mut names: Vec<String> = std::fs::read_dir(&frames)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| {
            (n.ends_with(".png") || n.ends_with(".jpg")) && !n.ends_with("_dark.png") && !n.ends_with("_rgb.png")
        })
        .collect();
    names.sort();

    let mut previous_watch: Option<GrayImage> = None;
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
        // ir_NN_lit pairs with ir_(NN-1)_dark and follows ir_(NN-2)_lit; watch_* frames pair with watch_*_rgb.
        let ir_frame = |suffix: &str, back: u32| {
            name.strip_prefix("ir_")
                .and_then(|r| r.strip_suffix("_lit.png"))
                .and_then(|n| n.parse::<u32>().ok())
                .and_then(|n| image::open(frames.join(format!("ir_{}_{suffix}.png", n - back))).ok())
                .map(|i| i.to_luma8())
        };
        let ambient: Option<GrayImage> = ir_frame("dark", 1);
        // The lit frame before this one: ir_(NN-2)_lit, or the previous watch_* frame (another time)
        let previous_lit: Option<GrayImage> = match (ir_frame("lit", 2), name.starts_with("watch_")) {
            (Some(frame), _) => Some(frame),
            (None, true) => previous_watch.replace(lit.clone()),
            (None, false) => None,
        };
        let rgb = name
            .strip_suffix("_ir_lit.png")
            .and_then(|s| image::open(frames.join(format!("{s}_rgb.png"))).ok())
            .map(|i| i.to_rgb8());

        let Some(face) = largest(scrfd.detect_gray(&lit)?) else {
            println!("{}", json!({ "frame": name, "face": null }));
            continue;
        };

        let ms_faces = pose.retina.detect(&lit)?;
        let retina_face = pose.retina.matching(&lit, &face)?.map(|(f, _)| f.face);
        let rgb_face = match &rgb {
            Some(image) => scrfd
                .detect(image)?
                .into_iter()
                .max_by(|a, b| (a.bbox[2] - a.bbox[0]).total_cmp(&(b.bbox[2] - b.bbox[0])))
                .map(|f| (image, f)),
            None => None,
        };
        // The nets with SCRFD's face (before) and with the Windows detector's face (the pipeline now).
        // The recogniser gets the recognition image (lit − min(lit, ambient), F40 a).
        let recognition_image = ambient.as_ref().map_or_else(|| lit.clone(), |a| subtract_ambient(&lit, a));
        let mut per_face = Vec::new();
        for (source, f) in [("scrfd", Some(&face)), ("retina", retina_face.as_ref())] {
            let Some(f) = f else { continue };
            let occ = occlusion.check(&lit, f)?;
            let live = liveness.frame(&lit, ambient.as_ref(), f, rgb_face.as_ref().map(|(i, f)| (*i, f)))?;
            let s = &live.scores;
            let start = Instant::now();
            let feature = recognizer.embed(&recognition_image, &f.landmarks)?;
            let recognizer_ms = start.elapsed().as_secs_f64() * 1e3;
            per_face.push((
                source,
                json!({
                    "occlusion": { "probs": occ.probs, "occluded": occ.is_occluded },
                    "liveness": {
                        "general_ir": s.general_ir, "diffusion_ir": s.diffusion_ir, "color": s.color,
                        "border": s.border, "hh": s.hh, "total": live.total,
                    },
                    "recognizer": { "feature": &feature[..], "ms": recognizer_ms },
                }),
            ));
        }
        let pose_json = pose.check(&lit, &face)?;
        let lbf: Vec<[f32; 2]> = serde_json::from_value(pose_json["legacy"]["points"].clone())?;
        // F43 block from p3, p4, p9, p15 of each landmark set; the LBF contour has real p9/p15.
        let blocks = [
            ("scrfd", Some(face.landmarks)),
            ("retina", retina_face.as_ref().map(|f| f.landmarks)),
            ("lbf", Some([lbf[9], lbf[15], lbf[2], lbf[3], lbf[4]])),
        ];
        let brightness: Vec<(&str, Value)> = blocks
            .iter()
            .filter_map(|(source, points)| {
                let points = points.as_ref()?;
                Some((
                    *source,
                    json!({
                        "lit": face_brightness(&lit, points),
                        "ambient": ambient.as_ref().map(|a| face_brightness(a, points)),
                    }),
                ))
            })
            .collect();
        // Motion compensation against the previous lit frame: with the real ambient frame between the
        // two (t = 0.5, as for alternate frames at a fixed rate) and with the previous lit frame itself
        // as a stand-in ambient (t = 1), where the residual shows what the warp corrects.
        let mut motion = serde_json::Map::new();
        if let Some(previous) = &previous_lit {
            let prev_face = largest(scrfd.detect_gray(previous)?);
            let prev_retina = match &prev_face {
                Some(f) => pose.retina.matching(previous, f)?.map(|(f, _)| f.face),
                None => None,
            };
            let points = match (&retina_face, &prev_retina) {
                (Some(cur), Some(prev)) => Some(("retina", cur.landmarks, prev.landmarks)),
                _ => prev_face.as_ref().map(|prev| ("scrfd", face.landmarks, prev.landmarks)),
            };
            if let Some((source, cur, prev)) = points {
                motion.insert("source".into(), source.into());
                if let Some(ambient) = &ambient {
                    let v = warp_check(Some(&mut embedder), &lit, ambient, 0.5, &face, &cur, &prev)?;
                    motion.insert("ambient".into(), v);
                }
                let v = warp_check(None, &lit, previous, 1.0, &face, &cur, &prev)?;
                motion.insert("stand_in".into(), v);
            }
        }
        println!(
            "{}",
            json!({
                "frame": name,
                "face": { "bbox": face.bbox, "landmarks": face.landmarks, "score": face.score },
                "retina": ms_faces.iter().map(|f| json!({
                    "bbox": f.face.bbox, "landmarks": f.face.landmarks, "score": f.face.score,
                    "pose": f.orientation.as_ref().map(orientation_json),
                })).collect::<Vec<_>>(),
                "pose": pose_json,
                "rgb_face": rgb_face.as_ref().map(|(_, f)| f.landmarks),
                "by_face": per_face.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<serde_json::Map<_, _>>(),
                "brightness": brightness.into_iter().map(|(k, v)| (k.to_string(), v)).collect::<serde_json::Map<_, _>>(),
                "warp": Value::Object(motion),
            })
        );
    }
    Ok(())
}
