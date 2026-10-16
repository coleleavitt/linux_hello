//! The lock-mode conversation against the real `pam_linux_hello.so` (built by
//! `cargo build -p pam_linux_hello`, else these tests are skipped) through a scratch stack like
//! the installed `/etc/pam.d/hello-lock`, with a stub daemon: the face match, the password
//! handed over to the password module without a second prompt, and an empty answer ending
//! the stack before the password modules (no pam_faillock tally).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;

use hello_greeter::auth::{pam, AuthEvent};

/// The module in the test executable's profile directory (an ancestor), when it has been built.
fn module() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|dir| dir.join("libpam_linux_hello.so")).find(|so| so.exists())
}

/// A daemon that answers each connection with the next reply.
fn stub_daemon(socket: &Path, replies: &[&str]) {
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket).unwrap();
    let replies: Vec<String> = replies.iter().map(|r| r.to_string()).collect();
    thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            writeln!(stream, "{reply}").unwrap();
        }
    });
}

/// `/etc/pam.d/hello-lock` as contrib/install-greeter.sh writes it, with `pam_exec` checking
/// the password `hunter2` from `PAM_AUTHTOK` in place of `pam_unix try_first_pass`, and
/// `pam_echo` standing in for `pam_faillock authfail`: reaching it shows in the events
/// (`pam_permit` at the end because a jump past the end of a stack is an error).
fn stack(name: &str, replies: &[&str]) -> (PathBuf, String) {
    let Some(module) = module() else {
        eprintln!("skipped: build pam_linux_hello first");
        std::process::exit(0);
    };
    let dir = std::env::temp_dir().join(format!("hello-greeter-pam-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("daemon.sock");
    stub_daemon(&socket, replies);
    let check = dir.join("check-password.sh");
    std::fs::write(&check, "#!/bin/sh\nread p; [ \"$p\" = hunter2 ]\n").unwrap();
    std::fs::set_permissions(&check, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join(pam::SERVICE),
        format!(
            "auth [success=done conv_err=die default=ignore] {} socket={} password_prompt wait_services=hello-lock timeout=1\n\
             auth [success=1 default=bad] pam_exec.so quiet expose_authtok {}\n\
             auth [default=die] pam_echo.so TALLY\n\
             auth required pam_permit.so\n",
            module.display(),
            socket.display(),
            check.display()
        ),
    )
    .unwrap();
    (dir, whoami::username())
}

mod whoami {
    pub fn username() -> String {
        std::env::var("USER").unwrap_or_else(|_| "root".into())
    }
}

fn events(rx: &async_channel::Receiver<AuthEvent>) -> Vec<AuthEvent> {
    let mut all = vec![];
    while let Ok(e) = rx.recv_blocking() {
        let last = matches!(e, AuthEvent::Success | AuthEvent::Failure(_));
        all.push(e);
        if last {
            break;
        }
    }
    all
}

#[test]
fn face_match_succeeds_without_a_prompt() {
    let (dir, user) = stack("match", &["MATCH 0.85"]);
    let (tx, rx) = async_channel::unbounded();
    let _attempt = pam::start_in(Some(&dir), &user, tx);
    assert_eq!(events(&rx), vec![AuthEvent::Info("Looking for your face...".into()), AuthEvent::Success]);
}

#[test]
fn password_is_handed_to_the_password_module() {
    let (dir, user) = stack("password", &["NOFACE no_face"]);
    let (tx, rx) = async_channel::unbounded();
    let attempt = pam::start_in(Some(&dir), &user, tx);
    let prompt = AuthEvent::Prompt { text: pam::PASSWORD_PROMPT.into(), secret: true };
    let mut seen = vec![];
    loop {
        let e = rx.recv_blocking().unwrap();
        seen.push(e.clone());
        if e == prompt {
            break;
        }
    }
    assert_eq!(
        seen,
        vec![AuthEvent::Info("Looking for your face...".into()), AuthEvent::Info("No face seen by the camera".into()), prompt]
    );
    attempt.answer(Some("hunter2".into()));
    assert_eq!(events(&rx), vec![AuthEvent::Success]);
}

#[test]
fn wrong_password_reaches_the_tally_line() {
    let (dir, user) = stack("wrong", &["NOMATCH 0.300"]);
    let (tx, rx) = async_channel::unbounded();
    let attempt = pam::start_in(Some(&dir), &user, tx);
    while !matches!(rx.recv_blocking().unwrap(), AuthEvent::Prompt { .. }) {}
    attempt.answer(Some("wrong".into()));
    let rest = events(&rx);
    assert_eq!(rest[0], AuthEvent::Info("TALLY".into()));
    assert!(matches!(rest.last(), Some(AuthEvent::Failure(_))));
}

#[test]
fn empty_answer_ends_the_stack_before_the_password_modules() {
    let (dir, user) = stack("empty", &["NOFACE no_face"]);
    let (tx, rx) = async_channel::unbounded();
    let attempt = pam::start_in(Some(&dir), &user, tx);
    while !matches!(rx.recv_blocking().unwrap(), AuthEvent::Prompt { .. }) {}
    attempt.answer(Some(String::new()));
    let rest = events(&rx);
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert!(matches!(&rest[0], AuthEvent::Failure(text) if text.contains("onversation")), "{rest:?}");
}
