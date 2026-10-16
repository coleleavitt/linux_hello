//! `/etc/greetd/hello-greeter.toml` (or `--config`): the account shown, the session command,
//! where the Windows assets are, and the two Windows Hello behaviours that are settings there.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_PATH: &str = "/etc/greetd/hello-greeter.toml";
/// Where contrib/install-greeter.sh puts the Windows assets (docs/greeter.md).
pub const INSTALLED_ASSETS: &str = "/usr/local/share/linux_hello/assets";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The account the sign-in screen shows; default: the first login account in /etc/passwd
    /// (greetd mode) or the current user (lock mode).
    pub user: Option<String>,
    /// The session command (greetd mode); default: `dbus-run-session ~/.local/bin/start-niri.sh`
    /// when that script exists, else the `Exec` of the first `/usr/share/wayland-sessions/*.desktop`.
    pub command: Option<String>,
    /// The Windows assets directory (`assets::find` when unset).
    pub assets: Option<PathBuf>,
    /// The lock-screen background: a path, relative to the assets directory or absolute.
    pub background: PathBuf,
    /// The Hello face glyph in the Segoe Fluent Icons / MDL2 font from the assets, as a hex
    /// code point (e.g. `"EB68"`); empty: the Hello frames from the assets, else the built-in icon.
    pub hello_glyph: String,
    /// Windows' "Automatically dismiss the lock screen if we recognize your face": a recognised
    /// face on the lock screen signs in at once; off, it shows "Welcome" and waits for a key.
    pub dismiss_lock_screen: bool,
    /// How long the lock screen keeps looking for the face after it appears or after the last
    /// key press, in seconds (F56: Windows asks for a confirmation after 120 s).
    pub lock_face_seconds: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            user: None,
            command: None,
            assets: None,
            background: PathBuf::from("screen/img100.jpg"),
            hello_glyph: String::new(),
            dismiss_lock_screen: true,
            lock_face_seconds: 120,
        }
    }
}

impl Config {
    /// The file at `path`; the defaults when it does not exist.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parse {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }
}
