//! `hello-greeter`: under greetd (default) or as the session's lock screen (`--lock`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use hello_greeter::assets::Assets;
use hello_greeter::config::{self, Config};
use hello_greeter::ui::{self, Backend, Options, Surface};
use hello_greeter::users::Account;

/// A greetd greeter and session locker with a Windows Hello-style sign-in screen.
#[derive(Parser, Debug)]
#[command(version)]
struct Cli {
    /// Lock the running session (ext-session-lock-v1) and unlock it through PAM (`hello-lock`)
    /// instead of greeting for greetd
    #[arg(long)]
    lock: bool,
    /// Development: a plain fullscreen window in the current session instead of a layer-shell
    /// surface or a session lock (greetd mode needs GREETD_SOCK, e.g. the fake greetd)
    #[arg(long)]
    window: bool,
    #[arg(long, default_value = config::DEFAULT_PATH)]
    config: PathBuf,
    /// The Windows assets directory (default: the config, $LINUX_HELLO_ASSETS, the installed one)
    #[arg(long)]
    assets: Option<PathBuf>,
    /// The account to show (default: the config, else the first login account; --lock: $USER)
    #[arg(long)]
    user: Option<String>,
    /// The greetd socket (greetd sets it)
    #[arg(long, env = "GREETD_SOCK")]
    greetd_sock: Option<PathBuf>,
    /// Development, with --lock: read the `hello-lock` service file from this directory
    /// (pam_start_confdir) instead of /etc/pam.d
    #[arg(long)]
    pam_dir: Option<PathBuf>,
    /// Log level filter (RUST_LOG syntax)
    #[arg(short, long, default_value = "info")]
    verbosity: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt().with_env_filter(EnvFilter::new(&cli.verbosity)).with_writer(std::io::stderr).init();

    let config = Config::load(&cli.config)?;
    // Fonts must be registered before GTK builds its font map.
    let assets = Assets::load(&config, cli.assets.as_deref());
    tracing::info!("{}", assets.describe());

    let (backend, surface) = if cli.lock {
        (Backend::Pam { confdir: cli.pam_dir }, if cli.window { Surface::Window } else { Surface::SessionLock })
    } else {
        let socket =
            cli.greetd_sock.context("GREETD_SOCK is not set: run under greetd, or --lock, or the fake greetd")?;
        (Backend::Greetd(socket), if cli.window { Surface::Window } else { Surface::LayerShell })
    };
    let user =
        cli.user.or_else(|| config.user.clone()).or_else(|| cli.lock.then(|| std::env::var("USER").ok()).flatten());
    let account = Account::find(user.as_deref())?;
    let session_command = match backend {
        Backend::Greetd(_) => account.session_command(config.command.as_deref())?,
        Backend::Pam { .. } => vec![],
    };
    tracing::info!("user {} ({}); session {:?}; {surface:?}", account.name, account.display_name, session_command);

    let options = Options { surface, backend, config, assets, account, session_command };
    let code = ui::run(options);
    if code == gtk::glib::ExitCode::SUCCESS {
        Ok(())
    } else {
        anyhow::bail!("exit code {code:?}")
    }
}
