//! `pam_linux_hello.so`: asks the `linux_hello daemon` over its Unix socket whether the
//! person at the camera is the user being authenticated.
//!
//! Module arguments: `socket=PATH` (default `/run/linux_hello.sock`), `timeout=SECONDS` (default
//! 10), `wait_services=NAME,NAME,...` (default `swaylock,login`), `password_prompt` (below).
//!
//! Face login is skipped (PAM_IGNORE) for remote sessions: SSH_CONNECTION/SSH_TTY set or PAM_RHOST set.
//!
//! The behaviour depends on the PAM service (`PAM_SERVICE`, the name the application passed to
//! `pam_start`). For a service in `wait_services`, a lock screen or a console login where the
//! user has just asked to be let in, the module asks the daemon for a presence wait
//! (`AUTH <user> <timeout>`): the daemon looks for the face for up to `timeout` seconds, keeping
//! the cameras open between sessions. For every other service (`sudo`, `su`, `polkit-1`, ...)
//! the daemon runs one session (`AUTH <user>`), 2–4.5 s. Windows keeps tracking until the UI
//! stops it (F55, F57); the 120 s after which its lock screen asks for a confirmation (F56) has
//! no counterpart here because the lock screen only runs PAM once the user submits.
//!
//! The daemon's reply may go on with `NOTICE <text>` lines (an enroll-on-failure addition since
//! the last check, five `NOMATCH` in a row); each is shown as PAM_TEXT_INFO after the verdict's
//! message. The request carries `progress`, so `NOTICE <event>` lines also come before the
//! verdict as things happen; `NOTICE recognized` is shown at once as "Making sure it's you..."
//! (Windows' text while the liveness votes are counted, docs/windows-hello-ui.md).
//!
//! Return codes are chosen for `auth sufficient pam_linux_hello.so` placed before the password
//! modules: a match returns PAM_SUCCESS; a face that is wrong, spoofed or missing returns
//! PAM_AUTH_ERR and prints a short reason (`NOFACE too_dark` → "Face too dark: ..."); anything
//! that means "face login is not available" (daemon down or busy, user not enrolled, lid closed
//! over a built-in camera, bad reply) returns PAM_IGNORE. Either way the stack falls through to
//! the password.
//!
//! Enroll-on-failure (F31, when the daemon runs with `--enroll-on-failure`): a `NOMATCH` reply with
//! a token is remembered in this PAM transaction (`pam_set_data`). If the application then calls
//! `pam_setcred` to establish or reinitialise credentials, which it does only after the whole
//! auth stack succeeded (with the password, since the face failed), the module sends
//! `ENROLL_ON_FAILURE <user> <token>` to the daemon and, when the daemon added the print, tells the
//! user so (PAM_TEXT_INFO "Face print added after password login"). A face match or a later face
//! check in the same transaction forgets the token. sudo calls `pam_setcred` by default
//! (`Defaults pam_setcred`).
//!
//! `password_prompt` is for a greeter or lock screen that runs face recognition over and over
//! (hello-greeter, docs/greeter.md), on a line `[success=done conv_err=die default=ignore]`:
//! after anything but a match the module asks for the password itself (`PAM_PROMPT_ECHO_OFF`
//! "Password") and hands the answer to the password modules through `PAM_AUTHTOK`
//! (`pam_unix try_first_pass`), so that they never prompt. An empty answer, or a conversation
//! that fails (the greeter cancelled), returns PAM_CONV_ERR and the line's `conv_err=die` ends
//! the stack there: the greeter can start the next face check without `pam_unix` and
//! `pam_faillock` ever seeing a failed attempt (every cancelled `pam_unix` prompt would count
//! towards faillock's lockout). "Looking for your face..." failing to reach the greeter ends
//! the stack the same way, before the camera is used.

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
    fn pam_set_item(pamh: *mut PamHandle, item_type: c_int, item: *const c_void) -> c_int;
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
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_TEXT_INFO: c_int = 4;
const PAM_SERVICE: c_int = 1;
const PAM_RHOST: c_int = 4;
const PAM_AUTHTOK: c_int = 6;
/// The prompt of `password_prompt`; a greeter tells it from `pam_unix`'s "Password: " by it.
pub const PASSWORD_PROMPT: &CStr = c"Password";
/// How much longer than the presence wait the module waits for the reply: the session running
/// at the deadline finishes first (up to 16 pairs, ≈ 2–4.5 s), and a single session (no wait)
/// takes as long.
const REPLY_MARGIN: Duration = Duration::from_secs(10);
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
    /// The presence wait of the services in `wait_services`
    timeout: u64,
    wait_services: Vec<String>,
    /// Ask for the password after a failed face check (greeter lines, module docs).
    password_prompt: bool,
}

fn args(argc: c_int, argv: *const *const c_char) -> Args {
    let mut args = Args {
        socket: "/run/linux_hello.sock".into(),
        timeout: 10,
        wait_services: vec!["swaylock".into(), "login".into()],
        password_prompt: false,
    };
    for i in 0..argc.max(0) as usize {
        // SAFETY: PAM passes `argc` valid NUL-terminated strings.
        let arg = unsafe { CStr::from_ptr(*argv.add(i)) }.to_string_lossy();
        if let Some(path) = arg.strip_prefix("socket=") {
            args.socket = path.to_string();
        } else if let Some(secs) = arg.strip_prefix("timeout=").and_then(|s| s.parse().ok()) {
            args.timeout = secs;
        } else if let Some(names) = arg.strip_prefix("wait_services=") {
            args.wait_services = names.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect();
        } else if arg == "password_prompt" {
            args.password_prompt = true;
        }
    }
    args
}

fn authenticate(pamh: *mut PamHandle, args: Args) -> c_int {
    let result = check_face(pamh, &args);
    if args.password_prompt && result != PAM_SUCCESS && result != PAM_CONV_ERR {
        return ask_password(pamh, result);
    }
    result
}

/// `password_prompt`: ask for the password and leave it in `PAM_AUTHTOK` for the password
/// modules; `result` (the face check's) is returned so that the line's control ignores it.
/// No answer: PAM_CONV_ERR, which ends the stack through `conv_err=die`.
fn ask_password(pamh: *mut PamHandle, result: c_int) -> c_int {
    let mut response: *mut c_char = ptr::null_mut();
    // SAFETY: "%s" with one NUL-terminated string argument; libpam mallocs the response.
    let status =
        unsafe { pam_prompt(pamh, PAM_PROMPT_ECHO_OFF, &mut response, c"%s".as_ptr(), PASSWORD_PROMPT.as_ptr()) };
    if status != PAM_SUCCESS || response.is_null() {
        return PAM_CONV_ERR;
    }
    // SAFETY: `response` is a malloced NUL-terminated string that is ours to free.
    unsafe {
        let len = libc::strlen(response);
        let outcome = if len == 0 {
            PAM_CONV_ERR
        } else {
            // pam_set_item copies the token.
            pam_set_item(pamh, PAM_AUTHTOK, response.cast());
            result
        };
        ptr::write_bytes(response, 0, len);
        libc::free(response.cast());
        outcome
    }
}

fn check_face(pamh: *mut PamHandle, args: &Args) -> c_int {
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
    if message(pamh, c"Looking for your face...") != PAM_SUCCESS && args.password_prompt {
        return PAM_CONV_ERR;
    }

    let service = item(pamh, PAM_SERVICE).unwrap_or_default();
    let wait = args.wait_services.contains(&service);
    let request = if wait { format!("AUTH {user} {} progress", args.timeout) } else { format!("AUTH {user} progress") };
    let (reply, lines) =
        ask(pamh, stream, &request, Duration::from_secs(if wait { args.timeout } else { 0 }) + REPLY_MARGIN);

    log(pamh, &format!("pam_linux_hello: service={service} user={user} request={request:?} reply={reply:?}"));
    let mut words = reply.split_whitespace();
    let result = match words.next() {
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
        Some("BUSY") => {
            message(pamh, c"Face login not available: another program is using the camera");
            PAM_IGNORE
        }
        // `ERROR ...` (a daemon that does not understand this module, a camera failure) or an
        // empty reply (daemon gone): face login is not available; the password stack follows.
        _ => {
            log(pamh, "pam_linux_hello: face login not available (daemon reply above)");
            PAM_IGNORE
        }
    };
    // `NOTICE <text>` lines after the verdict: things the daemon wants the user to know.
    for notice in lines.iter().filter_map(|l| l.strip_prefix("NOTICE ")) {
        if let Ok(text) = CString::new(notice) {
            message(pamh, &text);
        }
    }
    result
}

/// Windows' text for the daemon's progress events (`AUTH ... progress`), shown as they arrive:
/// the face is recognised and the liveness votes are being counted (docs/windows-hello-ui.md
/// state 3, resource 123).
fn progress_message(event: &str) -> Option<&'static CStr> {
    match event {
        "recognized" => Some(c"Making sure it's you..."),
        _ => None,
    }
}

/// A short hint for a failed face check. The daemon's reply is the verdict word, then for
/// `NOFACE` the most frequent reason a frame was unusable.
fn failure_message(reply: &str) -> &'static CStr {
    let mut words = reply.split_whitespace();
    match (words.next(), words.next()) {
        (Some("NOFACE"), Some("too_dark")) => c"Face too dark: move closer to the camera",
        (Some("NOFACE"), Some("too_bright")) => c"Face too bright: move back from the camera",
        (Some("NOFACE"), Some("clipped")) => c"Face too bright: move back or turn away from the lamp",
        (Some("NOFACE"), Some("too_far")) => c"Face too small: move closer to the camera",
        (Some("NOFACE"), Some("edge")) => c"Face at the edge of the picture: center it in front of the camera",
        (Some("NOFACE"), Some("occluded")) => c"Face covered: remove the mask, sunglasses or hand",
        (Some("NOFACE"), Some("not_frontal")) => c"Face turned away: look at the screen",
        (Some("NOFACE"), _) => c"No face seen by the camera",
        (Some("NOTLIVE"), _) => c"Face not accepted as live",
        // Windows' resource 125 with "PIN" → "password" (docs/windows-hello-ui.md §2.1).
        _ => c"Couldn't recognize you. Please sign in with your password.",
    }
}

/// Send one request line and read the reply until the daemon closes the connection: the
/// verdict line (empty on any error) and the lines after it. `NOTICE <event>` lines before the
/// verdict are the daemon's progress and are shown at once.
fn ask(pamh: *mut PamHandle, mut stream: UnixStream, request: &str, timeout: Duration) -> (String, Vec<String>) {
    let mut verdict = String::new();
    let mut after = Vec::new();
    let _ = (|| -> std::io::Result<()> {
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        writeln!(stream, "{request}")?;
        for line in BufReader::new(stream).lines() {
            let line = line?;
            if !verdict.is_empty() {
                after.push(line);
            } else if let Some(text) = line.strip_prefix("NOTICE ").and_then(progress_message) {
                message(pamh, text);
            } else if !line.starts_with("NOTICE ") {
                verdict = line;
            }
        }
        Ok(())
    })();
    (verdict, after)
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
    let (reply, _) = ask(pamh, stream, &format!("ENROLL_ON_FAILURE {user} {token}"), Duration::from_secs(5));
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
    item(pamh, PAM_RHOST).is_some_and(|rhost| !rhost.is_empty())
}

/// A string item of the PAM handle (`PAM_SERVICE`, `PAM_RHOST`), `None` when unset.
fn item(pamh: *mut PamHandle, item_type: c_int) -> Option<String> {
    let mut value: *const c_void = ptr::null();
    // SAFETY: string items are C strings owned by libpam, or NULL when unset.
    let ok = unsafe { pam_get_item(pamh, item_type, &mut value) } == PAM_SUCCESS;
    (ok && !value.is_null()).then(|| unsafe { CStr::from_ptr(value.cast()) }.to_string_lossy().into_owned())
}

/// Show `text`; the status says whether the application's conversation took it.
fn message(pamh: *mut PamHandle, text: &CStr) -> c_int {
    // SAFETY: "%s" with one NUL-terminated string argument.
    unsafe { pam_prompt(pamh, PAM_TEXT_INFO, ptr::null_mut(), c"%s".as_ptr(), text.as_ptr()) }
}

fn log(pamh: *mut PamHandle, text: &str) {
    if let Ok(text) = std::ffi::CString::new(text) {
        // SAFETY: "%s" with one NUL-terminated string argument.
        unsafe { pam_syslog(pamh, LOG_NOTICE, c"%s".as_ptr(), text.as_ptr()) };
    }
}
