//! `pam_linux_hello.so`: asks the `linux_hello daemon` over its Unix socket whether the
//! person at the camera is the user being authenticated.
//!
//! Module arguments: `socket=PATH` (default `/run/linux_hello.sock`), `timeout=SECONDS` (default 10).
//!
//! Return codes are chosen for `auth sufficient pam_linux_hello.so` placed before the password
//! modules: a match returns PAM_SUCCESS; a face that is wrong, spoofed or missing returns
//! PAM_AUTH_ERR; anything that means "face login is not available" (daemon down, user not
//! enrolled, bad reply) returns PAM_IGNORE. Either way the stack falls through to the password.

use std::ffi::{c_char, c_int, CStr};
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
    // pam_info() is a macro over pam_prompt() in pam_ext.h, so call pam_prompt directly.
    fn pam_prompt(pamh: *mut PamHandle, style: c_int, response: *mut *mut c_char, fmt: *const c_char, ...) -> c_int;
    fn pam_syslog(pamh: *const PamHandle, priority: c_int, fmt: *const c_char, ...);
}

const LOG_NOTICE: c_int = 5;
const PAM_TEXT_INFO: c_int = 4;

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

#[no_mangle]
pub extern "C" fn pam_sm_setcred(_: *mut PamHandle, _: c_int, _: c_int, _: *const *const c_char) -> c_int {
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
    let mut user_ptr: *const c_char = ptr::null();
    // SAFETY: pamh comes from libpam; on success user_ptr points to a string libpam owns.
    if unsafe { pam_get_user(pamh, &mut user_ptr, ptr::null()) } != PAM_SUCCESS || user_ptr.is_null() {
        return PAM_IGNORE;
    }
    let user = unsafe { CStr::from_ptr(user_ptr) }.to_string_lossy().into_owned();

    // Fails fast when the daemon is not running: fall back to the password silently.
    let Ok(mut stream) = UnixStream::connect(&args.socket) else {
        return PAM_IGNORE;
    };
    message(pamh, c"Looking for your face...");

    let reply = (|| -> std::io::Result<String> {
        stream.set_read_timeout(Some(args.timeout))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        writeln!(stream, "AUTH {user}")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(line.trim_end().to_string())
    })()
    .unwrap_or_default();

    let code = match reply.split_whitespace().next() {
        Some("MATCH") => PAM_SUCCESS,
        Some("NOMATCH" | "NOTLIVE" | "NOFACE") => PAM_AUTH_ERR,
        _ => PAM_IGNORE,
    };
    log(pamh, &format!("pam_linux_hello: user={user} reply={reply:?}"));
    if code == PAM_AUTH_ERR {
        message(pamh, c"Face not recognized");
    }
    code
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
