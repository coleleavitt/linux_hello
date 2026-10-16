//! A stand-in for greetd on a scratch socket, for running and testing the greeter without
//! root: it answers the greeter's IPC the way greetd does when the PAM stack is
//! `pam_linux_hello` then `pam_unix` (docs/pam.md), for one scripted scenario.
//!
//! ```sh
//! cargo run --example fake_greetd -- /tmp/fake-greetd.sock password --delay 2000
//! GREETD_SOCK=/tmp/fake-greetd.sock cargo run -- --window
//! ```
//!
//! Scenarios: `match` (the face is recognised: no prompt), `noface` (nobody seen, then the
//! password prompt), `nomatch` (seen but not recognised, then the password prompt),
//! `notlive`, `busy` (the camera is in use: the module falls through at once). The accepted
//! password is `--password` (default `hunter2`); a wrong one is an `auth_error`, as greetd
//! reports a failed stack, and so is an empty answer (the module ends the stack, pam/src/lib.rs
//! `password_prompt`). `--delay` is the camera time before the face verdict.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::thread;
use std::time::Duration;

use greetd_ipc::codec::SyncCodec;
use greetd_ipc::{AuthMessageType, ErrorType, Request, Response};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone)]
pub struct Scenario {
    /// What the face module prints before the verdict (`PAM_TEXT_INFO` lines).
    pub steps: Vec<String>,
    /// `MATCH`: the stack succeeds without a prompt.
    pub face_match: bool,
    pub password: String,
    pub delay: Duration,
}

impl Scenario {
    pub fn named(name: &str) -> Option<Scenario> {
        let looking = "Looking for your face...";
        let (steps, face_match) = match name {
            "match" => (vec![looking], true),
            "noface" => (vec![looking, "No face seen by the camera"], false),
            "nomatch" => (
                vec![looking, "Making sure it's you...", "Couldn't recognize you. Please sign in with your password."],
                false,
            ),
            "notlive" => (vec![looking, "Face not accepted as live"], false),
            "busy" => (vec![looking, "Face login not available: another program is using the camera"], false),
            _ => return None,
        };
        let steps = steps.into_iter().map(String::from).collect();
        Some(Scenario { steps, face_match, password: "hunter2".into(), delay: Duration::ZERO })
    }
}

/// Serve `scenario` on `socket` until the listener is dropped (the thread ends with the process).
pub fn serve(socket: &Path, scenario: Scenario) -> Result<()> {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    for stream in listener.incoming() {
        let scenario = scenario.clone();
        thread::spawn(move || {
            if let Err(e) = session(stream?, &scenario) {
                eprintln!("fake_greetd: {e}");
            }
            Result::Ok(())
        });
    }
    Ok(())
}

fn auth_message(auth_message_type: AuthMessageType, text: &str) -> Response {
    Response::AuthMessage { auth_message_type, auth_message: text.to_string() }
}

fn session(mut stream: UnixStream, scenario: &Scenario) -> Result<()> {
    let fail = |stream: &mut UnixStream, description: &str| -> Result<()> {
        Response::Error { error_type: ErrorType::Error, description: description.into() }.write_to(stream)?;
        Ok(())
    };
    match Request::read_from(&mut stream)? {
        Request::CreateSession { username } => eprintln!("fake_greetd: create_session {username}"),
        _ => return fail(&mut stream, "no session under configuration"),
    }
    let mut delayed = false;
    for text in &scenario.steps {
        auth_message(AuthMessageType::Info, text).write_to(&mut stream)?;
        match Request::read_from(&mut stream)? {
            Request::PostAuthMessageResponse { .. } => {}
            Request::CancelSession => return Ok(()),
            _ => return fail(&mut stream, "unexpected request"),
        }
        // The camera time comes after "Looking for your face..." was acknowledged, and half of
        // it again after "Making sure it's you..." (the liveness votes).
        if !delayed {
            delayed = true;
            thread::sleep(scenario.delay);
        } else if text.starts_with("Making sure") {
            thread::sleep(scenario.delay / 2);
        }
    }
    if !scenario.face_match {
        // The face module's own prompt (`password_prompt`): an empty answer ends the stack
        // at once, a wrong password reaches pam_unix (its failure delay) and pam_faillock.
        auth_message(AuthMessageType::Secret, "Password").write_to(&mut stream)?;
        match Request::read_from(&mut stream)? {
            Request::PostAuthMessageResponse { response: Some(p) } if p == scenario.password => {}
            Request::PostAuthMessageResponse { response } => {
                let description = if response.is_none_or(|p| p.is_empty()) {
                    "Conversation error"
                } else {
                    thread::sleep(Duration::from_secs(2));
                    "Authentication failure"
                };
                Response::Error { error_type: ErrorType::AuthError, description: description.into() }
                    .write_to(&mut stream)?;
                return Ok(());
            }
            Request::CancelSession => return Ok(()),
            _ => return fail(&mut stream, "unexpected request"),
        }
    }
    Response::Success.write_to(&mut stream)?;
    match Request::read_from(&mut stream)? {
        Request::StartSession { cmd, env } => {
            eprintln!("fake_greetd: start_session {cmd:?} env={env:?}");
            Response::Success.write_to(&mut stream)?;
            Ok(())
        }
        Request::CancelSession => Ok(()),
        _ => fail(&mut stream, "unexpected request"),
    }
}

#[allow(dead_code)]
fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: fake_greetd <socket> match|noface|nomatch|notlive|busy [--delay MS] [--password PW]";
    let socket = args.next().expect(usage);
    let mut scenario = Scenario::named(&args.next().expect(usage)).expect(usage);
    while let Some(flag) = args.next() {
        let value = args.next().expect(usage);
        match flag.as_str() {
            "--delay" => scenario.delay = Duration::from_millis(value.parse().expect(usage)),
            "--password" => scenario.password = value,
            _ => panic!("{usage}"),
        }
    }
    serve(Path::new(&socket), scenario).expect("serve");
}
