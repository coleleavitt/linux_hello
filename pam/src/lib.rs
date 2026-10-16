//! `pam_linux_hello.so`: asks the `linux_hello daemon` over its Unix socket whether the
//! person at the camera is the user being authenticated.
//!
//! Module arguments: `socket=PATH` (default `/run/linux_hello.sock`), `timeout=SECONDS` (default 10).
//!
//! Face login is skipped (PAM_IGNORE) for remote sessions: SSH_CONNECTION/SSH_TTY set or PAM_RHOST set.
//!
//! Return codes are chosen for `auth sufficient pam_linux_hello.so` placed before the password
//! modules: a match returns PAM_SUCCESS; a face that is wrong, spoofed or missing returns
//! PAM_AUTH_ERR and prints a short reason (`NOFACE too_dark` → "Face too dark: ..."); anything that means "face login is not available" (daemon down, user not
//! enrolled, lid closed over a built-in camera, bad reply) returns PAM_IGNORE. Either way the stack falls through to the password.
//!
//! Enroll-on-failure (F31, when the daemon runs with `--enroll-on-failure`): a `NOMATCH` reply with
//! a token is remembered in this PAM transaction (`pam_set_data`). If the application then calls
//! `pam_setcred` to establish or reinitialise credentials, which it does only after the whole
//! auth stack succeeded (with the password, since the face failed), the module sends
//! `ENROLL_ON_FAILURE <user> <token>` to the daemon and, when the daemon added the print, tells the
//! user so (PAM_TEXT_INFO "Face print added after password login"). A face match or a later face
//! check in the same transaction forgets the token. sudo calls `pam_setcred` by default
//! (`Defaults pam_setcred`).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::ptr;
use std::time::Duration;

const PAM_SUCCESS: c_int = 0;
const PAM_AUTH_ERR: c_int = 7;
const PAM_IGNORE: c_int = 25;

#[repr(C)]
pub struct PamHandle {
    _opaque: [u8; 0],
}

#[link(name = "pam")]
extern "C" {
    fn pam_get_user(pamh: *mut PamHandle, user: *mut *const c_char, prompt: *const c_char) -> c_int;
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
    // pam_info() is a macro over pam_prompt() in pam_ext.h, so call pam_prompt directly.
    fn pam_prompt(pamh: *mut PamHandle, style: c_int, response: *mut *mut c_char, fmt: *const c_char, ...) -> c_int;
    fn pam_syslog(pamh: *const PamHandle, priority: c_int, fmt: *const c_char, ...);
    fn pam_set_data(
        pamh: *mut PamHandle,
        name: *const c_char,
        data: *mut c_void,
        cleanup: Option<extern "C" fn(*mut PamHandle, *mut c_void, c_int)>,
    ) -> c_int;
    fn pam_get_data(pamh: *const PamHandle, name: *const c_char, data: *mut *const c_void) -> c_int;
}

const LOG_NOTICE: c_int = 5;
const PAM_TEXT_INFO: c_int = 4;
const PAM_RHOST: c_int = 4;
const PAM_ESTABLISH_CRED: c_int = 0x2;
const PAM_REINITIALIZE_CRED: c_int = 0x8;
/// `pam_set_data` name of `"<user> <token>"` after a `NOMATCH` with an enroll-on-failure token.
const FAILURE_DATA: &CStr = c"pam_linux_hello_failure";

#[no_mangle]
pub extern "C" fn pam_sm_authenticate(
    pamh: *mut PamHandle,
    _flags: c_int,
    argc: c_int,
    argv: *const *const c_char,
) -> c_int {
    // Never let a panic unwind into the calling program (sudo, login, ...).
    std::panic::catch_unwind(|| authenticate(pamh, args(argc, argv))).unwrap_or(PAM_IGNORE)
}

/// Called after the auth stack succeeded; reports a password success that followed this
/// transaction's `NOMATCH` (enroll-on-failure). Always succeeds, so it never changes the result.
#[no_mangle]
pub extern "C" fn pam_sm_setcred(pamh: *mut PamHandle, flags: c_int, argc: c_int, argv: *const *const c_char) -> c_int {
    if flags & (PAM_ESTABLISH_CRED | PAM_REINITIALIZE_CRED) != 0 {
        let _ = std::panic::catch_unwind(|| report_password_success(pamh, args(argc, argv)));
    }
    PAM_SUCCESS
}

struct Args {
    socket: String,
    timeout: Duration,
}

fn args(argc: c_int, argv: *const *const c_char) -> Args {
    let mut args = Args { socket: "/run/linux_hello.sock".into(), timeout: Duration::from_secs(10) };
    for i in 0..argc.max(0) as usize {
        // SAFETY: PAM passes `argc` valid NUL-terminated strings.
        let arg = unsafe { CStr::from_ptr(*argv.add(i)) }.to_string_lossy();
        if let Some(path) = arg.strip_prefix("socket=") {
            args.socket = path.to_string();
        } else if let Some(secs) = arg.strip_prefix("timeout=").and_then(|s| s.parse().ok()) {
            args.timeout = Duration::from_secs(secs);
        }
    }
    args
}

fn authenticate(pamh: *mut PamHandle, args: Args) -> c_int {
    // Only the last face check of the transaction may lead to enroll-on-failure.
    remember_failure(pamh, None);

    // The camera sees whoever sits at this machine, not the remote caller: someone logged in
    // over SSH must not be granted access because the owner happens to be at the laptop.
    if is_remote(pamh) {
        return PAM_IGNORE;
    }

    let mut user_ptr: *const c_char = ptr::null();
    // SAFETY: pamh comes from libpam; on success user_ptr points to a string libpam owns.
    if unsafe { pam_get_user(pamh, &mut user_ptr, ptr::null()) } != PAM_SUCCESS || user_ptr.is_null() {
        return PAM_IGNORE;
    }
    let user = unsafe { CStr::from_ptr(user_ptr) }.to_string_lossy().into_owned();

    // Fails fast when the daemon is not running: fall back to the password silently.
    let Ok(stream) = UnixStream::connect(&args.socket) else {
        return PAM_IGNORE;
    };
    message(pamh, c"Looking for your face...");

    let reply = ask(stream, &format!("AUTH {user}"), args.timeout);

    log(pamh, &format!("pam_linux_hello: user={user} reply={reply:?}"));
    let mut words = reply.split_whitespace();
    match words.next() {
        Some("MATCH") => PAM_SUCCESS,
        Some(word @ ("NOMATCH" | "NOTLIVE" | "NOFACE")) => {
            // `NOMATCH <similarity> <token>`: the daemon keeps this face for enroll-on-failure.
            if let ("NOMATCH", Some(token)) = (word, words.nth(1)) {
                remember_failure(pamh, Some(&format!("{user} {token}")));
            }
            message(pamh, failure_message(&reply));
            PAM_AUTH_ERR
        }
        // `UNAVAILABLE lid_closed`: the only reason so far.
        Some("UNAVAILABLE") => {
            message(pamh, c"Face login not available: the lid is closed");
            PAM_IGNORE
        }
        _ => PAM_IGNORE,
    }
}

/// A short hint for a failed face check. The daemon's reply is the verdict word, then for
/// `NOFACE` the most frequent reason a frame was unusable.
fn failure_message(reply: &str) -> &'static CStr {
    let mut words = reply.split_whitespace();
    match (words.next(), words.next()) {
        (Some("NOFACE"), Some("too_dark")) => c"Face too dark: move closer to the camera",
        (Some("NOFACE"), Some("too_bright")) => c"Face too bright: move back from the camera",
        (Some("NOFACE"), Some("too_far")) => c"Face too small: move closer to the camera",
        (Some("NOFACE"), Some("edge")) => c"Face at the edge of the picture: center it in front of the camera",
        (Some("NOFACE"), Some("occluded")) => c"Face covered: remove the mask, sunglasses or hand",
        (Some("NOFACE"), Some("not_frontal")) => c"Face turned away: look at the screen",
        (Some("NOFACE"), _) => c"No face seen by the camera",
        (Some("NOTLIVE"), _) => c"Face not accepted as live",
        _ => c"Face not recognized",
    }
}

/// Send one request line and read the one-line reply; empty on any error.
fn ask(mut stream: UnixStream, request: &str, timeout: Duration) -> String {
    (|| -> std::io::Result<String> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        writeln!(stream, "{request}")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(line.trim_end().to_string())
    })()
    .unwrap_or_default()
}

/// The auth stack succeeded after this transaction's `NOMATCH`, so the password confirmed the
/// user: ask the daemon to add that face (F31). The user is the one saved with the token: sudo
/// sets `PAM_USER` to the target user (root) before `pam_setcred`.
fn report_password_success(pamh: *mut PamHandle, args: Args) {
    let mut data: *const c_void = ptr::null();
    // SAFETY: the data under FAILURE_DATA is a NUL-terminated string set by remember_failure.
    if unsafe { pam_get_data(pamh, FAILURE_DATA.as_ptr(), &mut data) } != PAM_SUCCESS || data.is_null() {
        return;
    }
    let saved = unsafe { CStr::from_ptr(data.cast()) }.to_string_lossy().into_owned();
    remember_failure(pamh, None);
    let Some((user, token)) = saved.split_once(' ') else { return };
    let Ok(stream) = UnixStream::connect(&args.socket) else { return };
    let reply = ask(stream, &format!("ENROLL_ON_FAILURE {user} {token}"), Duration::from_secs(5));
    log(pamh, &format!("pam_linux_hello: enroll-on-failure user={user} reply={reply:?}"));
    if reply.starts_with("ADDED ") {
        message(pamh, c"Face print added after password login");
    }
}

/// Keep `"<user> <token>"` in the PAM handle until `pam_setcred`, or forget it (`None`).
fn remember_failure(pamh: *mut PamHandle, data: Option<&str>) {
    extern "C" fn free(_: *mut PamHandle, data: *mut c_void, _: c_int) {
        if !data.is_null() {
            // SAFETY: `data` came from CString::into_raw in remember_failure.
            drop(unsafe { CString::from_raw(data.cast()) });
        }
    }
    let data = data.and_then(|d| CString::new(d).ok()).map_or(ptr::null_mut(), |d| d.into_raw().cast());
    // SAFETY: libpam owns `data` from here and frees it through `free` when it is replaced or at pam_end.
    unsafe { pam_set_data(pamh, FAILURE_DATA.as_ptr(), data, Some(free)) };
}

fn is_remote(pamh: *mut PamHandle) -> bool {
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return true;
    }
    let mut rhost: *const c_void = ptr::null();
    // SAFETY: PAM_RHOST is a C string item owned by libpam, or NULL when unset.
    let ok = unsafe { pam_get_item(pamh, PAM_RHOST, &mut rhost) } == PAM_SUCCESS;
    ok && !rhost.is_null() && !unsafe { CStr::from_ptr(rhost.cast()) }.to_bytes().is_empty()
}

fn message(pamh: *mut PamHandle, text: &CStr) {
    // SAFETY: "%s" with one NUL-terminated string argument.
    unsafe { pam_prompt(pamh, PAM_TEXT_INFO, ptr::null_mut(), c"%s".as_ptr(), text.as_ptr()) };
}

fn log(pamh: *mut PamHandle, text: &str) {
    if let Ok(text) = std::ffi::CString::new(text) {
        // SAFETY: "%s" with one NUL-terminated string argument.
        unsafe { pam_syslog(pamh, LOG_NOTICE, c"%s".as_ptr(), text.as_ptr()) };
    }
}
