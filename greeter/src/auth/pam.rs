//! The lock-mode side of an attempt: run the `hello-lock` PAM stack ourselves, as the user,
//! like swaylock does (`auth` only, then `pam_setcred(PAM_REFRESH_CRED)`, so the face
//! module never commits enroll-on-failure from the lock screen, docs/pam.md).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::mpsc;
use std::thread;

use super::{Attempt, AuthEvent, Events, Reply};

/// The PAM service name (`/etc/pam.d/hello-lock`, contrib/install-greeter.sh).
pub const SERVICE: &str = "hello-lock";
/// The face module's own password prompt (`password_prompt`, pam/src/lib.rs): the only prompt
/// the greeter may answer with an empty string to end the attempt.
pub const PASSWORD_PROMPT: &str = "Password";

const PAM_SUCCESS: c_int = 0;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_REFRESH_CRED: c_int = 0x0010;

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *const *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, pamh: *mut *mut PamHandle)
        -> c_int;
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_setcred(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, status: c_int) -> c_int;
    fn pam_strerror(pamh: *const PamHandle, errnum: c_int) -> *const c_char;
}

/// The conversation's state, reached through `appdata_ptr` on the PAM thread only.
struct Conversation {
    events: Events,
    replies: mpsc::Receiver<Reply>,
}

/// Start an attempt for `user`; events go to `events` until `Success` or `Failure`.
pub fn start(user: &str, events: Events) -> Attempt {
    start_in(None, user, events)
}

/// [`start`] with the service file read from `confdir` instead of `/etc/pam.d`
/// (`pam_start_confdir`; the tests run the real module through a scratch stack).
pub fn start_in(confdir: Option<&Path>, user: &str, events: Events) -> Attempt {
    let (attempt, replies) = Attempt::channel();
    let user = user.to_string();
    let confdir = confdir.map(Path::to_path_buf);
    thread::Builder::new()
        .name("pam".into())
        .spawn(move || {
            let outcome = authenticate(confdir.as_deref(), &user, Conversation { events: events.clone(), replies });
            let _ = events.send_blocking(match outcome {
                Ok(()) => AuthEvent::Success,
                Err(text) => AuthEvent::Failure(text),
            });
        })
        .expect("spawn pam thread");
    attempt
}

fn authenticate(confdir: Option<&Path>, user: &str, mut conversation: Conversation) -> Result<(), String> {
    let service = CString::new(SERVICE).unwrap();
    let user = CString::new(user).map_err(|e| e.to_string())?;
    let confdir = confdir.map(|d| CString::new(d.as_os_str().as_bytes()).unwrap());
    let conv = PamConv { conv: converse, appdata_ptr: (&mut conversation as *mut Conversation).cast() };
    let mut pamh: *mut PamHandle = ptr::null_mut();
    // SAFETY: the strings and `conv` outlive the handle; `conversation` lives until pam_end.
    unsafe {
        let status = match &confdir {
            None => pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut pamh),
            Some(dir) => pam_start_confdir(service.as_ptr(), user.as_ptr(), &conv, dir.as_ptr(), &mut pamh),
        };
        if status != PAM_SUCCESS {
            return Err(format!("pam_start: {}", error_text(ptr::null(), status)));
        }
        let status = pam_authenticate(pamh, 0);
        let result = if status == PAM_SUCCESS {
            pam_setcred(pamh, PAM_REFRESH_CRED);
            Ok(())
        } else {
            Err(error_text(pamh, status))
        };
        pam_end(pamh, status);
        result
    }
}

unsafe fn error_text(pamh: *const PamHandle, status: c_int) -> String {
    let text = pam_strerror(pamh, status);
    if text.is_null() {
        format!("PAM error {status}")
    } else {
        CStr::from_ptr(text).to_string_lossy().into_owned()
    }
}

/// Linux-PAM's conversation: `msg` is an array of `num_msg` pointers; the responses are one
/// `calloc`ed array that libpam frees with `free`.
unsafe extern "C" fn converse(
    num_msg: c_int,
    msg: *const *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    let conversation = &*appdata_ptr.cast::<Conversation>();
    let count = usize::try_from(num_msg).unwrap_or(0);
    let responses = libc::calloc(count.max(1), std::mem::size_of::<PamResponse>()).cast::<PamResponse>();
    if responses.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..count {
        let message = &**msg.add(i);
        let text = if message.msg.is_null() {
            String::new()
        } else {
            CStr::from_ptr(message.msg).to_string_lossy().into_owned()
        };
        let event = match message.msg_style {
            PAM_PROMPT_ECHO_OFF => AuthEvent::Prompt { text, secret: true },
            PAM_PROMPT_ECHO_ON => AuthEvent::Prompt { text, secret: false },
            PAM_ERROR_MSG => AuthEvent::Error(text),
            PAM_TEXT_INFO => AuthEvent::Info(text),
            _ => {
                libc::free(responses.cast());
                return PAM_CONV_ERR;
            }
        };
        let prompt = matches!(event, AuthEvent::Prompt { .. });
        if conversation.events.send_blocking(event).is_err() {
            libc::free(responses.cast());
            return PAM_CONV_ERR;
        }
        if prompt {
            let answer = match conversation.replies.recv() {
                Ok(Reply::Answer(Some(answer))) => answer,
                _ => {
                    // Cancelled: free what was answered so far and fail the stack.
                    for j in 0..i {
                        libc::free((*responses.add(j)).resp.cast());
                    }
                    libc::free(responses.cast());
                    return PAM_CONV_ERR;
                }
            };
            let Ok(answer) = CString::new(answer) else {
                libc::free(responses.cast());
                return PAM_CONV_ERR;
            };
            (*responses.add(i)).resp = libc::strdup(answer.as_ptr());
        }
    }
    *resp = responses;
    PAM_SUCCESS
}
