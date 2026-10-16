//! The greetd IPC side of an attempt (greetd-ipc(7)): `create_session` for the user, answer
//! every `auth_message`, then `start_session`.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use anyhow::{anyhow, Context, Result};
use greetd_ipc::codec::SyncCodec;
use greetd_ipc::{AuthMessageType, ErrorType, Request, Response};

use super::{Attempt, AuthEvent, Events, Reply};

/// Start an attempt for `user` on the greetd socket; events go to `events` until `Success`
/// or `Failure`. After `Success` the thread waits for [`Attempt::start_session`] and sends
/// one last `Success` when greetd accepted the command (the greeter must then exit).
pub fn start(socket: &Path, user: &str, events: Events) -> Attempt {
    let (attempt, replies) = Attempt::channel();
    let socket = socket.to_path_buf();
    let user = user.to_string();
    thread::Builder::new()
        .name("greetd".into())
        .spawn(move || {
            if let Err(e) = run(&socket, &user, &events, &replies) {
                let _ = events.send_blocking(AuthEvent::Failure(format!("greetd: {e:#}")));
            }
        })
        .expect("spawn greetd thread");
    attempt
}

fn run(socket: &Path, user: &str, events: &Events, replies: &mpsc::Receiver<Reply>) -> Result<()> {
    let mut stream = UnixStream::connect(socket).with_context(|| format!("connect {}", socket.display()))?;
    let mut request = Request::CreateSession { username: user.to_string() };
    loop {
        request.write_to(&mut stream).context("write request")?;
        match Response::read_from(&mut stream).context("read response")? {
            Response::AuthMessage { auth_message_type, auth_message } => {
                request = match auth_message_type {
                    AuthMessageType::Info => {
                        events.send_blocking(AuthEvent::Info(auth_message))?;
                        Request::PostAuthMessageResponse { response: None }
                    }
                    AuthMessageType::Error => {
                        events.send_blocking(AuthEvent::Error(auth_message))?;
                        Request::PostAuthMessageResponse { response: None }
                    }
                    kind @ (AuthMessageType::Secret | AuthMessageType::Visible) => {
                        let secret = matches!(kind, AuthMessageType::Secret);
                        events.send_blocking(AuthEvent::Prompt { text: auth_message, secret })?;
                        match replies.recv() {
                            Ok(Reply::Answer(Some(answer))) => Request::PostAuthMessageResponse { response: Some(answer) },
                            // Cancelled by the UI (or the UI is gone): greetd drops the session.
                            _ => {
                                Request::CancelSession.write_to(&mut stream).context("write cancel")?;
                                events.send_blocking(AuthEvent::Failure("cancelled".into()))?;
                                return Ok(());
                            }
                        }
                    }
                };
            }
            Response::Success => {
                events.send_blocking(AuthEvent::Success)?;
                return match replies.recv() {
                    Ok(Reply::StartSession { cmd, env }) => {
                        Request::StartSession { cmd, env }.write_to(&mut stream).context("write start_session")?;
                        match Response::read_from(&mut stream).context("read start_session response")? {
                            Response::Success => {
                                events.send_blocking(AuthEvent::Success)?;
                                Ok(())
                            }
                            Response::Error { description, .. } => Err(anyhow!("start_session: {description}")),
                            Response::AuthMessage { .. } => Err(anyhow!("start_session answered with an auth message")),
                        }
                    }
                    _ => {
                        Request::CancelSession.write_to(&mut stream).context("write cancel")?;
                        events.send_blocking(AuthEvent::Failure("cancelled".into()))?;
                        Ok(())
                    }
                };
            }
            // greetd cancels the session itself on any error.
            Response::Error { error_type: ErrorType::AuthError, description } => {
                events.send_blocking(AuthEvent::Failure(description))?;
                return Ok(());
            }
            Response::Error { error_type: ErrorType::Error, description } => return Err(anyhow!("{description}")),
        }
    }
}
