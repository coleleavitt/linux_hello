//! Measure how the SCRFD face boxes of the IR and RGB cameras correspond, for the registration of
//! the colour face to the IR face (docs/capture-and-liveness.md §5, F66).
//!
//! Usage: cargo run --release --example color_pairs -- <frames_dir> [models_dir]
//!
//! Reads every `<stem>_ir_lit.png` or `<stem>_lit.png` (a `watch` output) with its `<stem>_rgb.png`,
//! prints the largest face box of each
//! frame, the per-pair scale and offset from the IR box to the RGB box, the mean registration over
//! the pairs and, with it, the overlap fraction each pair gets (`Registration::GetAreaOfOverlap`).

use anyhow::{Context, Result};
use linux_hello::pipeline::{overlap, ColorRegistration};
use linux_hello::FaceDetector;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().context("usage: color_pairs <frames_dir> [models_dir]")?);
    let models =
        args.next().map_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/models")), PathBuf::from);
    let mut detector = FaceDetector::load(models.join("det_10g.onnx"))?;

    let mut stems: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)?
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let stem = name.strip_suffix("_ir_lit.png").or_else(|| name.strip_suffix("_lit.png"))?;
            dir.join(format!("{stem}_rgb.png")).exists().then(|| (stem.to_string(), path.clone()))
        })
        .collect();
    stems.sort();
    let mut pairs = Vec::new();
    let mut used = Vec::new();
    for (stem, lit) in &stems {
        let ir = image::open(lit)?.to_luma8();
        let rgb = image::open(dir.join(format!("{stem}_rgb.png")))?.to_rgb8();
        let largest =
            |faces: Vec<linux_hello::Face>| faces.into_iter().max_by(|a, b| area(&a.bbox).total_cmp(&area(&b.bbox)));
        let (Some(i), Some(c)) = (largest(detector.detect_gray(&ir)?), largest(detector.detect(&rgb)?)) else {
            println!("{stem}: no face in one of the frames");
            continue;
        };
        let (iw, ih) = (i.bbox[2] - i.bbox[0], i.bbox[3] - i.bbox[1]);
        let (cw, ch) = (c.bbox[2] - c.bbox[0], c.bbox[3] - c.bbox[1]);
        let (sx, sy) = (cw / iw, ch / ih);
        let (tx, ty) = (c.bbox[0] - sx * i.bbox[0], c.bbox[1] - sy * i.bbox[1]);
        println!(
            "{stem}: ir {}x{} box {:.0?}  rgb {}x{} box {:.0?}  scale {sx:.3} {sy:.3} offset {tx:.1} {ty:.1}",
            ir.width(),
            ir.height(),
            i.bbox,
            rgb.width(),
            rgb.height(),
            c.bbox
        );
        pairs.push((i.bbox, c.bbox, [sx, sy, tx, ty]));
        used.push(stem.as_str());
    }
    if pairs.is_empty() {
        return Ok(());
    }
    let n = pairs.len() as f32;
    let mean: Vec<f32> = (0..4).map(|k| pairs.iter().map(|p| p.2[k]).sum::<f32>() / n).collect();
    let registration = ColorRegistration { scale: [mean[0], mean[1]], offset: [mean[2], mean[3]] };
    println!("mean registration: scale {:.3} {:.3} offset {:.1} {:.1}", mean[0], mean[1], mean[2], mean[3]);
    println!(
        "shipped registration: scale {:.3?} offset {:.1?}",
        ColorRegistration::P16_GEN3.scale,
        ColorRegistration::P16_GEN3.offset
    );
    for (stem, (ir, rgb, _)) in used.iter().zip(&pairs) {
        let mapped = registration.map(ir);
        let shipped = ColorRegistration::P16_GEN3.map(ir);
        println!(
            "{stem}: mapped {:.0?} overlap {:.3} (shipped {:.3})",
            mapped,
            overlap(&mapped, rgb),
            overlap(&shipped, rgb)
        );
    }
    Ok(())
}

fn area(b: &[f32; 4]) -> f32 {
    (b[2] - b[0]) * (b[3] - b[1])
}
