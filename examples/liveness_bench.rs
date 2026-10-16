//! Score labelled recordings (`linux_hello capture`, docs/liveness-bench.md) with every liveness
//! check behind the [`Check`] interface and with the daemon's own decision, and tabulate genuine
//! against attack acceptance per check and threshold.
//!
//! Usage: liveness_bench [--store PATH | --enroll-from DIR] [--user NAME] [--max-ambient N]
//!                       [--out DIR] <recordings>...
//!
//! `<recordings>` is a recording (a directory with `meta.json`, or `label:dir` for a `watch`
//! output without one) or a directory of recordings. The template is `--user`'s (default `$USER`)
//! from `--store` (default the user's own store), or built from the good frames of the genuine
//! recording in `--enroll-from` into `<out>/store.json`. Models from `LINUX_HELLO_MODELS`; the
//! converted Windows Hello models run when present.
//!
//! Every pair goes through `FacePipeline::check` and `Session::add` exactly as in the daemon
//! (sessions back to back, re-attempts included), so the per-session verdicts are what an unlock
//! would give; every check then scores the largest face of the pair with the pairs before it in
//! view. Writes `<out>/bench.json` (every score, every threshold) and `<out>/bench.md` (the
//! tables), and prints the markdown.

use anyhow::{bail, Context, Result};
use image::{GrayImage, RgbImage};
use linux_hello::authenticator::{Capture, MAX_PAIRS};
use linux_hello::liveness::{check, Check, Frame, Label, LivenessSession, LivenessState, Meta, Observed, Vote};
use linux_hello::{calibration, lighting, FacePipeline, IrPair, LightingStatus, Session, WindowsHelloModels};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A recording to score.
struct Recording {
    dir: PathBuf,
    label: Label,
    note: Option<String>,
}

/// One pair of a recording as read from disk.
struct Pair {
    n: u32,
    capture: Capture,
}

/// Scores of one recording.
#[derive(Serialize)]
struct Scored {
    dir: String,
    label: Label,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    pairs: usize,
    /// Lighting status or rejection reason of the largest face, per pair
    status: Vec<String>,
    /// Verdicts of the completed sessions, in order
    verdicts: Vec<String>,
    /// Per check, the score of the largest face per pair (`null` where the check did not run)
    scores: BTreeMap<String, Vec<Option<f32>>>,
}

/// Acceptance at one threshold, over frames and over 16-pair sessions, per label.
#[derive(Serialize, Clone)]
struct Rate {
    /// Frames passing / frames scored
    frames: BTreeMap<Label, [usize; 2]>,
    /// Sessions accepted (two live votes before the window ends) / sessions
    sessions: BTreeMap<Label, [usize; 2]>,
}

#[derive(Serialize)]
struct Sweep {
    name: String,
    higher_is_live: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    today: Option<f32>,
    /// Every distinct score as a threshold, in order, with its acceptance
    thresholds: Vec<(f32, Rate)>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let mut args = std::env::args().skip(1).peekable();
    let usage = "usage: liveness_bench [--store PATH | --enroll-from DIR] [--user NAME] [--max-ambient N] [--out DIR] <recordings>...";
    let (mut store, mut enroll_from, mut user, mut max_ambient) = (None, None, None, None);
    let mut out = PathBuf::from("/tmp/liveness_bench");
    let mut inputs = Vec::new();
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--store" => store = Some(PathBuf::from(value()?)),
            "--enroll-from" => enroll_from = Some(PathBuf::from(value()?)),
            "--user" => user = Some(value()?),
            "--max-ambient" => max_ambient = Some(value()?.parse::<f32>()?),
            "--out" => out = PathBuf::from(value()?),
            other if other.starts_with("--") => bail!("unknown option {other}\n{usage}"),
            _ => inputs.push(arg),
        }
    }
    if inputs.is_empty() {
        bail!(usage);
    }
    let user = user.unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
    let models = PathBuf::from(
        std::env::var("LINUX_HELLO_MODELS").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/models").into()),
    );
    if let Some(limit) = max_ambient {
        lighting::set_max_ambient(limit);
    }
    std::fs::create_dir_all(&out)?;

    let recordings = find_recordings(&inputs)?;
    let store = match (&enroll_from, store) {
        (Some(_), Some(_)) => bail!("--store and --enroll-from exclude each other"),
        (Some(_), None) => {
            let path = out.join("store.json");
            let _ = std::fs::remove_file(&path);
            path
        }
        (None, Some(path)) => path,
        (None, None) => {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/linux_hello/faces.json")
        }
    };
    // The bench reads saved frames; the camera id for the calibration table is the laptop's IR camera.
    let table =
        calibration::load(models.join("calibration.json"), linux_hello::sysfs::usb_id("/dev/video2").as_deref())?;
    let mut pipeline =
        FacePipeline::new(models.join("det_10g.onnx"), models.join("w600k_r50.onnx"), Vec::new(), &store, table)?;
    let windows = models.join("windows_hello/converted");
    if windows.exists() {
        pipeline = pipeline.with_windows_hello(WindowsHelloModels::load(&windows)?);
    }
    if let Some(dir) = &enroll_from {
        let n = enroll(&mut pipeline, &user, dir)?;
        println!("template: {n} prints of {user} from {}", dir.display());
    } else if !pipeline.is_enrolled(&user) {
        bail!("{user} is not enrolled in {}; use --store or --enroll-from", store.display());
    }

    let mut checks = check::all();
    let mut scored = Vec::with_capacity(recordings.len());
    for recording in &recordings {
        eprintln!("scoring {} ({})", recording.dir.display(), recording.label.as_str());
        scored.push(score_recording(&mut pipeline, &mut checks, &user, recording)?);
    }

    let sweeps: Vec<Sweep> = checks.iter().map(|c| sweep(c.as_ref(), &scored)).collect();
    let markdown = markdown(&scored, &sweeps, &user, lighting::max_ambient(), enroll_from.as_deref());
    std::fs::write(out.join("bench.md"), &markdown)?;
    std::fs::write(
        out.join("bench.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "user": user, "max_ambient": lighting::max_ambient(),
            "enroll_from": enroll_from.as_ref().map(|d| d.display().to_string()),
            "recordings": scored, "checks": sweeps,
        }))?,
    )?;
    print!("{markdown}");
    eprintln!("written {}/bench.md and bench.json", out.display());
    Ok(())
}

/// The recordings named by the arguments: `label:dir`, a directory with `meta.json`, or a
/// directory whose subdirectories are recordings.
fn find_recordings(inputs: &[String]) -> Result<Vec<Recording>> {
    let mut found = Vec::new();
    for input in inputs {
        if let Some((label, dir)) = input.split_once(':').filter(|(l, _)| l.parse::<Label>().is_ok()) {
            let dir = PathBuf::from(dir);
            let note = Meta::load(&dir).ok().and_then(|m| m.note);
            found.push(Recording { dir, label: label.parse().expect("checked"), note });
            continue;
        }
        let dir = PathBuf::from(input);
        if let Ok(meta) = Meta::load(&dir) {
            found.push(Recording { dir, label: meta.label, note: meta.note });
            continue;
        }
        let mut subdirs: Vec<PathBuf> = std::fs::read_dir(&dir)
            .with_context(|| dir.display().to_string())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join(Meta::FILE).exists())
            .collect();
        subdirs.sort();
        if subdirs.is_empty() {
            bail!("{}: no meta.json and no recordings in it (give label:dir for a watch output)", dir.display());
        }
        for dir in subdirs {
            let meta = Meta::load(&dir)?;
            found.push(Recording { dir, label: meta.label, note: meta.note });
        }
    }
    Ok(found)
}

/// The pairs of a recording, in order: `NNNN_lit.png` + `NNNN_ambient.png` (+ `NNNN_rgb.png`),
/// timed by `log.jsonl` (`lit_at`/`ambient_at`, else `t` and `gap_ms`), else by 15 fps.
fn load_pairs(dir: &Path) -> Result<Vec<Pair>> {
    let mut times: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
    if let Ok(log) = std::fs::read_to_string(dir.join("log.jsonl")) {
        for line in log.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line)?;
            let n = v["n"].as_u64().context("log.jsonl: n")? as u32;
            let t = v["t"].as_f64().unwrap_or(0.0);
            let gap = v["gap_ms"].as_f64().unwrap_or(67.0) / 1000.0;
            let lit_at = v["lit_at"].as_f64().unwrap_or(t);
            let ambient_at = v["ambient_at"].as_f64().unwrap_or(lit_at - gap);
            times.insert(n, (lit_at, ambient_at));
        }
    }
    let mut ids: Vec<u32> = std::fs::read_dir(dir)
        .with_context(|| dir.display().to_string())?
        .filter_map(|e| e.ok()?.file_name().into_string().ok()?.strip_suffix("_lit.png")?.parse().ok())
        .collect();
    ids.sort_unstable();
    let gray = |name: String| -> Result<GrayImage> {
        Ok(image::open(dir.join(&name)).with_context(|| name.clone())?.to_luma8())
    };
    let mut pairs = Vec::with_capacity(ids.len());
    for n in ids {
        let lit = gray(format!("{n:04}_lit.png"))?;
        let ambient = gray(format!("{n:04}_ambient.png"))?;
        let rgb: Option<RgbImage> = image::open(dir.join(format!("{n:04}_rgb.png"))).ok().map(|i| i.to_rgb8());
        let (lit_at, ambient_at) = times
            .get(&n)
            .copied()
            .unwrap_or_else(|| (f64::from(n) * 2.0 / 15.0, f64::from(n) * 2.0 / 15.0 - 1.0 / 15.0));
        let pair = IrPair {
            lit,
            ambient,
            lit_at: Duration::from_secs_f64(lit_at.max(0.0)),
            ambient_at: Duration::from_secs_f64(ambient_at.max(0.0)),
        };
        pairs.push(Pair { n, capture: Capture { pair, rgb } });
    }
    if pairs.is_empty() {
        bail!("{}: no NNNN_lit.png frames", dir.display());
    }
    Ok(pairs)
}

/// A template for the bench from the good-lighting prints of a genuine recording, as `enroll`
/// takes them (near-duplicates above `DUPLICATE_PRINT` skipped, `ENROLL_PRINTS` at most).
fn enroll(pipeline: &mut FacePipeline, user: &str, dir: &Path) -> Result<usize> {
    use linux_hello::authenticator::{DUPLICATE_PRINT, ENROLL_PRINTS};
    let mut prints = Vec::new();
    pipeline.set_enrolling(true);
    for pair in load_pairs(dir)? {
        let Some(analysis) = pipeline.analyze(&pair.capture.pair)?.into_iter().next() else { continue };
        if analysis.status() != Some(LightingStatus::Good) {
            continue;
        }
        let Some(print) = analysis.print else { continue };
        if prints.iter().any(|p: &linux_hello::Embedding| p.similarity(&print) > DUPLICATE_PRINT) {
            continue;
        }
        prints.push(print);
        if prints.len() == ENROLL_PRINTS {
            break;
        }
    }
    pipeline.set_enrolling(false);
    if prints.is_empty() {
        bail!("{}: no good-lighting frame to enroll from", dir.display());
    }
    Ok(pipeline.enroll(user, prints, true, false)?.0)
}

/// Replay one recording through the daemon's decision and every check.
fn score_recording(
    pipeline: &mut FacePipeline,
    checks: &mut [Box<dyn Check>],
    user: &str,
    recording: &Recording,
) -> Result<Scored> {
    let pairs = load_pairs(&recording.dir)?;
    let mut session = Session::default();
    let mut previous: Vec<Observed> = Vec::new();
    let mut scores: BTreeMap<String, Vec<Option<f32>>> =
        checks.iter().map(|c| (c.name().to_string(), Vec::with_capacity(pairs.len()))).collect();
    let (mut status, mut verdicts) = (Vec::with_capacity(pairs.len()), Vec::new());
    for Pair { n, capture } in pairs {
        let check = pipeline.check(user, &capture.pair, capture.rgb.as_ref())?;
        let verdict = session.add(&check);
        let tracks: Vec<u32> = session.attempts().map(|(id, _)| id).collect();
        status.push(check.largest().map_or("no_face".to_string(), |f| {
            match (f.analysis.problem, f.analysis.status()) {
                (Some(p), _) => p.as_str().to_string(),
                (None, Some(s)) => format!("{s:?}").to_lowercase(),
                (None, None) => "-".to_string(),
            }
        }));
        let current = Observed { capture, check, tracks };
        let frame = Frame { current: &current, previous: &previous };
        for check in checks.iter_mut() {
            let score = if current.check.faces.is_empty() {
                None
            } else {
                check.score(&frame, 0).with_context(|| format!("{} on pair {n}", check.name()))?
            };
            scores.get_mut(check.name()).expect("registered").push(score);
        }
        if let Some(verdict) = verdict {
            verdicts.push(verdict.to_string());
            session = Session::default();
        }
        previous.push(current);
        if previous.len() > MAX_PAIRS as usize {
            previous.remove(0);
        }
    }
    Ok(Scored {
        dir: recording.dir.display().to_string(),
        label: recording.label,
        note: recording.note.clone(),
        pairs: status.len(),
        status,
        verdicts,
        scores,
    })
}

/// Acceptance of one check at `threshold` over every recording.
fn rate(check: &dyn Check, scored: &[Scored], threshold: f32) -> Rate {
    let mut rate = Rate { frames: BTreeMap::new(), sessions: BTreeMap::new() };
    for recording in scored {
        let scores = &recording.scores[check.name()];
        let frames = rate.frames.entry(recording.label).or_default();
        let sessions = rate.sessions.entry(recording.label).or_default();
        for s in scores.iter().flatten() {
            frames[0] += usize::from(check.passes(*s, threshold));
            frames[1] += 1;
        }
        // Sessions of MAX_PAIRS pairs back to back, the check as the only voter: a frame that
        // passes votes live, one that fails votes spoof, one without a score abstains.
        for window in scores.chunks(MAX_PAIRS as usize) {
            let mut votes = LivenessSession::default();
            for s in window {
                votes.add(
                    s.map_or(Vote::Abstain, |s| if check.passes(s, threshold) { Vote::Live } else { Vote::Spoof }),
                );
            }
            sessions[0] += usize::from(votes.state() == LivenessState::Live);
            sessions[1] += 1;
        }
    }
    rate
}

/// The check's acceptance at every distinct score and at today's threshold.
fn sweep(check: &dyn Check, scored: &[Scored]) -> Sweep {
    let mut values: Vec<f32> = scored.iter().flat_map(|r| r.scores[check.name()].iter().flatten().copied()).collect();
    values.extend(check.threshold());
    values.sort_by(f32::total_cmp);
    values.dedup();
    Sweep {
        name: check.name().to_string(),
        higher_is_live: check.higher_is_live(),
        today: check.threshold(),
        thresholds: values.into_iter().map(|t| (t, rate(check, scored, t))).collect(),
    }
}

fn percent(n: [usize; 2]) -> String {
    if n[1] == 0 {
        "-".to_string()
    } else {
        format!("{:.0}% ({}/{})", 100.0 * n[0] as f64 / n[1] as f64, n[0], n[1])
    }
}

fn markdown(scored: &[Scored], sweeps: &[Sweep], user: &str, max_ambient: f32, enroll_from: Option<&Path>) -> String {
    let mut md = String::new();
    let labels: Vec<Label> = Label::ALL.into_iter().filter(|l| scored.iter().any(|r| r.label == *l)).collect();
    let _ = writeln!(md, "# Liveness bench: {user}, ambient limit {max_ambient}\n");
    if let Some(dir) = enroll_from {
        let _ = writeln!(md, "Template from the good frames of `{}`.\n", dir.display());
    }

    let _ = writeln!(md, "## Recordings\n");
    let _ = writeln!(md, "| Recording | Label | Pairs | Largest face: status | Sessions (daemon decision) |");
    let _ = writeln!(md, "|---|---|---|---|---|");
    for r in scored {
        let mut statuses: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &r.status {
            *statuses.entry(s).or_default() += 1;
        }
        let statuses: Vec<String> = statuses.iter().map(|(s, n)| format!("{s} {n}")).collect();
        let mut verdicts: BTreeMap<&str, usize> = BTreeMap::new();
        for v in &r.verdicts {
            *verdicts.entry(v.split(' ').next().unwrap_or(v)).or_default() += 1;
        }
        let verdicts: Vec<String> = verdicts.iter().map(|(v, n)| format!("{v} {n}")).collect();
        let note = r.note.as_deref().map_or(String::new(), |n| format!(" ({n})"));
        let _ = writeln!(
            md,
            "| `{}`{note} | {} | {} | {} | {} |",
            r.dir,
            r.label.as_str(),
            r.pairs,
            statuses.join(", "),
            if verdicts.is_empty() { "none completed".to_string() } else { verdicts.join(", ") }
        );
    }

    let _ = writeln!(md, "\n## Daemon decision today\n");
    let _ = writeln!(md, "| Label | Sessions | MATCH | NOMATCH | NOTLIVE | NOFACE |");
    let _ = writeln!(md, "|---|---|---|---|---|---|");
    for label in &labels {
        let mut counts = [0usize; 5];
        for r in scored.iter().filter(|r| r.label == *label) {
            for v in &r.verdicts {
                counts[0] += 1;
                let i = match v.split(' ').next() {
                    Some("MATCH") => 1,
                    Some("NOMATCH") => 2,
                    Some("NOTLIVE") => 3,
                    _ => 4,
                };
                counts[i] += 1;
            }
        }
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} |",
            label.as_str(),
            counts[0],
            percent([counts[1], counts[0]]),
            counts[2],
            counts[3],
            counts[4]
        );
    }

    let _ = writeln!(md, "\n## Per check: scores\n");
    let _ = writeln!(md, "Largest face per pair; `n` = pairs the check scored. Min / median / max.\n");
    let _ =
        writeln!(md, "| Check | Live when | {} |", labels.iter().map(|l| l.as_str()).collect::<Vec<_>>().join(" | "));
    let _ = writeln!(md, "|---|---|{}", "---|".repeat(labels.len()));
    for s in sweeps {
        let direction = format!("{} {}", if s.higher_is_live { "≥" } else { "≤" }, s.today.map_or("?".into(), fmt));
        let cells: Vec<String> = labels
            .iter()
            .map(|label| {
                let mut v: Vec<f32> = scored
                    .iter()
                    .filter(|r| r.label == *label)
                    .flat_map(|r| r.scores[&s.name].iter().flatten().copied())
                    .collect();
                v.sort_by(f32::total_cmp);
                match v.as_slice() {
                    [] => "-".to_string(),
                    _ => format!("{} / {} / {} (n={})", fmt(v[0]), fmt(v[v.len() / 2]), fmt(v[v.len() - 1]), v.len()),
                }
            })
            .collect();
        let _ = writeln!(md, "| {} | {direction} | {} |", s.name, cells.join(" | "));
    }

    let _ = writeln!(md, "\n## Per check and threshold: genuine vs attack acceptance\n");
    let _ = writeln!(
        md,
        "Frames: scored frames passing the threshold. Sessions: 16-pair windows accepted with the check as the \
         only voter (pass = live, fail = spoof, unscored = abstain; two live votes accept). `today` is the \
         threshold the pipeline applies; `best` the threshold with the widest gap between genuine and attack \
         session acceptance; `strict` the loosest threshold at which no attack session is accepted. The full \
         sweep is in bench.json.\n"
    );
    for s in sweeps {
        let attack = |r: &Rate| {
            let (mut a, mut n) = (0, 0);
            for (label, v) in &r.sessions {
                if !label.is_genuine() {
                    a += v[0];
                    n += v[1];
                }
            }
            (a, n)
        };
        let genuine = |r: &Rate| r.sessions.get(&Label::Genuine).copied().unwrap_or([0, 0]);
        let gap = |r: &Rate| {
            let (a, n) = attack(r);
            let g = genuine(r);
            let f = |x: usize, n: usize| if n == 0 { 0.0 } else { x as f64 / n as f64 };
            f(g[0], g[1]) - f(a, n)
        };
        let best = s.thresholds.iter().max_by(|a, b| gap(&a.1).total_cmp(&gap(&b.1)));
        let strict = s.thresholds.iter().filter(|(_, r)| attack(r).1 > 0 && attack(r).0 == 0).max_by(|a, b| {
            genuine(&a.1)[0].cmp(&genuine(&b.1)[0]).then(if s.higher_is_live {
                b.0.total_cmp(&a.0)
            } else {
                a.0.total_cmp(&b.0)
            })
        });
        let _ = writeln!(md, "### {} (live when {} threshold)\n", s.name, if s.higher_is_live { "≥" } else { "≤" });
        let header: Vec<String> =
            labels.iter().map(|l| format!("{} frames | {} sessions", l.as_str(), l.as_str())).collect();
        let _ = writeln!(md, "| Threshold | {} |", header.join(" | "));
        let _ = writeln!(md, "|---|{}", "---|---|".repeat(labels.len()));
        let mut rows: Vec<(String, f32)> = Vec::new();
        if let Some(t) = s.today {
            rows.push(("today".into(), t));
        }
        if let Some((t, _)) = best {
            rows.push(("best".into(), *t));
        }
        if let Some((t, _)) = strict {
            rows.push(("strict".into(), *t));
        }
        let n = s.thresholds.len();
        for q in [0.05, 0.25, 0.5, 0.75, 0.95] {
            if n > 2 {
                rows.push((format!("q{:.0}", q * 100.0), s.thresholds[((n - 1) as f64 * q).round() as usize].0));
            }
        }
        for (what, t) in rows {
            let Some((_, r)) = s.thresholds.iter().find(|(x, _)| *x == t) else { continue };
            let cells: Vec<String> = labels
                .iter()
                .map(|l| {
                    format!(
                        "{} | {}",
                        percent(r.frames.get(l).copied().unwrap_or([0, 0])),
                        percent(r.sessions.get(l).copied().unwrap_or([0, 0]))
                    )
                })
                .collect();
            let _ = writeln!(md, "| {what} {} | {} |", fmt(t), cells.join(" | "));
        }
        let _ = writeln!(md);
    }
    md
}

fn fmt(v: f32) -> String {
    if v.abs() >= 10.0 || v == 0.0 || v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}
