//! Client liveness against scripted fake daemons (no real daemon needed): handshake and
//! request deadlines, connection drop, idle-exit retry and reconnect backoff (#25 review).
use super::*;
use crate::{
    exchange::ImportRequestUi,
    ipc::{CapabilitySet, Credential, GrantId, MAX_IN_FLIGHT, PROTOCOL_VERSION},
    library::{LibraryClient, LibraryRequest, PaperList, ServiceError},
};
use std::{
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Script {
    /// Welcome, then read requests forever and never answer.
    Silent,
    /// Accept the Hello, never send a Welcome.
    NoWelcome,
    /// Close the first connection during the handshake, then behave like `Silent`.
    CloseFirst,
    /// Close every connection during the handshake.
    CloseAlways,
    /// The connector itself fails (no daemon).
    Refuse,
}
#[derive(Debug, PartialEq)]
enum Seen {
    Request(u64),
    Cancel(u64),
    Closed,
}
struct Fake {
    script: Script,
    connects: Arc<AtomicUsize>,
    seen: Mutex<mpsc::Sender<Seen>>,
}
fn fake(script: Script) -> (Fake, Arc<AtomicUsize>, mpsc::Receiver<Seen>) {
    let connects = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel();
    let fake = Fake {
        script,
        connects: connects.clone(),
        seen: Mutex::new(tx),
    };
    (fake, connects, rx)
}
fn hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        client_kind: "test".into(),
        requested: None,
        library: None,
        credential: Credential("not-a-secret".into()),
    }
}
fn welcome() -> ServerFrame {
    ServerFrame::Welcome(Welcome {
        protocol_version: PROTOCOL_VERSION,
        daemon_version: "fake".into(),
        session: 1,
        grant: GrantId(uuid::Uuid::nil()),
        capabilities: CapabilitySet::new(),
        max_request_frame: MAX_REQUEST_FRAME,
        max_in_flight: MAX_IN_FLIGHT as u32,
    })
}
impl Connector for Fake {
    fn connect(&self) -> io::Result<(Box<dyn Stream>, Hello)> {
        let attempt = self.connects.fetch_add(1, Ordering::SeqCst);
        let script = match self.script {
            Script::Refuse => return Err(io::ErrorKind::ConnectionRefused.into()),
            Script::CloseFirst if attempt == 0 => Script::CloseAlways,
            Script::CloseFirst => Script::Silent,
            other => other,
        };
        let (client, mut server) = UnixStream::pair()?;
        let seen = self.seen.lock().unwrap().clone();
        thread::spawn(move || {
            if matches!(script, Script::CloseAlways) {
                drop(server);
                return;
            }
            let _ = read_frame::<_, ClientFrame>(&mut server, MAX_HANDSHAKE_FRAME);
            if matches!(script, Script::Silent) {
                write_frame(&mut server, &welcome(), MAX_RESPONSE_FRAME).unwrap();
            }
            loop {
                match read_frame::<_, ClientFrame>(&mut server, MAX_REQUEST_FRAME) {
                    Ok(Some(ClientFrame::Request { id, .. })) => {
                        let _ = seen.send(Seen::Request(id));
                    }
                    Ok(Some(ClientFrame::Cancel { id })) => {
                        let _ = seen.send(Seen::Cancel(id));
                    }
                    _ => {
                        let _ = seen.send(Seen::Closed);
                        return;
                    }
                }
            }
        });
        Ok((Box::new(client), hello()))
    }
}

fn list(
    client: &IpcLibraryClient,
) -> mpsc::Receiver<Result<crate::library::LibraryReply, ServiceError>> {
    let (tx, rx) = mpsc::channel();
    client
        .submit(
            LibraryRequest::List(PaperList::default()),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
    rx
}

#[test]
fn request_to_a_daemon_that_never_answers_completes_unavailable_and_drops_the_connection() {
    let (connector, _, seen) = fake(Script::Silent);
    let timeouts = ClientTimeouts {
        interactive: Duration::from_millis(300),
        ..ClientTimeouts::default()
    };
    let client = IpcLibraryClient::with_timeouts(connector, timeouts).unwrap();
    let started = Instant::now();
    let reply = list(&client).recv_timeout(Duration::from_secs(10)).unwrap();
    let elapsed = started.elapsed();
    assert_eq!(reply.unwrap_err(), ServiceError::Unavailable);
    assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    // The daemon got the request, a best-effort cancel, then the connection was dropped.
    let Seen::Request(id) = seen.recv_timeout(Duration::from_secs(5)).unwrap() else {
        panic!("expected the request first");
    };
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).unwrap(),
        Seen::Cancel(id)
    );
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).unwrap(),
        Seen::Closed
    );
}

#[test]
fn an_expired_request_does_not_abort_a_long_operation_still_in_its_deadline() {
    let (connector, _, seen) = fake(Script::Silent);
    let timeouts = ClientTimeouts {
        interactive: Duration::from_millis(200),
        long: Duration::from_secs(60),
        ..ClientTimeouts::default()
    };
    let client = IpcLibraryClient::with_timeouts(connector, timeouts).unwrap();
    let (tx, long) = mpsc::channel();
    client
        .import(
            ImportRequestUi::Cancel(crate::import::PreviewId("p".into())),
            Box::new(move |r| {
                let _ = tx.send(r);
            }),
        )
        .unwrap();
    let reply = list(&client).recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(reply.unwrap_err(), ServiceError::Unavailable);
    let mut events = Vec::new();
    while let Ok(event) = seen.recv_timeout(Duration::from_millis(500)) {
        events.push(event);
    }
    assert!(!events.contains(&Seen::Closed), "{events:?}");
    assert!(
        long.try_recv().is_err(),
        "the long request is still pending"
    );
    // Dropping the client completes the long request instead of leaving it hanging.
    drop(client);
    assert_eq!(
        long.recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err(),
        ServiceError::Unavailable
    );
}

#[test]
fn handshake_without_welcome_times_out_instead_of_hanging() {
    let (connector, _, _) = fake(Script::NoWelcome);
    let timeouts = ClientTimeouts {
        handshake: Duration::from_millis(200),
        ..ClientTimeouts::default()
    };
    let client = IpcLibraryClient::with_timeouts(connector, timeouts).unwrap();
    let started = Instant::now();
    let reply = list(&client).recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(reply.unwrap_err(), ServiceError::Unavailable);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(client.last_error().is_some());
    // The blocking session uses the same bounded handshake.
    let (connector, _, _) = fake(Script::NoWelcome);
    let (mut stream, hello) = connector.connect().unwrap();
    assert!(handshake_within(stream.as_mut(), hello, Duration::from_millis(100)).is_err());
}

#[test]
fn handshake_closed_right_after_connect_is_retried_exactly_once() {
    let (connector, connects, _) = fake(Script::CloseFirst);
    let session = Session::open(&connector).unwrap();
    assert_eq!(session.welcome.daemon_version, "fake");
    assert_eq!(connects.load(Ordering::SeqCst), 2);

    let (connector, connects, _) = fake(Script::CloseAlways);
    assert!(Session::open(&connector).is_err());
    assert_eq!(connects.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_reconnects_back_off_instead_of_reconnecting_per_request() {
    let (connector, connects, _) = fake(Script::Refuse);
    let timeouts = ClientTimeouts {
        backoff_base: Duration::from_millis(800),
        ..ClientTimeouts::default()
    };
    let client = IpcLibraryClient::with_timeouts(connector, timeouts).unwrap();
    for _ in 0..5 {
        let reply = list(&client).recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(reply.unwrap_err(), ServiceError::Unavailable);
    }
    // First failure retried at once, second starts the backoff: later requests fail fast.
    assert_eq!(connects.load(Ordering::SeqCst), 2);
    thread::sleep(Duration::from_millis(900));
    let reply = list(&client).recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(reply.unwrap_err(), ServiceError::Unavailable);
    assert_eq!(connects.load(Ordering::SeqCst), 3);
}

#[test]
fn per_command_deadlines_are_generous_for_long_operations() {
    let t = ClientTimeouts::default();
    assert_eq!(t.for_command(&Command::Status), Duration::from_secs(30));
    assert_eq!(
        t.for_command(&Command::Library(
            LibraryRequest::List(PaperList::default())
        )),
        Duration::from_secs(300)
    );
    assert_eq!(
        t.for_command(&Command::Import(ImportRequestUi::Cancel(
            crate::import::PreviewId("p".into())
        ))),
        Duration::from_secs(3600)
    );
    assert_eq!(t.backoff(1), None);
    assert_eq!(t.backoff(2), Some(Duration::from_millis(250)));
    assert_eq!(t.backoff(40), Some(Duration::from_secs(10)));
}

/// A stream whose peer is already gone, reporting it the way macOS does: setting socket
/// options fails with EINVAL before any frame is written.
struct ClosedPeerQuirk;
impl io::Read for ClosedPeerQuirk {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}
impl io::Write for ClosedPeerQuirk {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Stream for ClosedPeerQuirk {
    fn try_clone_stream(&self) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(ClosedPeerQuirk))
    }
    fn shutdown_stream(&self) {}
    fn set_io_timeouts(&self, _: Option<Duration>, _: Option<Duration>) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(22))
    }
}

#[test]
fn transport_errors_after_connect_are_a_handshake_close_but_timeouts_are_not() {
    // The macOS EINVAL-from-setsockopt case, and the other ways a just-closed socket fails.
    let err =
        handshake_within(&mut ClosedPeerQuirk, hello(), Duration::from_millis(100)).unwrap_err();
    assert!(err.closed_during_handshake(), "{err:?}");
    for kind in [
        io::ErrorKind::NotConnected,
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::ConnectionReset,
        io::ErrorKind::UnexpectedEof,
    ] {
        assert!(
            ClientError::Io(kind.into()).closed_during_handshake(),
            "{kind:?}"
        );
    }
    for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
        assert!(
            !ClientError::Io(kind.into()).closed_during_handshake(),
            "{kind:?}"
        );
    }
    assert!(!ClientError::Protocol("expected welcome").closed_during_handshake());
}
