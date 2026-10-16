//! Score the physics liveness checks (docs/liveness-physics.md) on saved recordings: one JSON
//! line per pair with every check's score for the largest face (and its lighting numbers and
//! Windows frame total, the input of `tools/train_gate.py`), then a summary per recording
//! (count, min, 1st/10th/50th/90th percentiles, max per check, and each check's maximum over
//! the recording's 16-pair sessions). The pipeline runs as in `watch`, with the Windows Hello
//! models when `<models_dir>/windows_hello/converted` exists and no template (nothing matches).
//!
//! A recording is a `watch`/`capture` directory (`NNNN_lit.png`, `NNNN_ambient.png`,
//! `NNNN_rgb.png`) or `validation_frames` (`ir_NN_lit.png` with `ir_(NN-1)_dark.png`;
//! `watch_*_ir_lit.png` with `watch_*_rgb.png`, no ambient frame). Pairs are given the
//! camera's timing (133 ms apart, ambient 67 ms before the lit frame).
//! Usage: cargo run --release --example physics_replay -- [--max-ambient N] [--gate <trained_gate.json>] <recording_dir>... [--models <dir>]

use anyhow::{Context, Result};
use image::GrayImage;
use linux_hello::authenticator::observe;
use linux_hello::liveness::physics;
use linux_hello::{calibration, Capture, FacePipeline, IrPair, Session, WindowsHelloModels};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The frames of one pair by file name.
struct Files {
    lit: PathBuf,
    ambient: Option<PathBuf>,
    rgb: Option<PathBuf>,
}

fn recording(dir: &Path) -> Result<Vec<Files>> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir)?.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect();
    names.sort();
    let exists = |name: String| Some(dir.join(&name)).filter(|p| p.exists());
    let mut pairs = Vec::new();
    for name in names {
        let lit = dir.join(&name);
        if let Some(n) = name.strip_suffix("_lit.png").filter(|n| n.chars().all(|c| c.is_ascii_digit())) {
            pairs.push(Files { lit, ambient: exists(format!("{n}_ambient.png")), rgb: exists(format!("{n}_rgb.png")) });
        } else if let Some(n) = name.strip_prefix("ir_").and_then(|r| r.strip_suffix("_lit.png")) {
            let n: u32 = n.parse()?;
            pairs.push(Files { lit, ambient: exists(format!("ir_{}_dark.png", n - 1)), rgb: None });
        } else if let Some(n) = name.strip_suffix("_ir_lit.png") {
            pairs.push(Files { lit, ambient: None, rgb: exists(format!("{n}_rgb.png")) });
        }
    }
    Ok(pairs)
}

fn load(files: &Files, n: usize) -> Result<Capture> {
    let lit = image::open(&files.lit)?.to_luma8();
    let ambient = match &files.ambient {
        Some(p) => image::open(p)?.to_luma8(),
        None => GrayImage::new(lit.width(), lit.height()),
    };
    let rgb = files.rgb.as_ref().map(|p| image::open(p).map(|i| i.to_rgb8())).transpose()?;
    let lit_at = Duration::from_millis(133 * n as u64 + 67);
    let pair = IrPair { lit, ambient, lit_at, ambient_at: lit_at - Duration::from_millis(67) };
    Ok(Capture { pair, rgb })
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    let i = ((sorted.len() - 1) as f32 * p).round() as usize;
    sorted[i]
}

fn main() -> Result<()> {
    let mut dirs = Vec::new();
    let mut gate = None;
    let mut models = std::env::var("LINUX_HELLO_MODELS")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/models").to_string());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => models = args.next().context("--models <dir>")?,
            "--max-ambient" => linux_hello::lighting::set_max_ambient(args.next().context("--max-ambient N")?.parse()?),
            "--gate" => gate = Some(args.next().context("--gate <trained_gate.json>")?),
            _ => dirs.push(PathBuf::from(arg)),
        }
    }
    if dirs.is_empty() {
        anyhow::bail!("usage: physics_replay [--max-ambient N] [--gate <json>] <recording_dir>... [--models <dir>]");
    }
    let calibration = calibration::load(format!("{models}/calibration.json"), None)?;
    let store = std::env::temp_dir().join("physics_replay_empty_store.json");
    let mut pipeline = FacePipeline::new(
        format!("{models}/det_10g.onnx"),
        format!("{models}/w600k_r50.onnx"),
        Vec::new(),
        &store,
        calibration,
    )?;
    let converted = PathBuf::from(format!("{models}/windows_hello/converted"));
    if converted.exists() {
        pipeline = pipeline.with_windows_hello(WindowsHelloModels::load(&converted)?);
    }
    let mut checks = physics::with_gate(gate.map(physics::TrainedGate::load).transpose()?);

    for dir in dirs {
        let mut session = Session::default();
        let mut history = Vec::new();
        let mut scores: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut session_max: BTreeMap<String, Vec<f32>> = BTreeMap::new();
        let mut current_max: BTreeMap<String, f32> = BTreeMap::new();
        let mut pairs = 0;
        for (n, files) in recording(&dir)?.iter().enumerate() {
            let capture = load(files, n)?;
            let (check, verdict) = observe(&mut pipeline, &mut checks, "", capture, &mut session, &mut history)?;
            let face = check.largest();
            let line: BTreeMap<&str, serde_json::Value> = face
                .map(|f| f.physics.iter().map(|(k, v)| (k.as_str(), serde_json::json!(v))).collect())
                .unwrap_or_default();
            let status = face.map(|f| match (f.analysis.problem, f.analysis.status()) {
                (Some(p), _) => p.as_str().to_string(),
                (None, Some(s)) => format!("{s:?}").to_lowercase(),
                (None, None) => "-".to_string(),
            });
            let lighting = face.and_then(|f| f.analysis.lighting);
            println!(
                "{}",
                serde_json::json!({
                    "n": n, "file": files.lit.file_name().unwrap().to_str(), "status": status,
                    "lit": lighting.map(|l| l.lit), "ambient": lighting.map(|l| l.ambient), "d": lighting.map(|l| l.gain()),
                    "windows_total": face.and_then(|f| f.windows.as_ref()).map(|w| w.total),
                    "scores": line,
                })
            );
            if let Some(f) = face {
                for (k, v) in &f.physics {
                    if let Some(v) = v {
                        scores.entry(k.clone()).or_default().push(*v);
                        let m = current_max.entry(k.clone()).or_insert(f32::MIN);
                        *m = m.max(*v);
                    }
                }
            }
            pairs += 1;
            if verdict.is_some() || pairs == 16 {
                for (k, v) in std::mem::take(&mut current_max) {
                    session_max.entry(k).or_default().push(v);
                }
                session = Session::default();
                history.clear();
                pairs = 0;
            }
        }
        for (k, v) in std::mem::take(&mut current_max) {
            session_max.entry(k).or_default().push(v);
        }
        eprintln!("== {}", dir.display());
        for (name, values) in &mut scores {
            values.sort_by(f32::total_cmp);
            let mut smax = session_max.get(name).cloned().unwrap_or_default();
            smax.sort_by(f32::total_cmp);
            eprintln!(
                "{name:>10}: n={:3} min={:7.3} p1={:7.3} p10={:7.3} p50={:7.3} p90={:7.3} max={:7.3} | session max: min={:7.3} p50={:7.3} ({} sessions)",
                values.len(),
                values[0],
                percentile(values, 0.01),
                percentile(values, 0.10),
                percentile(values, 0.50),
                percentile(values, 0.90),
                values[values.len() - 1],
                smax.first().copied().unwrap_or(f32::NAN),
                if smax.is_empty() { f32::NAN } else { percentile(&smax, 0.5) },
                smax.len(),
            );
        }
    }
    Ok(())
}
