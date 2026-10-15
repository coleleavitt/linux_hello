//! Linux Hello CLI

use anyhow::Result;
use clap::{Parser, Subcommand};
use image::{ImageReader, RgbImage};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Instant;
use tracing_subscriber::EnvFilter;

use linux_hello::{
    daemon, AntiSpoof, Authenticator, CameraSetup, CameraType, FacePipeline, LivenessModel, OpenCameras, Verdict,
};

#[derive(Parser)]
#[command(name = "linux_hello", about = "Windows Hello-style face authentication for Linux")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, default_value = "info", global = true)]
    verbose: String,

    /// Use the RGB camera for recognition instead of the IR camera
    #[arg(long, global = true)]
    rgb: bool,

    /// Recognition camera (default: /dev/video2 for IR, /dev/video0 for RGB)
    #[arg(short, long, global = true)]
    device: Option<String>,

    /// RGB camera for the liveness check when recognizing with IR
    #[arg(long, default_value = "/dev/video0", global = true)]
    liveness_device: String,

    /// Skip the anti-spoofing check
    #[arg(long, global = true)]
    no_liveness: bool,

    /// Face store (default: /var/lib/linux_hello/faces.json as root, else ~/.local/share/linux_hello/faces.json)
    #[arg(long, env = "LINUX_HELLO_STORE", global = true)]
    store: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Check whether the person in front of the camera is USER (default: current user)
    Auth {
        user: Option<String>,
        /// Frames to try before giving up
        #[arg(long, default_value_t = 10)]
        attempts: usize,
    },
    /// Enroll a face from the camera or an image file
    Enroll {
        user: String,
        /// Image file instead of the camera
        #[arg(short, long)]
        image: Option<String>,
        /// Camera frames to average
        #[arg(long, default_value_t = 5)]
        frames: usize,
    },
    /// Remove an enrolled face
    Remove { user: String },
    /// List enrolled users
    List,
    /// Capture one frame to /tmp/linux_hello_test.png
    Camera,
    /// Serve authentication requests from the PAM module
    Daemon {
        #[arg(long, default_value = "/run/linux_hello.sock")]
        socket: PathBuf,
        /// Frames to try per request
        #[arg(long, default_value_t = 10)]
        attempts: usize,
    },
    /// Log every frame's scores for SECONDS (for spoof testing); frames are saved to --out
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
        .with_env_filter(EnvFilter::new(format!("{},ort=warn", cli.verbose)))
        .init();

    let models = std::env::var("LINUX_HELLO_MODELS")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/models").to_string());
    let store = cli.store.clone().unwrap_or_else(default_store);
    let detector = format!("{models}/det_10g.onnx");
    let embedder = format!("{models}/w600k_r50.onnx");

    let kind = if cli.rgb { CameraType::Rgb } else { CameraType::Ir };
    let cameras = CameraSetup {
        device: cli.device.clone().unwrap_or_else(|| if cli.rgb { "/dev/video0" } else { "/dev/video2" }.to_string()),
        kind,
        liveness_device: (kind == CameraType::Ir && !cli.no_liveness).then(|| cli.liveness_device.clone()),
    };
    let liveness = || -> Result<Vec<AntiSpoof>> {
        if cli.no_liveness {
            return Ok(Vec::new());
        }
        Ok(vec![
            AntiSpoof::load(format!("{models}/antispoof/facenox_antispoof.onnx"), LivenessModel::FaceNox)?,
            AntiSpoof::load(format!("{models}/antispoof/MiniFASNetV2.onnx"), LivenessModel::MiniFasNetV2)?,
        ])
    };
    let current_user = || std::env::var("USER").unwrap_or_default();

    match cli.command {
        Commands::Auth { user, attempts } => {
            let user = user.unwrap_or_else(current_user);
            let pipeline = FacePipeline::new(&detector, &embedder, liveness()?, &store)?;
            let verdict = Authenticator { pipeline, cameras }.verify(&user, attempts)?;
            println!("{user}: {verdict}");
            if !matches!(verdict, Verdict::Match { .. }) {
                std::process::exit(1);
            }
        }

        Commands::Enroll { user, image, frames } => {
            let mut pipeline = FacePipeline::new(&detector, &embedder, Vec::new(), &store)?;
            let images: Vec<RgbImage> = match image {
                Some(path) => vec![ImageReader::open(&path)?.decode()?.to_rgb8()],
                None => {
                    let mut cam = linux_hello::Camera::open(&cameras.device, kind)?;
                    (0..frames).map(|_| cam.capture()).collect::<Result<_>>()?
                }
            };
            let used = pipeline.enroll(&user, &images)?;
            println!("Enrolled {user} from {used} of {} frames into {}", images.len(), store.display());
        }

        Commands::Remove { user } => {
            let mut pipeline = FacePipeline::new(&detector, &embedder, Vec::new(), &store)?;
            println!("{}", if pipeline.unenroll(&user)? { format!("Removed {user}") } else { format!("{user} is not enrolled") });
        }

        Commands::List => {
            let pipeline = FacePipeline::new(&detector, &embedder, Vec::new(), &store)?;
            for user in pipeline.list_enrolled() {
                println!("{user}");
            }
        }

        Commands::Camera => {
            let frame = linux_hello::Camera::open(&cameras.device, kind)?.capture()?;
            frame.save("/tmp/linux_hello_test.png")?;
            println!("Saved {}x{} frame from {} to /tmp/linux_hello_test.png", frame.width(), frame.height(), cameras.device);
        }

        Commands::Daemon { socket, attempts } => {
            let pipeline = FacePipeline::new(&detector, &embedder, liveness()?, &store)?;
            daemon::serve(&mut Authenticator { pipeline, cameras }, &socket, attempts)?;
        }

        Commands::Watch { user, seconds, out } => {
            let user = user.unwrap_or_else(current_user);
            let mut pipeline = FacePipeline::new(&detector, &embedder, liveness()?, &store)?;
            std::fs::create_dir_all(&out)?;
            let mut log = std::fs::File::create(out.join("log.jsonl"))?;
            let mut cams = OpenCameras::open(&cameras)?;
            let start = Instant::now();
            let mut n = 0;
            while start.elapsed().as_secs() < seconds {
                let (frame, live) = cams.capture()?;
                let check = pipeline.check(&user, &frame, live.as_ref())?;
                let verdict = pipeline.verdict(&check);
                let t = start.elapsed().as_secs_f32();
                println!(
                    "{n:4} t={t:5.1}s face={} liveness={} similarity={} => {verdict}",
                    if check.face.is_some() { "yes" } else { "no " },
                    check.liveness.map_or("  -  ".into(), |v| format!("{v:.3}")),
                    check.similarity.map_or("  -  ".into(), |v| format!("{v:.3}")),
                );
                frame.save(out.join(format!("{n:04}_main.png")))?;
                if let Some(live) = &live {
                    live.save(out.join(format!("{n:04}_rgb.png")))?;
                }
                writeln!(log, "{}", serde_json::json!({
                    "n": n, "t": t, "face": check.face.as_ref().map(|f| f.bbox),
                    "liveness": check.liveness, "similarity": check.similarity, "verdict": verdict.to_string(),
                }))?;
                n += 1;
            }
        }
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
