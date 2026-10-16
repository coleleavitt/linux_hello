//! Run `FacePipeline::check` on saved IR frames and report the time per pair and what each face
//! gave, to measure the cost of analysing more than one face (docs/capture-and-liveness.md §7).
//!
//! Usage: cargo run --release --example check_frames -- <lit.png> [--ambient <ambient.png>]
//!        [--rgb <rgb.png>] [--store <faces.json>] [--user <user>] [--runs N] [models_dir]
//!
//! Without an ambient frame a black one is used (ambient 0). The store is read as is; without
//! `--store` no template is matched (`similarity=-`).

use anyhow::{Context, Result};
use image::GrayImage;
use linux_hello::authenticator::describe;
use linux_hello::{calibration, FacePipeline, IrPair, Session, WindowsHelloModels};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let lit = PathBuf::from(args.next().context("usage: check_frames <lit.png> [options] [models_dir]")?);
    let (mut ambient, mut rgb, mut store, mut user, mut runs, mut models) =
        (None, None, None, "user".to_string(), 20, None);
    while let Some(arg) = args.next() {
        let mut value = || args.next().context("missing value");
        match arg.as_str() {
            "--ambient" => ambient = Some(PathBuf::from(value()?)),
            "--rgb" => rgb = Some(PathBuf::from(value()?)),
            "--store" => store = Some(PathBuf::from(value()?)),
            "--user" => user = value()?,
            "--runs" => runs = value()?.parse()?,
            other => models = Some(PathBuf::from(other)),
        }
    }
    let models = models.unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/models")));
    let lit = image::open(&lit)?.to_luma8();
    let ambient = match ambient {
        Some(p) => image::open(p)?.to_luma8(),
        None => GrayImage::new(lit.width(), lit.height()),
    };
    let rgb = rgb.map(|p| image::open(p).map(|i| i.to_rgb8())).transpose()?;
    let store = store.unwrap_or_else(|| PathBuf::from("/nonexistent/faces.json"));
    let calibration = calibration::load(models.join("calibration.json"), None)?;
    let mut pipeline =
        FacePipeline::new(models.join("det_10g.onnx"), models.join("w600k_r50.onnx"), Vec::new(), store, calibration)?;
    let windows = models.join("windows_hello/converted");
    if windows.exists() {
        pipeline = pipeline.with_windows_hello(WindowsHelloModels::load(&windows)?);
    }

    let mut session = Session::default();
    let mut total = Duration::ZERO;
    let mut check = None;
    for i in 0..runs {
        // Consecutive pairs 67 ms apart, so the ambient warp (F13) runs from the second pair on.
        let at = Duration::from_millis(67 * (2 * i + 1) as u64);
        let pair = IrPair {
            lit: lit.clone(),
            ambient: ambient.clone(),
            lit_at: at,
            ambient_at: at - Duration::from_millis(67),
        };
        let start = Instant::now();
        let c = pipeline.check(&user, &pair, rgb.as_ref())?;
        let elapsed = start.elapsed();
        if i > 0 {
            total += elapsed;
        }
        session.add(&c);
        check = Some(c);
    }
    let check = check.context("runs = 0")?;
    println!("{}", describe(&check, &session));
    println!(
        "faces={} ms/pair={:.1} (mean over {} runs after the first)",
        check.faces.len(),
        total.as_secs_f64() * 1e3 / (runs - 1).max(1) as f64,
        runs - 1
    );
    Ok(())
}
