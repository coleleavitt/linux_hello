//! Embed every image in a list with linux_hello's own pipeline, for threshold calibration.
//!
//! For each image: SCRFD detection, keep the largest face (as `FacePipeline::largest_face`),
//! 5-point ArcFace alignment, w600k_r50 embedding (L2-normalised). Each image is processed
//! twice: as RGB, and as grayscale replicated to three channels (the IR-camera path).
//!
//! Usage: embed_dir <models_dir> <image_root> <list.txt> <out_prefix> [workers] [largest|centre]
//!
//! `centre` keeps the face nearest the image centre instead of the largest one. LFW labels the
//! centred face; in group photos the largest face can be someone else.
//!
//! Writes `<out_prefix>.{rgb,gray}.f32` (N x 512 little-endian f32, zero rows where no face
//! was found) and `<out_prefix>.{rgb,gray}.tsv` (index, path, face count, score, bbox).

use anyhow::{Context, Result};
use linux_hello::{gray_to_rgb, Face, FaceDetector, FaceEmbedder};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const DIM: usize = 512;

struct Row {
    faces: usize,
    face: Option<Face>,
    features: Vec<f32>,
}

fn area(f: &Face) -> f32 {
    (f.bbox[2] - f.bbox[0]) * (f.bbox[3] - f.bbox[1])
}

fn centre_dist(f: &Face, img: &image::RgbImage) -> f32 {
    let dx = (f.bbox[0] + f.bbox[2]) / 2.0 - img.width() as f32 / 2.0;
    let dy = (f.bbox[1] + f.bbox[3]) / 2.0 - img.height() as f32 / 2.0;
    dx * dx + dy * dy
}

fn embed(det: &mut FaceDetector, emb: &mut FaceEmbedder, img: &image::RgbImage, centre: bool) -> Result<Row> {
    let faces = det.detect(img)?;
    let n = faces.len();
    let pick = if centre {
        faces.into_iter().min_by(|a, b| centre_dist(a, img).total_cmp(&centre_dist(b, img)))
    } else {
        faces.into_iter().max_by(|a, b| area(a).total_cmp(&area(b)))
    };
    let Some(face) = pick else {
        return Ok(Row { faces: 0, face: None, features: vec![0.0; DIM] });
    };
    let features = emb.embed_face(img, &face)?.features;
    anyhow::ensure!(features.len() == DIM, "embedding dim {}", features.len());
    Ok(Row { faces: n, face: Some(face), features })
}

fn write_variant(prefix: &str, name: &str, paths: &[String], rows: &[Row]) -> Result<()> {
    let mut bin = std::io::BufWriter::new(std::fs::File::create(format!("{prefix}.{name}.f32"))?);
    let mut tsv = std::io::BufWriter::new(std::fs::File::create(format!("{prefix}.{name}.tsv"))?);
    writeln!(tsv, "idx\tpath\tfaces\tscore\tx1\ty1\tx2\ty2")?;
    for (i, (p, r)) in paths.iter().zip(rows).enumerate() {
        for v in &r.features {
            bin.write_all(&v.to_le_bytes())?;
        }
        match &r.face {
            Some(f) => writeln!(
                tsv,
                "{i}\t{p}\t{}\t{:.4}\t{:.1}\t{:.1}\t{:.1}\t{:.1}",
                r.faces, f.score, f.bbox[0], f.bbox[1], f.bbox[2], f.bbox[3]
            )?,
            None => writeln!(tsv, "{i}\t{p}\t0\t0\t0\t0\t0\t0")?,
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    anyhow::ensure!(args.len() >= 5, "usage: embed_dir <models_dir> <image_root> <list.txt> <out_prefix> [workers]");
    let models = PathBuf::from(&args[1]);
    let root = PathBuf::from(&args[2]);
    let paths: Vec<String> =
        std::fs::read_to_string(&args[3])?.lines().filter(|l| !l.is_empty()).map(String::from).collect();
    let prefix = args[4].clone();
    let workers: usize = args.get(5).map(|s| s.parse()).transpose()?.unwrap_or(1);
    let centre = match args.get(6).map(String::as_str) {
        None | Some("largest") => false,
        Some("centre") => true,
        Some(other) => anyhow::bail!("unknown face selection {other:?}"),
    };

    let n = paths.len();
    let rgb: Mutex<Vec<Option<Row>>> = Mutex::new((0..n).map(|_| None).collect());
    let gray: Mutex<Vec<Option<Row>>> = Mutex::new((0..n).map(|_| None).collect());
    let next = AtomicUsize::new(0);
    let start = std::time::Instant::now();

    std::thread::scope(|s| -> Result<()> {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| -> Result<()> {
                    let mut det = FaceDetector::load(models.join("det_10g.onnx"))?;
                    let mut emb = FaceEmbedder::load(models.join("w600k_r50.onnx"))?;
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            return Ok(());
                        }
                        let path: &Path = &root.join(&paths[i]);
                        let img = image::open(path).with_context(|| path.display().to_string())?;
                        let color = img.to_rgb8();
                        let gray_rgb = gray_to_rgb(&img.to_luma8());
                        let a = embed(&mut det, &mut emb, &color, centre)?;
                        let b = embed(&mut det, &mut emb, &gray_rgb, centre)?;
                        rgb.lock().unwrap()[i] = Some(a);
                        gray.lock().unwrap()[i] = Some(b);
                        if i.is_multiple_of(500) {
                            eprintln!("{i}/{n} {:.0}s", start.elapsed().as_secs_f32());
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker panicked")?;
        }
        Ok(())
    })?;

    for (name, rows) in [("rgb", rgb), ("gray", gray)] {
        let rows: Vec<Row> = rows.into_inner().unwrap().into_iter().map(Option::unwrap).collect();
        write_variant(&prefix, name, &paths, &rows)?;
    }
    eprintln!("done {n} images in {:.0}s", start.elapsed().as_secs_f32());
    Ok(())
}
