//! Record gated IR frame pairs of a volunteer who is not enrolled, for the IR impostor check of
//! `examples/calibrate_ir` (docs/calibration.md, "IR impostor check"). Ask for consent first; the
//! frames are face images and stay on this machine.
//!
//! Usage: impostor_capture <out_dir> [--frames N] [--seconds S]
//! Devices: `LINUX_HELLO_IR` (default /dev/video2), `LINUX_HELLO_IR_META` (default /dev/video3);
//! models from `LINUX_HELLO_MODELS` (default `models/`).
//!
//! A pair is kept only when the pipeline would take a print from it in an unlock with good
//! lighting (face ≥ 72 px, away from the edges, frontal, not occluded, lit 35–205, lit − ambient
//! ≥ 45, ambient ≤ 25; `FacePipeline::analyze`), so the recorded set is what an impostor attempt
//! would compare. Writes `NNNN_lit.png`, `NNNN_ambient.png` and `frames.jsonl` (face box, lit and
//! ambient brightness, gain) into `out_dir`; the face-metered exposure runs as in an unlock.

use anyhow::{Context, Result};
use linux_hello::{calibration, FacePipeline, IrCamera, LightingStatus, WindowsHelloModels};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let mut args = std::env::args().skip(1).peekable();
    let out = PathBuf::from(args.next().context("usage: impostor_capture <out_dir> [--frames N] [--seconds S]")?);
    let (mut wanted, mut seconds) = (32usize, 60u64);
    while let Some(flag) = args.peek().filter(|a| a.starts_with("--")).cloned() {
        args.next();
        let value = args.next().with_context(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--frames" => wanted = value.parse()?,
            "--seconds" => seconds = value.parse()?,
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    let models = PathBuf::from(
        std::env::var("LINUX_HELLO_MODELS").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()),
    );
    let device = std::env::var("LINUX_HELLO_IR").unwrap_or_else(|_| "/dev/video2".into());
    let meta = std::env::var("LINUX_HELLO_IR_META").unwrap_or_else(|_| "/dev/video3".into());

    std::fs::create_dir_all(&out)?;
    // The gates do not need a face store; an absent file is an empty one.
    let table = calibration::load(models.join("calibration.json"), None)?;
    let mut pipeline = FacePipeline::new(
        models.join("det_10g.onnx"),
        models.join("w600k_r50.onnx"),
        Vec::new(),
        out.join("no_store.json"),
        table,
    )?;
    let windows = models.join("windows_hello/converted");
    if windows.exists() {
        pipeline = pipeline.with_windows_hello(WindowsHelloModels::load(&windows)?);
    }
    let mut log = std::fs::File::create(out.join("frames.jsonl"))?;

    println!("Volunteer: look at the camera and move your head a little, as when talking to someone.");
    let mut camera = IrCamera::open(&device, Some(&meta))?;
    let start = Instant::now();
    let (mut kept, mut seen) = (0usize, 0usize);
    while kept < wanted && start.elapsed() < Duration::from_secs(seconds) {
        let pair = camera.capture_pair()?;
        let analysis = pipeline.analyze(&pair)?;
        seen += 1;
        if let Some(face) = &analysis.face {
            camera.expose_on(face.bbox);
        }
        let (Some(face), Some(lighting), true) = (&analysis.face, analysis.lighting, analysis.print.is_some()) else {
            print!(
                "\r{kept}/{wanted} ({seen} pairs, skipped: {})",
                analysis.problem.map_or("weak lighting", |p| p.as_str())
            );
            let _ = std::io::stdout().flush();
            continue;
        };
        if lighting.status() != LightingStatus::Good {
            print!("\r{kept}/{wanted} ({seen} pairs, skipped: weak lighting)");
            let _ = std::io::stdout().flush();
            continue;
        }
        pair.lit.save(out.join(format!("{kept:04}_lit.png")))?;
        pair.ambient.save(out.join(format!("{kept:04}_ambient.png")))?;
        writeln!(
            log,
            "{}",
            serde_json::json!({
                "n": kept, "t": start.elapsed().as_secs_f32(), "bbox": face.bbox,
                "lit": lighting.lit, "ambient": lighting.ambient, "gain": lighting.gain(),
            })
        )?;
        kept += 1;
        print!("\r{kept}/{wanted} ({seen} pairs)");
        let _ = std::io::stdout().flush();
    }
    println!();
    println!("{kept} gated pairs of {seen} in {:.1} s into {}", start.elapsed().as_secs_f32(), out.display());
    Ok(())
}
