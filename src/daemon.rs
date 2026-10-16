//! Unix-socket service used by the PAM module.
//!
//! Protocol, one line each way: the client sends `AUTH <user>`, the daemon answers with a
//! [`Verdict`] (`MATCH 0.850`, `NOMATCH 0.120`, `NOTLIVE`, `NOFACE <reason>`, `NOTENROLLED`,
//! `UNAVAILABLE lid_closed`) or `ERROR <message>`. The first word decides; `<reason>` is one of
//! `no_face`, `too_dark`, `too_bright`, `too_far`, `edge`, `occluded`, `not_frontal` (see
//! [`crate::FaceProblem`]).
//!
//! Enroll-on-failure (F31, opt-in with `--enroll-on-failure`): when a `NOMATCH` session has a
//! candidate print ([`crate::Session::failed_print`]), the reply carries a third word, a random
//! token: `NOMATCH 0.430 <token>`. The daemon keeps that print for [`PENDING_MAX_AGE`]. After the
//! password succeeded in the same PAM transaction, the PAM module sends
//! `ENROLL_ON_FAILURE <user> <token>` and the daemon answers with a [`crate::OnFailure`]
//! (`ADDED 0.432`, `REFUSED <reason> ...`). Only root may send it (`SO_PEERCRED`). Any `AUTH`
//! drops the pending print, so it never outlives the next face check (a match included).

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::{Authenticator, Embedding, Verdict};

/// How long after the failed face check the password success may add its print.
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(60);

/// The print of the last `NOMATCH`, waiting for the PAM module to report a password success.
struct Pending {
    user: String,
    token: String,
    print: Embedding,
    at: Instant,
}

impl Pending {
    /// Take `slot`'s print if it belongs to `user` and `token` and is recent. The slot is emptied
    /// either way: a print is offered once.
    fn take(slot: &mut Option<Pending>, user: &str, token: &str, now: Instant) -> Option<Embedding> {
        let pending = slot.take()?;
        let fresh = now.saturating_duration_since(pending.at) <= PENDING_MAX_AGE;
        (pending.user == user && pending.token == token && fresh).then_some(pending.print)
    }
}

pub fn serve(auth: &mut Authenticator, socket: &Path, enroll_on_failure: bool) -> Result<()> {
    if socket.exists() {
        std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    // Only the owner (root in a real install) may ask for authentications.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    info!(socket = %socket.display(), enroll_on_failure, "listening");

    let mut pending = None;
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "accept failed");
                continue;
            }
        };
        let reply = match handle(auth, &mut stream, enroll_on_failure.then_some(&mut pending)) {
            Ok(reply) => reply,
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

/// `pending` is `None` when enroll-on-failure is off.
fn handle(auth: &mut Authenticator, stream: &mut UnixStream, pending: Option<&mut Option<Pending>>) -> Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(stream.take(256)).read_line(&mut line)?;
    let mut words = line.split_whitespace();

    match (words.next(), words.next(), words.next(), words.next()) {
        (Some("AUTH"), Some(user), None, None) if valid_username(user) => {
            // Re-read the store for enrollments made meanwhile; `verify` saves it again after a
            // successful unlock adds a print (a sealed store stays sealed).
            auth.pipeline.reload_store()?;
            let Some(pending) = pending else {
                let verdict = auth.verify(user)?.verdict;
                info!(user, %verdict, "request");
                return Ok(verdict.to_string());
            };
            *pending = None;
            let outcome = auth.verify(user)?;
            info!(user, verdict = %outcome.verdict, on_failure_candidate = outcome.failed_print.is_some(), "request");
            let (Verdict::NoMatch { .. }, Some(print)) = (outcome.verdict, outcome.failed_print) else {
                return Ok(outcome.verdict.to_string());
            };
            let token = random_token()?;
            let reply = format!("{} {token}", outcome.verdict);
            *pending = Some(Pending { user: user.to_string(), token, print, at: Instant::now() });
            Ok(reply)
        }
        (Some("ENROLL_ON_FAILURE"), Some(user), Some(token), None) if valid_username(user) => {
            let uid = peer_uid(stream)?;
            if uid != 0 {
                bail!("ENROLL_ON_FAILURE from uid {uid}: only root may add prints");
            }
            let Some(print) = pending.and_then(|p| Pending::take(p, user, token, Instant::now())) else {
                info!(user, "enroll-on-failure: no pending print for this token");
                return Ok("REFUSED no_pending".to_string());
            };
            auth.pipeline.reload_store()?;
            let result = auth.pipeline.add_on_failure(user, &print)?;
            info!(user, %result, "enroll-on-failure");
            Ok(result.to_string())
        }
        _ => bail!("malformed request"),
    }
}

fn valid_username(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// The uid of the process on the other end of the socket.
fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills a `ucred` of `len` bytes on a valid Unix socket.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        bail!("SO_PEERCRED: {}", std::io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// 128 random bits as hex.
fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_print_is_taken_once_by_its_user_and_token() {
        let now = Instant::now();
        let pending = |at| {
            Some(Pending { user: "alice".into(), token: "t1".into(), print: Embedding { features: vec![1.0] }, at })
        };
        let mut slot = pending(now);
        assert!(Pending::take(&mut slot, "alice", "t1", now + Duration::from_secs(5)).is_some());
        assert!(Pending::take(&mut slot, "alice", "t1", now + Duration::from_secs(5)).is_none());
        for (user, token, age) in [("bob", "t1", 5), ("alice", "t2", 5), ("alice", "t1", 61)] {
            let mut slot = pending(now);
            assert!(Pending::take(&mut slot, user, token, now + Duration::from_secs(age)).is_none());
            assert!(slot.is_none(), "a wrong request still drops the print");
        }
    }

    #[test]
    fn peer_uid_is_ours() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&a).unwrap(), unsafe { libc::geteuid() });
    }
}
