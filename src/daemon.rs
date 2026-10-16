//! Unix-socket service used by the PAM module.
//!
//! Protocol, one line each way: the client sends `AUTH <user>`, the daemon answers with a
//! [`Verdict`] (`MATCH 0.850`, `NOMATCH 0.120`, `NOTLIVE`, `NOFACE <reason>`, `NOTENROLLED`) or
//! `ERROR <message>`. The first word decides; `<reason>` is one of `no_face`, `too_dark`,
//! `too_bright`, `too_far`, `edge`, `occluded`, `not_frontal` (see [`crate::FaceProblem`]).

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;
use tracing::{info, warn};

use crate::Authenticator;

pub fn serve(auth: &mut Authenticator, socket: &Path) -> Result<()> {
    if socket.exists() {
        std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    // Only the owner (root in a real install) may ask for authentications.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    info!(socket = %socket.display(), "listening");

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "accept failed");
                continue;
            }
        };
        let reply = match handle(auth, &mut stream) {
            Ok(verdict) => verdict.to_string(),
            Err(e) => {
                warn!(error = format!("{e:#}"), "request failed");
                format!("ERROR {}", format!("{e:#}").replace('\n', " "))
            }
        };
        if let Err(e) = writeln!(stream, "{reply}") {
            warn!(error = %e, "could not send reply");
        }
    }
    Ok(())
}

fn handle(auth: &mut Authenticator, stream: &mut UnixStream) -> Result<crate::Verdict> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(stream.take(256)).read_line(&mut line)?;

    let Some(user) = line.trim_end().strip_prefix("AUTH ") else {
        bail!("malformed request");
    };
    if !valid_username(user) {
        bail!("invalid user name");
    }

    // Re-read the store for enrollments made meanwhile; `verify` saves it again after a
    // successful unlock adds a print (a sealed store stays sealed).
    auth.pipeline.reload_store()?;
    let verdict = auth.verify(user)?;
    info!(user, %verdict, "request");
    Ok(verdict)
}

fn valid_username(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
