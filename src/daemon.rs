//! Unix-socket service used by the PAM module.
//!
//! Protocol, one line each way: the client sends `AUTH <user>` for one face check, or
//! `AUTH <user> <seconds>` for a presence wait of up to that long (lock screen, console login;
//! [`Authenticator::verify_within`], at most [`MAX_WAIT`]). The daemon answers with a
//! [`Verdict`] (`MATCH 0.850`, `NOMATCH 0.120`, `NOTLIVE`, `NOFACE <reason>`, `NOTENROLLED`,
//! `UNAVAILABLE lid_closed`), `BUSY` (another request is using the cameras) or
//! `ERROR <message>`. The first word decides; `<reason>` is one of `no_face`, `too_dark`,
//! `too_bright`, `clipped`, `too_far`, `edge`, `occluded`, `not_frontal` (see
//! [`crate::FaceProblem`]).
//!
//! Access control: the socket is world-connectable (a screen locker runs PAM as the user), and
//! every request is checked against the peer's uid (`SO_PEERCRED`): root may ask about any user;
//! another uid may only `AUTH` its own user name; `ENROLL_ON_FAILURE` is root only. One request
//! runs at a time; a request arriving meanwhile is answered `BUSY` at once. A user process thus
//! learns whether its own user is in front of the camera and can keep the cameras busy, but gets
//! no credential: the PAM stack, run by the application, is what grants access
//! (docs/pam.md, "Who may ask").
//!
//! Enroll-on-failure (F31, opt-in with `--enroll-on-failure`): when a `NOMATCH` session has a
//! candidate print ([`crate::Session::failed_print`]), the reply carries a third word, a random
//! token: `NOMATCH 0.430 <token>`. The daemon keeps that print for [`PENDING_MAX_AGE`]. After the
//! password succeeded in the same PAM transaction, the PAM module sends
//! `ENROLL_ON_FAILURE <user> <token>` and the daemon answers with a [`crate::OnFailure`]
//! (`ADDED 0.432`, `REFUSED <reason> ...`). Any `AUTH` drops the pending print, so it never
//! outlives the next face check (a match included).

use anyhow::{bail, Context, Result};
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::{Authenticator, Embedding, Verdict};

/// How long after the failed face check the password success may add its print.
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(60);
/// The longest presence wait one `AUTH <user> <seconds>` may ask for, so that a user process
/// cannot hold the cameras for longer per request: the lock screen's own limit on Windows, which
/// asks for a confirmation after 120 s (F56).
pub const MAX_WAIT: Duration = Duration::from_secs(120);

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

/// What one request runs against: the cameras and models, and the enroll-on-failure slot
/// (`None` when enroll-on-failure is off).
struct State<'a> {
    auth: &'a mut Authenticator,
    pending: Option<Option<Pending>>,
}

pub fn serve(auth: &mut Authenticator, socket: &Path, enroll_on_failure: bool) -> Result<()> {
    if socket.exists() {
        std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    // Anyone may connect; each request is checked against the peer's uid.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
    info!(socket = %socket.display(), enroll_on_failure, "listening");

    let state = Mutex::new(State { auth, pending: enroll_on_failure.then_some(None) });
    std::thread::scope(|scope| {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    scope.spawn(|| answer(&state, stream));
                }
                Err(e) => warn!(error = %e, "accept failed"),
            }
        }
    });
    Ok(())
}

/// Read one request, run it unless another one is running, and send the reply.
fn answer(state: &Mutex<State>, mut stream: UnixStream) {
    let reply = request(&mut stream).and_then(|request| {
        let peer = peer_uid(&stream)?;
        request.check_peer(peer)?;
        match state.try_lock() {
            Ok(mut state) => handle(&mut state, &request),
            Err(TryLockError::WouldBlock) => {
                info!(peer, "busy: another request is using the cameras");
                Ok("BUSY".to_string())
            }
            Err(TryLockError::Poisoned(e)) => bail!("daemon state poisoned: {e}"),
        }
    });
    let reply = reply.unwrap_or_else(|e| {
        warn!(error = format!("{e:#}"), "request failed");
        format!("ERROR {}", format!("{e:#}").replace('\n', " "))
    });
    if let Err(e) = writeln!(stream, "{reply}") {
        warn!(error = %e, "could not send reply");
    }
}

enum Request {
    /// `wait`: the presence wait, `None` for one session
    Auth {
        user: String,
        wait: Option<Duration>,
    },
    EnrollOnFailure {
        user: String,
        token: String,
    },
}

impl Request {
    /// Whether the process with uid `peer` may make this request.
    fn check_peer(&self, peer: u32) -> Result<()> {
        if peer == 0 {
            return Ok(());
        }
        match self {
            Request::Auth { user, .. } if uid_of(user) == Some(peer) => Ok(()),
            Request::Auth { user, .. } => bail!("AUTH {user} from uid {peer}: a user may only ask about itself"),
            Request::EnrollOnFailure { .. } => bail!("ENROLL_ON_FAILURE from uid {peer}: only root may add prints"),
        }
    }
}

/// The request line of the connection.
fn request(stream: &mut UnixStream) -> Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(stream.take(256)).read_line(&mut line)?;
    let mut words = line.split_whitespace();
    match (words.next(), words.next(), words.next(), words.next()) {
        (Some("AUTH"), Some(user), seconds, None) if valid_username(user) => {
            let wait = match seconds {
                Some(s) => Some(Duration::from_secs(s.parse::<u64>().context("AUTH wait")?).min(MAX_WAIT)),
                None => None,
            };
            Ok(Request::Auth { user: user.to_string(), wait })
        }
        (Some("ENROLL_ON_FAILURE"), Some(user), Some(token), None) if valid_username(user) => {
            Ok(Request::EnrollOnFailure { user: user.to_string(), token: token.to_string() })
        }
        _ => bail!("malformed request"),
    }
}

fn handle(state: &mut State, request: &Request) -> Result<String> {
    let State { auth, pending } = state;
    match request {
        Request::Auth { user, wait } => {
            // Re-read the store for enrollments made meanwhile; `verify` saves it again after a
            // successful unlock adds a print (a sealed store stays sealed).
            auth.pipeline.reload_store()?;
            let verify = |auth: &mut Authenticator| match wait {
                Some(wait) => auth.verify_within(user, Instant::now() + *wait),
                None => auth.verify(user),
            };
            let Some(pending) = pending else {
                let verdict = verify(auth)?.verdict;
                info!(user, ?wait, %verdict, "request");
                return Ok(verdict.to_string());
            };
            *pending = None;
            let outcome = verify(auth)?;
            info!(user, ?wait, verdict = %outcome.verdict, on_failure_candidate = outcome.failed_print.is_some(), "request");
            let (Verdict::NoMatch { .. }, Some(print)) = (outcome.verdict, outcome.failed_print) else {
                return Ok(outcome.verdict.to_string());
            };
            let token = random_token()?;
            let reply = format!("{} {token}", outcome.verdict);
            *pending = Some(Pending { user: user.clone(), token, print, at: Instant::now() });
            Ok(reply)
        }
        Request::EnrollOnFailure { user, token } => {
            let Some(print) = pending.as_mut().and_then(|p| Pending::take(p, user, token, Instant::now())) else {
                info!(user, "enroll-on-failure: no pending print for this token");
                return Ok("REFUSED no_pending".to_string());
            };
            auth.pipeline.reload_store()?;
            let result = auth.pipeline.add_on_failure(user, &print)?;
            info!(user, %result, "enroll-on-failure");
            Ok(result.to_string())
        }
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

/// The uid of the account `user` (`getpwnam_r`), `None` for an unknown name.
fn uid_of(user: &str) -> Option<u32> {
    let name = CString::new(user).ok()?;
    // SAFETY: `passwd` is a plain C struct; all-zero is a valid value.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = [0u8; 16384];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: the name is NUL-terminated and `buf` holds the strings `pwd` points into for the
    // duration of the call; `result` is set to `&pwd` or NULL.
    let rc = unsafe { libc::getpwnam_r(name.as_ptr(), &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut result) };
    (rc == 0 && !result.is_null()).then_some(pwd.pw_uid)
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
            Some(Pending {
                user: "alice".into(),
                token: "t1".into(),
                print: Embedding { features: vec![1.0], windows: None },
                at,
            })
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

    #[test]
    fn a_user_may_only_ask_about_itself_and_root_about_anyone() {
        assert_eq!(uid_of("root"), Some(0));
        assert_eq!(uid_of("no-such-user-linux-hello"), None);
        let auth = |user: &str| Request::Auth { user: user.to_string(), wait: None };
        let enroll = Request::EnrollOnFailure { user: "root".into(), token: "t".into() };
        assert!(auth("root").check_peer(0).is_ok());
        assert!(auth("nobody").check_peer(0).is_ok());
        assert!(enroll.check_peer(0).is_ok());
        assert!(auth("root").check_peer(1000).is_err());
        assert!(auth("no-such-user-linux-hello").check_peer(1000).is_err());
        assert!(enroll.check_peer(1000).is_err());
        // The uid of a real account may ask about that account.
        let me = unsafe { libc::geteuid() };
        if let Ok(user) = std::env::var("USER") {
            if uid_of(&user) == Some(me) {
                assert!(auth(&user).check_peer(me).is_ok());
            }
        }
    }
    #[test]
    fn auth_requests_carry_an_optional_bounded_wait() {
        let parse = |line: &str| {
            let (mut a, mut b) = UnixStream::pair().unwrap();
            writeln!(b, "{line}").unwrap();
            request(&mut a)
        };
        let wait = |line| match parse(line) {
            Ok(Request::Auth { wait, .. }) => wait,
            _ => panic!("{line}"),
        };
        assert_eq!(wait("AUTH cole"), None);
        assert_eq!(wait("AUTH cole 5"), Some(Duration::from_secs(5)));
        assert_eq!(wait("AUTH cole 100000"), Some(MAX_WAIT));
        assert!(parse("AUTH cole soon").is_err());
        assert!(parse("AUTH cole 5 more").is_err());
        assert!(matches!(parse("ENROLL_ON_FAILURE cole tok"), Ok(Request::EnrollOnFailure { .. })));
    }
}
