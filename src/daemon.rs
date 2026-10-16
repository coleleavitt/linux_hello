//! Unix-socket service used by the PAM module.
//!
//! Protocol, one line each way: the client sends `AUTH <user>` for one face check, or
//! `AUTH <user> <seconds>` for a presence wait of up to that long (lock screen, console login;
//! [`Authenticator::verify_within`], at most [`MAX_WAIT`]). The daemon answers with a
//! [`Verdict`] (`MATCH 0.850`, `NOMATCH 0.120`, `NOTLIVE`, `NOFACE <reason>`, `NOTENROLLED`,
//! `UNAVAILABLE lid_closed`), `BUSY` (another request is using the cameras) or
//! `ERROR <message>`. The first word decides; `<reason>` is one of `no_face`, `too_dark`,
//! `too_bright`, `clipped`, `too_far`, `edge`, `occluded`, `not_frontal` (see
//! [`crate::FaceProblem`]). The reply to `AUTH` may go on with `NOTICE <text>` lines for the
//! PAM module to show the user (Windows' "improve recognition" notification, F56): after
//! [`FAILURES_PER_NOTICE`] `NOMATCH` in a row for the user, and once after enroll-on-failure
//! added a print; both are also logged at warn.
//!
//! `AUTH <user> [<seconds>] progress`: the client also wants to hear what happens before the
//! verdict, as `NOTICE <event>` lines sent as they happen (`NOTICE recognized` the moment the
//! face is recognised and the liveness votes begin: Windows' "Making sure it's you..."). A
//! client that asks for it must read lines until the verdict word; without the flag the first
//! line is the verdict, as before.
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
use std::collections::HashMap;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Mutex, TryLockError};
use std::time::{Duration, Instant};
use tracing::{info, warn};

use crate::{Authenticator, Embedding, OnFailure, Verdict};

/// How long after the failed face check the password success may add its print.
pub const PENDING_MAX_AGE: Duration = Duration::from_secs(60);
/// `NOMATCH` replies in a row for one user after which the reply tells the user to improve the
/// template (`linux_hello enroll --add`): the face was seen but not recognised that often, so
/// the password was typed instead each time.
pub const FAILURES_PER_NOTICE: u32 = 5;
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
    /// Consecutive `NOMATCH` replies per user
    failures: HashMap<String, u32>,
    /// Notices queued for the next `AUTH` reply per user
    notices: HashMap<String, Vec<String>>,
}

pub fn serve(auth: &mut Authenticator, socket: &Path, enroll_on_failure: bool) -> Result<()> {
    if socket.exists() {
        std::fs::remove_file(socket).with_context(|| format!("removing stale {}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    // Anyone may connect; each request is checked against the peer's uid.
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o666))?;
    info!(socket = %socket.display(), enroll_on_failure, "listening");

    let state = Mutex::new(State {
        auth,
        pending: enroll_on_failure.then_some(None),
        failures: HashMap::new(),
        notices: HashMap::new(),
    });
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
        // The progress lines go out as they happen; a client that did not ask gets none.
        let wants_progress = matches!(request, Request::Auth { progress: true, .. });
        let mut progress = |event: &str| {
            if wants_progress {
                if let Err(e) = writeln!(&stream, "NOTICE {event}") {
                    warn!(error = %e, "could not send progress");
                }
            }
        };
        match state.try_lock() {
            Ok(mut state) => handle(&mut state, &request, &mut progress),
            Err(TryLockError::WouldBlock) => {
                info!(peer, "busy: another request is using the cameras");
                Ok("BUSY".to_string())
            }
            // A request that panicked (printed by the panic hook) closed its cameras on unwind;
            // the models and the store are still usable.
            Err(TryLockError::Poisoned(e)) => {
                warn!("the previous request panicked; carrying on");
                handle(&mut e.into_inner(), &request, &mut progress)
            }
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
        /// `NOTICE <event>` lines before the verdict
        progress: bool,
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
    let mut words: Vec<&str> = line.split_whitespace().collect();
    let progress = words.last() == Some(&"progress");
    if progress {
        words.pop();
    }
    match words[..] {
        ["AUTH", user] | ["AUTH", user, _] if valid_username(user) => {
            let wait = match words.get(2) {
                Some(s) => Some(Duration::from_secs(s.parse::<u64>().context("AUTH wait")?).min(MAX_WAIT)),
                None => None,
            };
            Ok(Request::Auth { user: user.to_string(), wait, progress })
        }
        ["ENROLL_ON_FAILURE", user, token] if valid_username(user) && !progress => {
            Ok(Request::EnrollOnFailure { user: user.to_string(), token: token.to_string() })
        }
        _ => bail!("malformed request"),
    }
}

fn handle(state: &mut State, request: &Request, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let State { auth, pending, failures, notices } = state;
    match request {
        Request::Auth { user, wait, .. } => {
            // Re-read the store for enrollments made meanwhile; `verify` saves it again after a
            // successful unlock adds a print (a sealed store stays sealed).
            auth.pipeline.reload_store()?;
            let deadline = wait.map(|wait| Instant::now() + wait);
            let outcome = auth.verify_reporting(user, deadline, &mut || progress("recognized"))?;
            info!(user, ?wait, verdict = %outcome.verdict, on_failure_candidate = outcome.failed_print.is_some(), "request");
            let mut reply = outcome.verdict.to_string();
            if let Some(pending) = pending {
                *pending = None;
                if let (Verdict::NoMatch { .. }, Some(print)) = (outcome.verdict, outcome.failed_print) {
                    let token = random_token()?;
                    reply = format!("{reply} {token}");
                    *pending = Some(Pending { user: user.clone(), token, print, at: Instant::now() });
                }
            }
            let lines = notice_lines(outcome.verdict, user, failures, notices);
            for line in lines {
                reply = format!("{reply}\nNOTICE {line}");
            }
            Ok(reply)
        }
        Request::EnrollOnFailure { user, token } => {
            let Some(print) = pending.as_mut().and_then(|p| Pending::take(p, user, token, Instant::now())) else {
                info!(user, "enroll-on-failure: no pending print for this token");
                return Ok("REFUSED no_pending".to_string());
            };
            auth.pipeline.reload_store()?;
            let result = auth.pipeline.add_on_failure(user, &print)?;
            if let OnFailure::Added { .. } = result {
                warn!(user, %result, "enroll-on-failure added a print after a password login");
                notices.entry(user.clone()).or_default().push("A face print was added after a password login (enroll-on-failure); run `linux_hello enroll <user>` if that was not you".to_string());
            } else {
                info!(user, %result, "enroll-on-failure");
            }
            Ok(result.to_string())
        }
    }
}

/// What the reply to `user`'s `AUTH` tells the user after the verdict: the notices queued for
/// the user, and at every [`FAILURES_PER_NOTICE`]-th `NOMATCH` in a row the hint to improve
/// the template.
fn notice_lines(
    verdict: Verdict,
    user: &str,
    failures: &mut HashMap<String, u32>,
    queued: &mut HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut lines = queued.remove(user).unwrap_or_default();
    if let Verdict::NoMatch { .. } = verdict {
        let failed = failures.entry(user.to_string()).or_default();
        *failed += 1;
        if failed.is_multiple_of(FAILURES_PER_NOTICE) {
            warn!(
                user,
                failures = *failed,
                "face seen but not recognised again and again; the template may need prints"
            );
            lines.push(format!(
                "Face login has fallen back to the password {failed} times in a row: run `linux_hello enroll --add`"
            ));
        }
    } else {
        failures.remove(user);
    }
    lines
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
        let auth = |user: &str| Request::Auth { user: user.to_string(), wait: None, progress: false };
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
        // `progress` after the user or the wait asks for the interim NOTICE lines.
        let progress = |line| match parse(line) {
            Ok(Request::Auth { wait, progress, .. }) => (wait, progress),
            _ => panic!("{line}"),
        };
        assert_eq!(progress("AUTH cole progress"), (None, true));
        assert_eq!(progress("AUTH cole 5 progress"), (Some(Duration::from_secs(5)), true));
        assert_eq!(progress("AUTH cole 5"), (Some(Duration::from_secs(5)), false));
        assert!(parse("ENROLL_ON_FAILURE cole tok progress").is_err());
    }

    #[test]
    fn notices_come_with_the_fifth_nomatch_in_a_row_and_once_when_queued() {
        let (mut failures, mut queued) = (HashMap::new(), HashMap::new());
        let nomatch = Verdict::NoMatch { similarity: 0.3 };
        let mut lines = |verdict| notice_lines(verdict, "alice", &mut failures, &mut queued);
        for _ in 0..4 {
            assert!(lines(nomatch).is_empty());
        }
        assert_eq!(lines(nomatch).len(), 1);
        for _ in 0..4 {
            assert!(lines(nomatch).is_empty());
        }
        assert_eq!(lines(nomatch).len(), 1, "and again at the 10th");
        // Any other verdict starts the count over; NOFACE does not count.
        assert!(lines(Verdict::NoFace { problem: crate::FaceProblem::NoFace }).is_empty());
        for _ in 0..4 {
            assert!(lines(nomatch).is_empty());
        }
        assert!(lines(Verdict::Match { similarity: 0.9 }).is_empty());
        assert!(lines(nomatch).is_empty());
        // A queued notice goes out with the next reply, whatever the verdict, once.
        queued.entry("alice".to_string()).or_default().push("added".to_string());
        assert_eq!(notice_lines(Verdict::Match { similarity: 0.9 }, "alice", &mut failures, &mut queued), ["added"]);
        assert!(notice_lines(Verdict::Match { similarity: 0.9 }, "alice", &mut failures, &mut queued).is_empty());
    }
}
