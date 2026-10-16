//! What the screens show and when an attempt runs: Windows' lock screen → sign-in screen flow
//! (docs/windows-hello-ui.md when it lands; docs/greeter.md) over the [`AuthEvent`] stream.
//!
//! One attempt at a time. On the lock screen attempts follow each other while the looking
//! window (`lock_face_seconds` after the screen appeared or the last key press) is open: the
//! face module's own prompt at the end of a failed attempt is answered with an empty string,
//! which ends the stack cleanly (`password_prompt`), and the next attempt starts. On the
//! sign-in screen a failed attempt leaves its prompt pending: the reason is shown with the
//! password box, and "Sign-in options → face" starts the next attempt.

use std::time::{Duration, Instant};

use crate::auth::pam::PASSWORD_PROMPT;
use crate::auth::AuthEvent;

/// The face module's lines that mean a state rather than a reason (docs/pam.md).
pub const LOOKING: &str = "Looking for your face...";
pub const VERIFYING: &str = "Making sure it's you...";
/// Windows' text after a wrong password (INFERRED from public screenshots; not in the
/// extracted string tables, docs/windows-hello-ui.md §2).
pub const WRONG_PASSWORD: &str = "The password is incorrect. Try again.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Lock,
    SignIn,
}

/// The Hello icon's state and the status text (Windows' `BioFeedbackState`,
/// docs/windows-hello-ui.md §1.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Face {
    /// Nothing running and nothing to say.
    Idle,
    /// An attempt has started; the module has not spoken yet ("Getting ready...", id 105).
    Starting,
    /// The module is looking for the face (state 2, "Looking for you...", id 104).
    Looking,
    /// Recognised, liveness being checked (state 3, "Making sure it's you...", id 123).
    Verifying,
    /// Recognised and live (state 4, "Hello <name>", id 101).
    Welcome,
    /// The face check failed (state 1, or 7 for camera errors); the reason, or `None` when
    /// face sign-in was not available at all.
    Failed(Option<String>),
}

#[derive(Clone, PartialEq, Eq)]
pub enum Input {
    /// A key press or click on the lock screen.
    Wake,
    Auth(AuthEvent),
    /// The password box was submitted. Never logged.
    Submit(String),
    /// "Sign-in options → face".
    RetryFace,
    /// "Sign-in options → password": just focus the box.
    UsePassword,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Effect {
    StartAttempt,
    /// The answer to the prompt. Never logged: it is the password.
    Answer(String),
    /// greetd: start the session (one more `Success` follows, then `Exit`).
    StartSession,
    /// Lock mode: unlock and exit.
    Unlock,
    Exit,
    FocusPassword,
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Input::Wake => f.write_str("Wake"),
            Input::Auth(e) => write!(f, "Auth({e:?})"),
            Input::Submit(_) => f.write_str("Submit(<redacted>)"),
            Input::RetryFace => f.write_str("RetryFace"),
            Input::UsePassword => f.write_str("UsePassword"),
        }
    }
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Effect::StartAttempt => f.write_str("StartAttempt"),
            Effect::Answer(text) => write!(f, "Answer({})", if text.is_empty() { "empty" } else { "<redacted>" }),
            Effect::StartSession => f.write_str("StartSession"),
            Effect::Unlock => f.write_str("Unlock"),
            Effect::Exit => f.write_str("Exit"),
            Effect::FocusPassword => f.write_str("FocusPassword"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct State {
    pub screen: Screen,
    pub face: Face,
    /// Status lines after the reason (the daemon's notices).
    pub notices: Vec<String>,
    /// A prompt is waiting: the face module's own (empty answer allowed) or another module's.
    pub prompt: Option<Prompt>,
    /// Submitted before the prompt arrived.
    pub typed: Option<String>,
    /// The password went out; waiting for the verdict.
    pub signing_in: bool,
    pub password_error: Option<String>,
    /// Something is wrong with the backend (greetd/PAM), shown instead of a face status.
    pub backend_error: Option<String>,
    attempt_running: bool,
    /// Start the next attempt when this one ends.
    restart: bool,
    session_started: bool,
    lock_window: Instant,
    lock_face_window: Duration,
    dismiss_lock_screen: bool,
    pub lock_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// The face module's `Password` prompt.
    Module,
    /// Any other prompt: only a real answer may go back.
    Other,
}

impl State {
    pub fn new(lock_mode: bool, dismiss_lock_screen: bool, lock_face_seconds: u64, now: Instant) -> State {
        State {
            screen: Screen::Lock,
            face: Face::Idle,
            notices: vec![],
            prompt: None,
            typed: None,
            signing_in: false,
            password_error: None,
            backend_error: None,
            attempt_running: false,
            restart: false,
            session_started: false,
            lock_window: now,
            lock_face_window: Duration::from_secs(lock_face_seconds),
            dismiss_lock_screen,
            lock_mode,
        }
    }

    pub fn attempt_running(&self) -> bool {
        self.attempt_running
    }

    /// The first attempt, when the screens appear.
    pub fn start(&mut self) -> Vec<Effect> {
        self.begin_attempt()
    }

    pub fn handle(&mut self, input: Input, now: Instant) -> Vec<Effect> {
        match input {
            Input::Wake => self.wake(now),
            Input::Auth(event) => self.auth(event, now),
            Input::Submit(password) => self.submit(password),
            Input::RetryFace => self.retry_face(),
            Input::UsePassword => vec![Effect::FocusPassword],
        }
    }

    fn begin_attempt(&mut self) -> Vec<Effect> {
        self.attempt_running = true;
        self.restart = false;
        self.prompt = None;
        self.notices.clear();
        self.backend_error = None;
        self.face = Face::Starting;
        vec![Effect::StartAttempt]
    }

    fn wake(&mut self, now: Instant) -> Vec<Effect> {
        self.lock_window = now;
        if self.screen == Screen::SignIn {
            return vec![];
        }
        self.screen = Screen::SignIn;
        if self.face == Face::Welcome {
            // The lock screen showed "Welcome" and waited for this (dismiss_lock_screen off).
            return vec![self.sign_in_effect()];
        }
        let mut effects = vec![Effect::FocusPassword];
        if !self.attempt_running && self.typed.is_none() {
            effects.extend(self.begin_attempt());
        }
        effects
    }

    fn auth(&mut self, event: AuthEvent, now: Instant) -> Vec<Effect> {
        match event {
            AuthEvent::Info(text) if text == LOOKING => {
                self.face = Face::Looking;
                vec![]
            }
            AuthEvent::Info(text) if text == VERIFYING => {
                self.face = Face::Verifying;
                vec![]
            }
            AuthEvent::Info(text) | AuthEvent::Error(text) => {
                // The first line after "Looking" is the verdict's reason; the rest are notices.
                if matches!(self.face, Face::Starting | Face::Looking | Face::Verifying) {
                    self.face = Face::Failed(Some(text));
                } else {
                    self.notices.push(text);
                }
                vec![]
            }
            AuthEvent::Prompt { text, .. } => {
                let prompt = if text == PASSWORD_PROMPT { Prompt::Module } else { Prompt::Other };
                self.prompt = Some(prompt);
                if matches!(self.face, Face::Starting | Face::Looking | Face::Verifying | Face::Idle) {
                    self.face = Face::Failed(None);
                }
                if let Some(password) = self.typed.take() {
                    self.signing_in = true;
                    return vec![Effect::Answer(password)];
                }
                let lock_window_open = now.duration_since(self.lock_window) < self.lock_face_window;
                if self.screen == Screen::Lock && prompt == Prompt::Module && lock_window_open {
                    return self.end_attempt_and_restart();
                }
                vec![Effect::FocusPassword]
            }
            AuthEvent::Success => {
                if self.session_started {
                    return vec![Effect::Exit];
                }
                self.face = Face::Welcome;
                self.prompt = None;
                self.signing_in = false;
                self.attempt_running = !self.lock_mode;
                if self.screen == Screen::Lock && !self.dismiss_lock_screen {
                    return vec![];
                }
                vec![self.sign_in_effect()]
            }
            AuthEvent::Failure(text) => {
                self.attempt_running = false;
                self.prompt = None;
                if self.signing_in {
                    self.signing_in = false;
                    self.password_error = Some(WRONG_PASSWORD.into());
                    self.face = Face::Idle;
                    let mut effects = self.begin_attempt();
                    effects.push(Effect::FocusPassword);
                    return effects;
                }
                if self.restart {
                    return self.begin_attempt();
                }
                self.face = Face::Idle;
                self.backend_error = Some(text);
                vec![]
            }
        }
    }

    fn sign_in_effect(&mut self) -> Effect {
        if self.lock_mode {
            Effect::Unlock
        } else {
            self.session_started = true;
            Effect::StartSession
        }
    }

    /// Answer the module's prompt with nothing: the attempt ends, the next one starts.
    fn end_attempt_and_restart(&mut self) -> Vec<Effect> {
        self.restart = true;
        self.prompt = None;
        vec![Effect::Answer(String::new())]
    }

    fn submit(&mut self, password: String) -> Vec<Effect> {
        if password.is_empty() {
            return vec![];
        }
        self.password_error = None;
        self.screen = Screen::SignIn;
        if self.prompt.take().is_some() {
            self.signing_in = true;
            return vec![Effect::Answer(password)];
        }
        self.typed = Some(password);
        if self.attempt_running {
            vec![]
        } else {
            self.begin_attempt()
        }
    }

    fn retry_face(&mut self) -> Vec<Effect> {
        self.password_error = None;
        match self.prompt {
            Some(Prompt::Module) => self.end_attempt_and_restart(),
            Some(Prompt::Other) => vec![],
            None if self.attempt_running => vec![],
            None => self.begin_attempt(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(text: &str) -> Input {
        Input::Auth(AuthEvent::Info(text.into()))
    }
    fn prompt() -> Input {
        Input::Auth(AuthEvent::Prompt { text: PASSWORD_PROMPT.into(), secret: true })
    }
    fn greeter() -> (State, Instant) {
        let now = Instant::now();
        (State::new(false, true, 120, now), now)
    }

    #[test]
    fn recognised_on_the_lock_screen_starts_the_session() {
        let (mut s, now) = greeter();
        assert_eq!(s.start(), vec![Effect::StartAttempt]);
        assert_eq!(s.face, Face::Starting);
        assert_eq!(s.handle(info(LOOKING), now), vec![]);
        assert_eq!(s.face, Face::Looking);
        assert_eq!(s.handle(Input::Auth(AuthEvent::Success), now), vec![Effect::StartSession]);
        assert_eq!(s.face, Face::Welcome);
        assert_eq!(s.handle(Input::Auth(AuthEvent::Success), now), vec![Effect::Exit]);
    }

    #[test]
    fn lock_screen_keeps_looking_within_the_window_then_waits() {
        let (mut s, now) = greeter();
        s.start();
        s.handle(info(LOOKING), now);
        s.handle(info("No face seen by the camera"), now);
        assert_eq!(s.handle(prompt(), now), vec![Effect::Answer(String::new())]);
        assert_eq!(
            s.handle(Input::Auth(AuthEvent::Failure("Conversation error".into())), now),
            vec![Effect::StartAttempt]
        );
        // After the window the prompt stays pending and the cameras are left alone.
        let later = now + Duration::from_secs(121);
        s.handle(info(LOOKING), later);
        s.handle(info("No face seen by the camera"), later);
        assert_eq!(s.handle(prompt(), later), vec![Effect::FocusPassword]);
        assert_eq!(s.prompt, Some(Prompt::Module));
        // A key press ends that attempt and starts a fresh one on the sign-in screen.
        assert_eq!(s.handle(Input::Wake, later), vec![Effect::FocusPassword]);
        assert_eq!(s.screen, Screen::SignIn);
        assert_eq!(s.handle(Input::RetryFace, later), vec![Effect::Answer(String::new())]);
    }

    #[test]
    fn sign_in_screen_shows_the_reason_and_takes_the_password() {
        let (mut s, now) = greeter();
        s.start();
        assert_eq!(s.handle(Input::Wake, now), vec![Effect::FocusPassword]);
        s.handle(info(LOOKING), now);
        assert_eq!(s.handle(info(VERIFYING), now), vec![]);
        assert_eq!(s.face, Face::Verifying);
        s.handle(info("Couldn't recognize you. Please sign in with your password."), now);
        assert_eq!(s.handle(prompt(), now), vec![Effect::FocusPassword]);
        assert_eq!(s.face, Face::Failed(Some("Couldn't recognize you. Please sign in with your password.".into())));
        assert_eq!(s.handle(Input::Submit("hunter2".into()), now), vec![Effect::Answer("hunter2".into())]);
        assert!(s.signing_in);
        assert_eq!(s.handle(Input::Auth(AuthEvent::Success), now), vec![Effect::StartSession]);
    }

    #[test]
    fn password_typed_while_looking_answers_the_prompt_when_it_comes() {
        let (mut s, now) = greeter();
        s.start();
        s.handle(Input::Wake, now);
        s.handle(info(LOOKING), now);
        assert_eq!(s.handle(Input::Submit("hunter2".into()), now), vec![]);
        s.handle(info("Face not recognized"), now);
        assert_eq!(s.handle(prompt(), now), vec![Effect::Answer("hunter2".into())]);
    }

    #[test]
    fn wrong_password_says_so_and_looks_again() {
        let (mut s, now) = greeter();
        s.start();
        s.handle(Input::Wake, now);
        s.handle(info(LOOKING), now);
        s.handle(info("Face not recognized"), now);
        s.handle(prompt(), now);
        s.handle(Input::Submit("wrong".into()), now);
        assert_eq!(
            s.handle(Input::Auth(AuthEvent::Failure("Authentication failure".into())), now),
            vec![Effect::StartAttempt, Effect::FocusPassword]
        );
        assert_eq!(s.password_error.as_deref(), Some(WRONG_PASSWORD));
    }

    #[test]
    fn another_modules_prompt_is_never_answered_with_nothing() {
        let (mut s, now) = greeter();
        s.start();
        s.handle(Input::Auth(AuthEvent::Prompt { text: "Password: ".into(), secret: true }), now);
        assert_eq!(s.prompt, Some(Prompt::Other));
        assert_eq!(s.face, Face::Failed(None));
        assert_eq!(s.handle(Input::Wake, now), vec![Effect::FocusPassword]);
        assert_eq!(s.handle(Input::RetryFace, now), vec![]);
    }

    #[test]
    fn lock_mode_unlocks_and_welcome_waits_for_a_key_when_not_dismissing() {
        let now = Instant::now();
        let mut s = State::new(true, false, 120, now);
        s.start();
        s.handle(info(LOOKING), now);
        assert_eq!(s.handle(Input::Auth(AuthEvent::Success), now), vec![]);
        assert_eq!(s.face, Face::Welcome);
        assert_eq!(s.handle(Input::Wake, now), vec![Effect::Unlock]);
    }

    #[test]
    fn backend_error_is_shown_and_not_retried_in_a_loop() {
        let (mut s, now) = greeter();
        s.start();
        assert_eq!(s.handle(Input::Auth(AuthEvent::Failure("greetd: connect".into())), now), vec![]);
        assert_eq!(s.backend_error.as_deref(), Some("greetd: connect"));
        assert!(!s.attempt_running());
        assert_eq!(s.handle(Input::Wake, now), vec![Effect::FocusPassword, Effect::StartAttempt]);
    }
}
