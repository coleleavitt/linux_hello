//! Linux Hello CLI

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Instant;
use tracing_subscriber::EnvFilter;

use linux_hello::authenticator::{describe, describe_skipped, ENROLL_PRINTS};
use linux_hello::lighting::subtract_ambient;
use linux_hello::{
    calibration, daemon, sysfs, AntiSpoof, Authenticator, CameraSetup, FacePipeline, LivenessModel, OpenCameras,
    Session, Verdict, WindowsHelloModels,
};

#[derive(Parser)]
#[command(name = "linux_hello", about = "Windows Hello-style face authentication for Linux")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info", global = true)]
    verbose: String,

    /// IR camera used for recognition
    #[arg(short, long, default_value = "/dev/video2", global = true)]
    device: String,

    /// uvcvideo metadata node of the IR camera (per-frame emitter flag)
    #[arg(long, default_value = "/dev/video3", global = true)]
    ir_metadata: String,

    /// Tell lit from ambient IR frames by brightness instead of the metadata node
    #[arg(long, global = true)]
    no_ir_metadata: bool,

    /// RGB camera for the liveness check
    #[arg(long, default_value = "/dev/video0", global = true)]
    liveness_device: String,

    /// Skip the RGB anti-spoofing veto (the IR lighting gate still applies)
    #[arg(long, global = true)]
    no_liveness: bool,

    /// Do not use the converted Windows Hello models (anti-spoofing vote, occlusion gate) even
    /// when `<models>/windows_hello/converted` exists
    #[arg(long, global = true)]
    no_windows_hello: bool,

    /// Face store (default: /var/lib/linux_hello/faces.json as root, else ~/.local/share/linux_hello/faces.json)
    #[arg(long, env = "LINUX_HELLO_STORE", global = true)]
    store: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Check whether the person in front of the camera is USER (default: current user)
    Auth { user: Option<String> },
    /// Enroll a face from the IR camera: 16 prints over a few seconds (10 s at most)
    Enroll {
        user: String,
        /// Prints to collect, each from a frame pair with good IR lighting and a different view
        #[arg(long, default_value_t = ENROLL_PRINTS)]
        frames: usize,
        /// Enroll even if the face matches another enrolled user
        #[arg(long)]
        force: bool,
        /// Add the prints to USER's existing template (e.g. with glasses) instead of replacing it
        #[arg(long)]
        add: bool,
    },
    /// Remove an enrolled face
    Remove { user: String },
    /// List enrolled users
    List,
    /// Encrypt the face store with a key sealed by the TPM (see docs/tpm.md)
    #[cfg(feature = "tpm")]
    Seal {
        /// Also bind the key to these SHA-256 PCRs, e.g. 7 (Secure Boot state)
        #[arg(long, value_delimiter = ',')]
        pcrs: Vec<u8>,
    },
    /// Decrypt a sealed face store back to plain JSON
    #[cfg(feature = "tpm")]
    Unseal,
    /// Capture one IR frame pair (and an RGB frame) to /tmp/linux_hello_{lit,ambient,diff,rgb}.png
    Camera,
    /// Serve authentication requests from the PAM module
    Daemon {
        #[arg(long, default_value = "/run/linux_hello.sock")]
        socket: PathBuf,
        /// After a failed face check that the user then passes with the password, add the face
        /// to the template (opt-in; see docs/sudo.md)
        #[arg(long)]
        enroll_on_failure: bool,
    },
    /// Log every frame pair's scores for SECONDS (for spoof testing); frames are saved to --out
    Watch {
        user: Option<String>,
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        #[arg(long, default_value = "/tmp/linux_hello_watch")]
        out: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(format!("{},ort=warn,tss_esapi=warn", cli.verbose)))
        .init();

    let models = std::env::var("LINUX_HELLO_MODELS")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/models").to_string());
    let store = cli.store.clone().unwrap_or_else(default_store);
    let detector = format!("{models}/det_10g.onnx");
    let embedder = format!("{models}/w600k_r50.onnx");
    // Similarity → FAR table for this IR camera (USB vid:pid), or the default one (F51, F60).
    let calibration = calibration::load(format!("{models}/calibration.json"), sysfs::usb_id(&cli.device).as_deref())?;

    let cameras = CameraSetup {
        device: cli.device.clone(),
        ir_metadata: (!cli.no_ir_metadata).then(|| cli.ir_metadata.clone()),
        liveness_device: (!cli.no_liveness).then(|| cli.liveness_device.clone()),
    };
    // Fails closed: if the liveness models are wanted but do not load, nothing runs.
    let liveness = || -> Result<Vec<AntiSpoof>> {
        if cli.no_liveness {
            return Ok(Vec::new());
        }
        Ok(vec![
            AntiSpoof::load(format!("{models}/antispoof/facenox_antispoof.onnx"), LivenessModel::FaceNox)?,
            AntiSpoof::load(format!("{models}/antispoof/MiniFASNetV2.onnx"), LivenessModel::MiniFasNetV2)?,
        ])
    };
    // The converted Windows Hello models are git-ignored (docs/winhello-models/) and used when
    // present. If they are present but do not load, nothing runs.
    let pipeline = |liveness: Vec<AntiSpoof>| -> Result<FacePipeline> {
        let pipeline = FacePipeline::new(&detector, &embedder, liveness, &store, calibration.clone())?;
        let dir = PathBuf::from(format!("{models}/windows_hello/converted"));
        if cli.no_windows_hello || !dir.exists() {
            return Ok(pipeline);
        }
        Ok(pipeline.with_windows_hello(WindowsHelloModels::load(&dir)?))
    };
    let current_user = || std::env::var("USER").unwrap_or_default();

    match cli.command {
        Commands::Auth { user } => {
            let user = user.unwrap_or_else(current_user);
            let pipeline = pipeline(liveness()?)?;
            let start = Instant::now();
            let verdict = Authenticator { pipeline, cameras }.verify(&user)?.verdict;
            println!("{user}: {verdict} ({:.2} s)", start.elapsed().as_secs_f32());
            if !matches!(verdict, Verdict::Match { .. }) {
                std::process::exit(1);
            }
        }

        Commands::Enroll { user, frames, force, add } => {
            let pipeline = pipeline(Vec::new())?;
            let mut auth = Authenticator { pipeline, cameras };
            println!(
                "Look at the camera and move your head a little, as when talking to someone: \
                 {frames} prints are taken over a few seconds."
            );
            let mut progress = |n: usize, wanted: usize| {
                print!("\r{n}/{wanted}");
                let _ = std::io::stdout().flush();
            };
            let result = auth.enroll(&user, frames, force, add, &mut progress);
            println!();
            let enrolled = result?;
            println!(
                "{} {user} with {} prints ({} in the template, {} wanted) in {:.1} s into {}; skipped frames: {}",
                if add { "Improved" } else { "Enrolled" },
                enrolled.stored,
                enrolled.total,
                enrolled.wanted,
                enrolled.elapsed.as_secs_f32(),
                store.display(),
                describe_skipped(&enrolled.skipped),
            );
        }

        Commands::Remove { user } => {
            let mut pipeline = FacePipeline::new(&detector, &embedder, Vec::new(), &store, calibration)?;
            println!(
                "{}",
                if pipeline.unenroll(&user)? { format!("Removed {user}") } else { format!("{user} is not enrolled") }
            );
        }

        Commands::List => {
            let pipeline = FacePipeline::new(&detector, &embedder, Vec::new(), &store, calibration)?;
            for user in pipeline.list_enrolled() {
                println!("{user}");
            }
        }

        #[cfg(feature = "tpm")]
        Commands::Seal { pcrs } => {
            let mut faces = linux_hello::FaceStore::load(&store)?;
            faces.seal(&pcrs)?;
            faces.save()?;
            println!("Sealed {} ({} faces) with the TPM, PCRs {pcrs:?}", store.display(), faces.len());
        }

        #[cfg(feature = "tpm")]
        Commands::Unseal => {
            let mut faces = linux_hello::FaceStore::load(&store)?;
            faces.unseal();
            faces.save()?;
            println!("Stored {} ({} faces) as plain JSON", store.display(), faces.len());
        }

        Commands::Camera => {
            let capture = OpenCameras::open(&cameras)?.capture()?;
            save_capture(&capture, &PathBuf::from("/tmp"), "linux_hello")?;
            println!("Saved /tmp/linux_hello_{{lit,ambient,diff,rgb}}.png (pair {:?} apart)", capture.pair.gap());
        }

        Commands::Daemon { socket, enroll_on_failure } => {
            let pipeline = pipeline(liveness()?)?;
            daemon::serve(&mut Authenticator { pipeline, cameras }, &socket, enroll_on_failure)?;
        }

        Commands::Watch { user, seconds, out } => {
            let user = user.unwrap_or_else(current_user);
            let mut pipeline = pipeline(liveness()?)?;
            std::fs::create_dir_all(&out)?;
            let mut log = std::fs::File::create(out.join("log.jsonl"))?;
            let mut cams = OpenCameras::open(&cameras)?;
            let mut session = Session::default();
            let start = Instant::now();
            let mut n = 0;
            while start.elapsed().as_secs() < seconds {
                let capture = cams.capture()?;
                let check = pipeline.check(&user, &capture.pair, capture.rgb.as_ref())?;
                cams.expose_on(check.largest().map(|f| &f.analysis.face));
                let verdict = session.add(&check);
                let t = start.elapsed().as_secs_f32();
                println!("{n:4} t={t:5.2}s {}", describe(&check, &session));
                save_capture(&capture, &out, &format!("{n:04}"))?;
                let faces: Vec<_> = check
                    .faces
                    .iter()
                    .zip(session.attempts())
                    .map(|(f, (track, attempt))| {
                        let a = &f.analysis;
                        let l = a.lighting;
                        serde_json::json!({
                            "track": track, "face": a.face.bbox,
                            "lit": l.map(|l| l.lit), "ambient": l.map(|l| l.ambient), "d": l.map(|l| l.gain()),
                            "status": a.status().map(|s| format!("{s:?}")), "problem": a.problem.map(|p| p.as_str()),
                            "rgb_real": f.liveness, "vote": format!("{:?}", f.vote),
                            "windows": f.windows.as_ref().map(|w| serde_json::json!({
                                "total": w.total, "diffusion_ir": w.scores.diffusion_ir, "color": w.scores.color,
                                "border": w.scores.border, "hh": w.scores.hh,
                            })),
                            "similarity": f.best.map(|b| b.similarity), "far": f.best.map(|b| b.far),
                            "recognised": attempt.recognised(), "liveness": format!("{:?}", attempt.liveness()),
                        })
                    })
                    .collect();
                writeln!(
                    log,
                    "{}",
                    serde_json::json!({
                        "n": n, "t": t, "gap_ms": capture.pair.gap().as_secs_f32() * 1000.0,
                        "faces": faces,
                        "verdict": verdict.map(|v| v.to_string()),
                    })
                )?;
                if let Some(verdict) = verdict {
                    println!("     => {verdict}; new session");
                    session = Session::default();
                }
                n += 1;
            }
        }
    }
    Ok(())
}

/// Save the lit, ambient and ambient-subtracted IR frames and the RGB frame as `<prefix>_*.png`.
fn save_capture(capture: &linux_hello::Capture, dir: &std::path::Path, prefix: &str) -> Result<()> {
    let pair = &capture.pair;
    pair.lit.save(dir.join(format!("{prefix}_lit.png")))?;
    pair.ambient.save(dir.join(format!("{prefix}_ambient.png")))?;
    subtract_ambient(&pair.lit, &pair.ambient).save(dir.join(format!("{prefix}_diff.png")))?;
    if let Some(rgb) = &capture.rgb {
        rgb.save(dir.join(format!("{prefix}_rgb.png")))?;
    }
    Ok(())
}

fn default_store() -> PathBuf {
    let is_root = std::fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false);
    if is_root {
        PathBuf::from("/var/lib/linux_hello/faces.json")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/linux_hello/faces.json")
    }
}
