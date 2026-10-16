//! One authentication attempt, whichever program runs the PAM stack.
//!
//! Under greetd the stack runs in greetd's session worker and its conversation reaches us as
//! greetd IPC `auth_message`s ([`greetd`]); in lock mode we run the stack ourselves ([`pam`]).
//! Both produce the same [`AuthEvent`] stream on a worker thread, and take the user's replies
//! through [`Attempt`]. The face module speaks first in every attempt (`PAM_TEXT_INFO`
//! "Looking for your face...", docs/pam.md), then either the stack succeeds without a prompt
//! (`MATCH`) or the failure reason arrives as text and the password stack prompts.

use std::sync::mpsc;

pub mod greetd;
pub mod pam;

/// What the PAM stack says to the user, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    /// `PAM_TEXT_INFO` (greetd `info`): a status line or a notice; needs no answer.
    Info(String),
    /// `PAM_ERROR_MSG` (greetd `error`): shown like `Info`, needs no answer.
    Error(String),
    /// A question; the attempt blocks until [`Attempt::answer`] (`None` cancels it).
    Prompt { text: String, secret: bool },
    /// The stack succeeded: the session may start / the screen may unlock.
    Success,
    /// The stack failed (wrong password, cancelled, or every line fell through) or the
    /// backend broke; the attempt is over and a new one must be started.
    Failure(String),
}

/// What the user interface sends back to the attempt's thread.
pub enum Reply {
    /// The answer to the pending prompt; `None` cancels the attempt. Never logged.
    Answer(Option<String>),
    /// After `Success` under greetd: run this command as the session.
    StartSession { cmd: Vec<String>, env: Vec<String> },
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Reply::Answer(None) => f.write_str("Answer(None)"),
            Reply::Answer(Some(_)) => f.write_str("Answer(<redacted>)"),
            Reply::StartSession { cmd, .. } => write!(f, "StartSession({cmd:?})"),
        }
    }
}

/// The user interface's handle on a running attempt.
#[derive(Debug, Clone)]
pub struct Attempt {
    replies: mpsc::Sender<Reply>,
}

impl Attempt {
    pub(crate) fn channel() -> (Attempt, mpsc::Receiver<Reply>) {
        let (replies, rx) = mpsc::channel();
        (Attempt { replies }, rx)
    }

    /// Answer the pending prompt; harmless when the attempt has already ended. An empty
    /// answer to the face module's own prompt ends the attempt cleanly (`password_prompt`,
    /// pam/src/lib.rs). `None` (or dropping the handle) fails the conversation instead, which
    /// at a `pam_unix` prompt counts as a failed login for `pam_faillock`: the greeter never
    /// does that.
    pub fn answer(&self, text: Option<String>) {
        let _ = self.replies.send(Reply::Answer(text));
    }

    /// greetd only: start the session after `Success`.
    pub fn start_session(&self, cmd: Vec<String>, env: Vec<String>) {
        let _ = self.replies.send(Reply::StartSession { cmd, env });
    }
}

/// Where the events of an attempt go: the UI's async channel.
pub type Events = async_channel::Sender<AuthEvent>;
