//! Browser-launched mode: validate the caller, then answer framed messages until the
//! browser closes stdin (one-shot `sendNativeMessage` or a disconnected port) or the port
//! stays idle. stdout carries only protocol frames; stderr only short diagnostics (message
//! types, error codes, sizes), never page content, URLs or credentials.
use crate::{
    caller::{self, Caller},
    config::{HostConfig, Store, euid},
    daemon::{self, HostConnector},
    frame::{self, MAX_INCOMING, MAX_OUTGOING, ReadError},
    message::{self, ErrorCode, HostError, Request},
};
use cm_protocol::client::local::DaemonPaths;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::Duration,
};

/// An open port with no message for this long is closed by exiting (the daemon stays up
/// for its own jobs; the extension reconnects on demand).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub mod exit {
    pub const OK: u8 = 0;
    pub const OUTPUT: u8 = 1;
    pub const USAGE: u8 = 2;
    pub const CALLER_REJECTED: u8 = 3;
    pub const BAD_FRAME: u8 = 4;
}

pub fn diag(message: &str) {
    let _ = writeln!(io::stderr().lock(), "papers-native-host: {message}");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The browser closed stdin.
    Closed,
    Idle,
    /// Oversized or truncated frame: an error was sent (if possible) and the host stops.
    BadFrame,
    /// stdout is gone (browser exited).
    OutputClosed,
}

/// Reads frames on a helper thread so an idle port can be detected.
pub fn serve<R, W>(
    input: R,
    output: &mut W,
    idle: Duration,
    handle: &mut dyn FnMut(Request) -> Result<Value, HostError>,
) -> Ended
where
    R: Read + Send + 'static,
    W: Write,
{
    let (tx, rx) = mpsc::sync_channel::<Result<Option<Vec<u8>>, ReadError>>(1);
    thread::spawn(move || {
        let mut input = input;
        loop {
            let next = frame::read_message(&mut input, MAX_INCOMING);
            let stop = !matches!(next, Ok(Some(_)));
            if tx.send(next).is_err() || stop {
                return;
            }
        }
    });
    loop {
        let body = match rx.recv_timeout(idle) {
            Ok(Ok(Some(body))) => body,
            Ok(Ok(None)) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ended::Closed,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                diag("idle port, exiting");
                return Ended::Idle;
            }
            Ok(Err(error)) => {
                diag(&error.to_string());
                let error = match error {
                    ReadError::TooLarge(_) => HostError::new(
                        ErrorCode::TooLarge,
                        format!("messages are limited to {MAX_INCOMING} bytes"),
                    ),
                    _ => HostError::new(ErrorCode::BadMessage, "malformed native message frame"),
                };
                let _ = respond(output, message::failure(&Value::Null, &error));
                return Ended::BadFrame;
            }
        };
        let size = body.len();
        let response = match message::parse(&body) {
            Ok(incoming) => {
                drop(body);
                let kind = incoming.request.type_name();
                match handle(incoming.request) {
                    Ok(result) => {
                        diag(&format!("{kind}: ok ({size} bytes in)"));
                        message::success(&incoming.id, kind, result)
                    }
                    Err(error) => {
                        diag(&format!("{kind}: {:?} ({size} bytes in)", error.code));
                        message::failure(&incoming.id, &error)
                    }
                }
            }
            Err((id, error)) => {
                diag(&format!(
                    "rejected message: {:?} ({size} bytes in)",
                    error.code
                ));
                message::failure(&id, &error)
            }
        };
        if respond(output, response).is_err() {
            return Ended::OutputClosed;
        }
    }
}

/// Encodes a response; one over the 1 MiB browser limit becomes a `response_too_large` error.
fn respond<W: Write>(output: &mut W, response: Value) -> io::Result<()> {
    let mut body = serde_json::to_vec(&response).map_err(io::Error::other)?;
    if body.len() > MAX_OUTGOING {
        let error = HostError::new(
            ErrorCode::ResponseTooLarge,
            "result exceeds the 1 MiB native messaging limit",
        );
        body = serde_json::to_vec(&message::failure(&response["id"], &error))
            .map_err(io::Error::other)?;
    }
    frame::write_message(output, &body)
}

pub struct HostContext {
    pub caller: Caller,
    pub config: HostConfig,
    pub store: Store,
    pub exe: PathBuf,
    pub uid: u32,
}
impl HostContext {
    pub fn handle(&self, request: Request) -> Result<Value, HostError> {
        let Some(command) = daemon::command(request) else {
            return Ok(self.hello());
        };
        let credential = self.store.credential().map_err(|_| {
            HostError::new(
                ErrorCode::NotPaired,
                "browser capture is not paired with the library; run `papers-native-host pair`",
            )
        })?;
        let paths = DaemonPaths::new(
            &self.config.library,
            self.config.runtime_dir.as_deref(),
            self.uid,
        )
        .map_err(|e| HostError::new(ErrorCode::DaemonUnavailable, e.to_string()))?;
        let connector = HostConnector::capture(paths, self.uid, credential, &self.exe);
        daemon::call(&connector, command)
    }
    fn hello(&self) -> Value {
        json!({
            "host_version": env!("CARGO_PKG_VERSION"),
            "protocol": message::HOST_PROTOCOL_VERSION,
            "browser": self.caller.browser(),
            "paired": self.store.credential().is_ok(),
            "types": message::TYPES,
            "limits": { "max_request_bytes": MAX_INCOMING, "max_response_bytes": MAX_OUTGOING },
        })
    }
}

/// Entry point when a browser starts the host. Returns the process exit code.
pub fn main(args: &[OsString]) -> u8 {
    let Some(caller) = caller::parse(args) else {
        diag("not started by a browser (unrecognized arguments); see `papers-native-host help`");
        return exit::USAGE;
    };
    let store = match Store::default_location() {
        Ok(store) => store,
        Err(e) => {
            diag(&format!("cannot locate configuration: {e}"));
            return exit::CALLER_REJECTED;
        }
    };
    let config = match store.load() {
        Ok(config) => config,
        Err(e) => {
            diag(&format!(
                "not registered ({e}); run `papers-native-host register`"
            ));
            return exit::CALLER_REJECTED;
        }
    };
    if !config.allowed.allows(&caller) {
        let id = match &caller {
            Caller::Chrome { extension_id } | Caller::Firefox { extension_id, .. } => extension_id,
        };
        diag(&format!(
            "{} extension {id} is not allowed",
            caller.browser()
        ));
        return exit::CALLER_REJECTED;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            diag(&format!("cannot resolve own executable: {e}"));
            return exit::OUTPUT;
        }
    };
    let context = HostContext {
        caller,
        config,
        store,
        exe,
        uid: euid(),
    };
    let mut stdout = io::stdout().lock();
    match serve(io::stdin(), &mut stdout, IDLE_TIMEOUT, &mut |r| {
        context.handle(r)
    }) {
        Ended::Closed | Ended::Idle => exit::OK,
        Ended::BadFrame => exit::BAD_FRAME,
        Ended::OutputClosed => exit::OUTPUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn framed(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut out = (body.len() as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }
    fn responses(mut out: &[u8]) -> Vec<Value> {
        let mut list = Vec::new();
        while let Some(body) = frame::read_message(&mut out, u32::MAX).unwrap() {
            list.push(serde_json::from_slice(&body).unwrap());
        }
        list
    }

    #[test]
    fn answers_each_message_then_ends_on_eof() {
        let input = [
            framed(&json!({"v":1,"id":"1","type":"hello"})),
            framed(&json!({"v":9,"id":"2","type":"hello"})),
            framed(&json!({"v":1,"id":"3","type":"jobs.submit"})),
            {
                let mut f = 5u32.to_ne_bytes().to_vec();
                f.extend_from_slice(b"nope!");
                f
            },
            framed(&json!({"v":1,"id":"4","type":"destinations"})),
        ]
        .concat();
        let mut seen = Vec::new();
        let mut out = Vec::new();
        let ended = serve(
            Cursor::new(input),
            &mut out,
            Duration::from_secs(5),
            &mut |r| {
                seen.push(r.type_name());
                match r {
                    Request::Hello => Ok(json!({"hello": true})),
                    _ => Err(HostError::new(ErrorCode::DaemonUnavailable, "down")),
                }
            },
        );
        assert_eq!(ended, Ended::Closed);
        assert_eq!(seen, ["hello", "destinations"]);
        let r = responses(&out);
        assert_eq!(r.len(), 5);
        assert_eq!(
            (r[0]["id"].clone(), r[0]["ok"].clone()),
            (json!("1"), json!(true))
        );
        assert_eq!(r[1]["error"]["code"], "unsupported_version");
        assert_eq!(r[2]["error"]["code"], "unknown_type");
        assert_eq!(
            (r[3]["id"].clone(), r[3]["error"]["code"].clone()),
            (Value::Null, json!("bad_message"))
        );
        assert_eq!(r[4]["error"]["retryable"], json!(true));
    }

    #[test]
    fn oversized_or_truncated_frames_stop_the_host_after_an_error() {
        let mut out = Vec::new();
        let ended = serve(
            Cursor::new((MAX_INCOMING + 1).to_ne_bytes().to_vec()),
            &mut out,
            Duration::from_secs(5),
            &mut |_| unreachable!(),
        );
        assert_eq!(ended, Ended::BadFrame);
        assert_eq!(responses(&out)[0]["error"]["code"], "too_large");
        let mut out = Vec::new();
        let ended = serve(
            Cursor::new(vec![9u8, 0, 0, 0, b'{']),
            &mut out,
            Duration::from_secs(5),
            &mut |_| unreachable!(),
        );
        assert_eq!(ended, Ended::BadFrame);
        assert_eq!(responses(&out)[0]["error"]["code"], "bad_message");
    }

    #[test]
    fn results_over_the_browser_limit_become_errors() {
        let mut out = Vec::new();
        serve(
            Cursor::new(framed(&json!({"v":1,"id":"big","type":"destinations"}))),
            &mut out,
            Duration::from_secs(5),
            &mut |_| Ok(json!({"blob": "x".repeat(MAX_OUTGOING)})),
        );
        let r = responses(&out);
        assert_eq!(r[0]["id"], "big");
        assert_eq!(r[0]["error"]["code"], "response_too_large");
    }

    #[test]
    fn an_idle_open_port_ends_the_host() {
        struct Never;
        impl Read for Never {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                thread::sleep(Duration::from_secs(3600));
                Ok(0)
            }
        }
        let mut out = Vec::new();
        let ended = serve(
            Never,
            &mut out,
            Duration::from_millis(50),
            &mut |_| unreachable!(),
        );
        assert_eq!(ended, Ended::Idle);
        assert!(out.is_empty());
    }
}
