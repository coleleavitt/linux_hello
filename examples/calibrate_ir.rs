//! Score saved IR frames of other people (impostors) and of the owner (genuine) against the
//! owner's template with the pipeline's own crop, and compare the impostor scores with the
//! calibration table (docs/calibration.md, "IR impostor check").
//!
//! Usage: calibrate_ir [--store PATH | --template-from DIR] <user> <impostor_dir> [genuine_dir]
//! Models from `LINUX_HELLO_MODELS` (default `models/`), the calibration table from
//! `calibration.json` there for the camera at /dev/video2.
//!
//! Frames in a directory:
//! - `*_lit.png` with `*_ambient.png` (from `examples/impostor_capture` or `linux_hello watch`), or
//!   `ir_NN_lit.png` with `ir_(NN-1)_dark.png`: the pair goes through `FacePipeline::analyze`, the
//!   print is the one an unlock would compare (largest face, ambient-subtracted crop, all frame
//!   gates); a lit frame without an ambient one is subtracted against a black frame;
//! - any other image except `*_ambient/_dark/_diff/_rgb.png` (e.g. a visible-light group photo):
//!   every SCRFD face is embedded on the grayscale image, as the LFW table was built
//!   (`examples/embed_dir`). These are pseudo-impostors, not IR captures.
//!
//! With the converted Windows Hello models, Microsoft's recogniser gives its second opinion (`ms`
//! distance, `ms_far`) on pairs, and "recognised" means both opinions accept (`PrintMatch::recognised`).
//! The template is `<user>`'s from the face store (`--store`, default the user's own store), or
//! built from the prints of the frames in `--template-from DIR` (a dry run without enrolling).
//! Prints one line per frame and a summary with the pass criteria of docs/calibration.md.

use anyhow::{bail, Context, Result};
use image::GrayImage;
use linux_hello::matcher::ENROLL_ON_FAILURE_THRESHOLD;
use linux_hello::{
    calibration, cosine, gray_to_rgb, sysfs, CalibrationTable, Embedding, FaceDetector, FaceEmbedder, FacePipeline,
    FaceStore, IrPair, LightingStatus, Template, WindowsHelloModels, TARGET_FAR,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Impostor frames scoring at or above this are flagged: on grayscale LFW a stranger's best
/// similarity over 15 prints passes 0.35 about 4 times in 100,000 probes (docs/calibration.md).
const SUSPECT_SIMILARITY: f32 = 0.35;

/// One scored face.
struct Scored {
    name: String,
    similarity: f32,
    far: f64,
    recognised: bool,
}

struct Tools {
    pipeline: FacePipeline,
    detector: FaceDetector,
    embedder: FaceEmbedder,
    table: CalibrationTable,
    /// Microsoft's table for the second opinion of the Windows recogniser, when its models exist
    windows: Option<CalibrationTable>,
}

/// The prints of every frame in `dir`, with what they are (`pair`, `lit`, `photo:<n>`).
fn prints(tools: &mut Tools, dir: &Path) -> Result<Vec<(String, String, Embedding)>> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .with_context(|| dir.display().to_string())?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".png") || n.ends_with(".jpg") || n.ends_with(".jpeg"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
        if ["_ambient", "_dark", "_diff", "_rgb"].iter().any(|s| stem.ends_with(s)) {
            continue;
        }
        let path = dir.join(name);
        let Some(base) = stem.strip_suffix("_lit") else {
            let image = image::open(&path).with_context(|| path.display().to_string())?.to_luma8();
            let faces = tools.detector.detect_gray(&image)?;
            let rgb = gray_to_rgb(&image);
            for (k, face) in faces.iter().enumerate() {
                out.push((name.clone(), format!("photo face {k}"), tools.embedder.embed_face(&rgb, face)?));
            }
            if faces.is_empty() {
                println!("{name}: no face");
            }
            continue;
        };
        let lit = image::open(&path).with_context(|| path.display().to_string())?.to_luma8();
        let ambient = ambient_of(dir, base, &lit)?;
        let (kind, ambient) = match ambient {
            Some(a) => ("pair", a),
            None => ("lit only", GrayImage::new(lit.width(), lit.height())),
        };
        // Timestamps far apart, so that the ambient warp never links two unrelated frames (F13).
        let at = Duration::from_secs(10 * i as u64 + 10);
        let pair = IrPair { lit, ambient, lit_at: at, ambient_at: at - Duration::from_millis(67) };
        let analysis = tools.pipeline.analyze(&pair)?;
        let status = analysis.status();
        match (analysis.print, status) {
            (Some(print), Some(LightingStatus::Good)) => out.push((name.clone(), kind.to_string(), print)),
            (Some(print), status) => out.push((name.clone(), format!("{kind}, lighting {status:?}"), print)),
            (None, _) => println!("{name}: no print ({})", analysis.problem.map_or("-", |p| p.as_str())),
        }
    }
    Ok(out)
}

/// `<base>_ambient.png`, or for `ir_NN` the `ir_(NN-1)_dark.png` of the validation set.
fn ambient_of(dir: &Path, base: &str, lit: &GrayImage) -> Result<Option<GrayImage>> {
    let mut candidates = vec![dir.join(format!("{base}_ambient.png"))];
    if let Some(n) = base.strip_prefix("ir_").and_then(|n| n.parse::<u32>().ok()) {
        candidates.push(dir.join(format!("ir_{:02}_dark.png", n.wrapping_sub(1))));
    }
    for path in candidates {
        if path.exists() {
            let ambient = image::open(&path)?.to_luma8();
            if ambient.dimensions() != lit.dimensions() {
                bail!("{}: size differs from its lit frame", path.display());
            }
            return Ok(Some(ambient));
        }
    }
    Ok(None)
}

/// Best match per frame (ArcFace, and Microsoft's second opinion where both sides have its
/// feature), and every single ArcFace comparison with a template print.
fn score(template: &Template, tools: &Tools, prints: Vec<(String, String, Embedding)>) -> (Vec<Scored>, Vec<f32>) {
    let mut comparisons = Vec::new();
    let scored = prints
        .into_iter()
        .map(|(name, kind, print)| {
            let m = template.best_match(&print, &tools.table, tools.windows.as_ref()).expect("template has prints");
            let ms = m.windows.map_or(String::new(), |w| format!(" ms {:.3} ms_far {:.1e}", w.distance, w.far));
            println!("{name} ({kind}): similarity {:.3} far {:.1e}{ms}", m.similarity, m.far);
            comparisons.extend(template.prints.iter().map(|p| cosine(&p.features, &print.features)));
            Scored {
                name: format!("{name} ({kind})"),
                similarity: m.similarity,
                far: m.far,
                recognised: m.recognised(),
            }
        })
        .collect();
    (scored, comparisons)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let mut args = std::env::args().skip(1).peekable();
    let usage = "usage: calibrate_ir [--store PATH | --template-from DIR] <user> <impostor_dir> [genuine_dir]";
    let (mut store, mut template_from) = (None, None);
    while let Some(flag) = args.peek().filter(|a| a.starts_with("--")).cloned() {
        args.next();
        let value = PathBuf::from(args.next().with_context(|| format!("{flag} needs a value"))?);
        match flag.as_str() {
            "--store" => store = Some(value),
            "--template-from" => template_from = Some(value),
            other => bail!("unknown option {other}"),
        }
    }
    let user = args.next().context(usage)?;
    let impostors = PathBuf::from(args.next().context(usage)?);
    let genuine = args.next().map(PathBuf::from);
    let models = PathBuf::from(
        std::env::var("LINUX_HELLO_MODELS").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()),
    );
    let store = store.unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/linux_hello/faces.json")
    });

    let table = calibration::load(models.join("calibration.json"), sysfs::usb_id("/dev/video2").as_deref())?;
    let mut pipeline = FacePipeline::new(
        models.join("det_10g.onnx"),
        models.join("w600k_r50.onnx"),
        Vec::new(),
        if template_from.is_some() { PathBuf::from("/nonexistent") } else { store.clone() },
        table.clone(),
    )?;
    let windows = models.join("windows_hello/converted");
    let windows_table = if windows.exists() {
        pipeline = pipeline.with_windows_hello(WindowsHelloModels::load(&windows)?);
        Some(calibration::load_windows(windows.join("cefa/MODEL_FILES_123.json"))?)
    } else {
        None
    };
    let mut tools = Tools {
        pipeline,
        detector: FaceDetector::load(models.join("det_10g.onnx"))?,
        embedder: FaceEmbedder::load(models.join("w600k_r50.onnx"))?,
        table,
        windows: windows_table,
    };

    let template = match &template_from {
        Some(dir) => {
            println!("== template from {}", dir.display());
            let prints = prints(&mut tools, dir)?;
            if prints.is_empty() {
                bail!("{}: no usable frames for the template", dir.display());
            }
            Template::enrolled(prints.into_iter().map(|(_, _, p)| p))
        }
        None => FaceStore::load(&store)?
            .get(&user)
            .cloned()
            .with_context(|| format!("{user} is not enrolled in {}", store.display()))?,
    };
    let n_prints = template.prints.len();
    println!("template: {user}, {n_prints} prints (table {})", tools.table.name());

    println!("== impostors: {}", impostors.display());
    let impostor_prints = prints(&mut tools, &impostors)?;
    let (impostor, comparisons) = score(&template, &tools, impostor_prints);
    let genuine = match &genuine {
        Some(dir) => {
            println!("== genuine: {}", dir.display());
            let genuine_prints = prints(&mut tools, dir)?;
            score(&template, &tools, genuine_prints).0
        }
        None => Vec::new(),
    };

    println!("== summary");
    let max = |s: &[Scored]| s.iter().map(|x| x.similarity).fold(f32::NEG_INFINITY, f32::max);
    let min = |s: &[Scored]| s.iter().map(|x| x.similarity).fold(f32::INFINITY, f32::min);
    let median = |s: &[Scored]| {
        let mut v: Vec<f32> = s.iter().map(|x| x.similarity).collect();
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(f32::NAN)
    };
    let recognised: Vec<&Scored> = impostor.iter().filter(|s| s.recognised).collect();
    let suspect: Vec<&Scored> = impostor.iter().filter(|s| s.similarity >= SUSPECT_SIMILARITY).collect();
    let addable = impostor.iter().filter(|s| s.similarity >= ENROLL_ON_FAILURE_THRESHOLD).count();
    println!(
        "impostor frames: {} (best-of-template similarity median {:.3}, max {:.3}); recognised (far < {TARGET_FAR:.0e}, both opinions): {}; \
         ≥ {SUSPECT_SIMILARITY}: {}; ≥ enroll-on-failure {ENROLL_ON_FAILURE_THRESHOLD}: {addable}",
        impostor.len(),
        median(&impostor),
        max(&impostor),
        recognised.len(),
        suspect.len(),
    );
    for s in recognised.iter().chain(&suspect) {
        println!("  {}: similarity {:.3} far {:.1e}", s.name, s.similarity, s.far);
    }
    if !genuine.is_empty() {
        let missed = genuine.iter().filter(|s| !s.recognised).count();
        println!(
            "genuine frames: {} (similarity min {:.3}, median {:.3}); not recognised on their own: {missed}",
            genuine.len(),
            min(&genuine),
            median(&genuine)
        );
    }
    // Every impostor print against every template print, against the table's per-comparison FPR:
    // only rows where the table expects at least one comparison above them say anything.
    println!("comparisons: {} impostor prints × {n_prints} template prints = {}", impostor.len(), comparisons.len());
    let rows: Vec<_> = tools
        .table
        .rows()
        .iter()
        .map(|row| {
            (row, row.fpr * comparisons.len() as f64, comparisons.iter().filter(|&&c| c > row.similarity).count())
        })
        .take_while(|(_, expected, observed)| *expected >= 0.5 || *observed > 0)
        .collect();
    if rows.is_empty() {
        println!(
            "  too few comparisons to check any table row (the lowest row expects {:.2} above it)",
            tools.table.rows()[0].fpr * comparisons.len() as f64
        );
    } else {
        println!("  row similarity | table FPR | expected above | observed above");
    }
    for (row, expected, observed) in rows {
        println!("  {:14.3} | {:9.2e} | {expected:14.1} | {observed:14}", row.similarity, row.fpr);
    }
    let pass = recognised.is_empty() && suspect.is_empty();
    println!("{}", if pass { "PASS" } else { "FAIL" });
    if !pass {
        std::process::exit(1);
    }
    Ok(())
}
