//! The greetd client against the fake greetd of `examples/fake_greetd.rs`: the event stream
//! the UI sees for a recognised face, a password fallback, and a wrong password.

#[path = "../examples/fake_greetd.rs"]
mod fake_greetd;

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use hello_greeter::auth::{greetd, AuthEvent};

fn serve(name: &str, scenario: &str) -> PathBuf {
    let socket = std::env::temp_dir().join(format!("hello-greeter-test-{}-{name}.sock", std::process::id()));
    let scenario = fake_greetd::Scenario::named(scenario).unwrap();
    let path = socket.clone();
    thread::spawn(move || fake_greetd::serve(&path, scenario));
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    socket
}

fn next(rx: &async_channel::Receiver<AuthEvent>) -> AuthEvent {
    rx.recv_blocking().expect("event")
}

#[test]
fn recognised_face_starts_the_session_without_a_prompt() {
    let socket = serve("match", "match");
    let (tx, rx) = async_channel::unbounded();
    let attempt = greetd::start(&socket, "cole", tx);
    assert_eq!(next(&rx), AuthEvent::Info("Looking for your face...".into()));
    assert_eq!(next(&rx), AuthEvent::Success);
    attempt.start_session(vec!["true".into()], vec![]);
    assert_eq!(next(&rx), AuthEvent::Success);
}

#[test]
fn no_face_falls_back_to_the_password() {
    let socket = serve("noface", "noface");
    let (tx, rx) = async_channel::unbounded();
    let attempt = greetd::start(&socket, "cole", tx);
    assert_eq!(next(&rx), AuthEvent::Info("Looking for your face...".into()));
    assert_eq!(next(&rx), AuthEvent::Info("No face seen by the camera".into()));
    assert_eq!(next(&rx), AuthEvent::Prompt { text: "Password".into(), secret: true });
    attempt.answer(Some("hunter2".into()));
    assert_eq!(next(&rx), AuthEvent::Success);
}

#[test]
fn wrong_password_ends_the_attempt() {
    let socket = serve("wrong", "nomatch");
    let (tx, rx) = async_channel::unbounded();
    let attempt = greetd::start(&socket, "cole", tx);
    assert_eq!(next(&rx), AuthEvent::Info("Looking for your face...".into()));
    assert_eq!(next(&rx), AuthEvent::Info("Making sure it's you...".into()));
    assert_eq!(next(&rx), AuthEvent::Info("Couldn't recognize you. Please sign in with your password.".into()));
    assert!(matches!(next(&rx), AuthEvent::Prompt { secret: true, .. }));
    attempt.answer(Some("wrong".into()));
    assert_eq!(next(&rx), AuthEvent::Failure("Authentication failure".into()));
}

#[test]
fn empty_answer_ends_the_attempt() {
    let socket = serve("empty", "noface");
    let (tx, rx) = async_channel::unbounded();
    let attempt = greetd::start(&socket, "cole", tx);
    while !matches!(next(&rx), AuthEvent::Prompt { .. }) {}
    attempt.answer(Some(String::new()));
    assert_eq!(next(&rx), AuthEvent::Failure("Conversation error".into()));
}
