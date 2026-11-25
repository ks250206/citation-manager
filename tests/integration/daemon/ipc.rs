//! #25: GUI-less CRUD through the real IPC transport, handshake limits and endpoint permissions.
mod support;
use cm_daemon::ipc::start;
use cm_protocol::{
    client::{IpcLibraryClient, Session, handshake},
    ipc::{
        ClientFrame, Command, Credential, HandshakeRejection, MAX_HANDSHAKE_FRAME,
        PROTOCOL_VERSION, Reply, ServerFrame, read_frame, write_frame,
    },
    library::{LibraryClient, LibraryReply, LibraryRequest, NewPaper, PaperList, ServiceError},
    library_query::LibraryQuery,
    organization::{CollectionInput, OrganizationRequest},
    settings::Settings,
};
use std::{
    io::Write,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::mpsc,
    time::Duration,
};
use support::*;

fn wait<T: Send + 'static>(
    f: impl FnOnce(Box<dyn FnOnce(Result<T, ServiceError>) + Send>) -> Result<(), ServiceError>,
) -> Result<T, ServiceError> {
    let (tx, rx) = mpsc::channel();
    f(Box::new(move |r| tx.send(r).unwrap())).unwrap();
    rx.recv_timeout(Duration::from_secs(20)).unwrap()
}

#[test]
fn library_client_crud_over_ipc_survives_daemon_restart() {
    let env = Env::new();
    let daemon = start(env.config()).unwrap();
    let client = IpcLibraryClient::new(env.connector("desktop")).unwrap();
    let mut input = NewPaper::titled("固体電解質 🙂 over IPC");
    input.bibliography.doi = Some("https://doi.org/10.1000/IPC".into());
    let created = match wait(|c| client.submit(LibraryRequest::Create(Box::new(input.clone())), c))
        .unwrap()
    {
        LibraryReply::Paper(p) => *p,
        other => panic!("{other:?}"),
    };
    assert_eq!(created.bibliography.doi.as_deref(), Some("10.1000/ipc"));
    assert_eq!(
        wait(|c| client.submit(LibraryRequest::Create(Box::new(input)), c)),
        Err(ServiceError::Conflict)
    );
    let mut edited = created.clone();
    edited.title.push_str(" / 編集");
    wait(|c| client.submit(LibraryRequest::Update(Box::new(edited.clone())), c)).unwrap();
    let page = wait(|c| client.query(LibraryQuery::default(), c)).unwrap();
    assert_eq!(page.papers.len(), 1);
    assert_eq!(page.papers[0].title, edited.title);
    let organization = wait(|c| {
        client.organization(
            OrganizationRequest::CreateCollection(CollectionInput::named("Thesis")),
            c,
        )
    })
    .unwrap();
    assert_eq!(organization.collections.len(), 1);
    let settings = Settings {
        reading: true,
        ..Settings::default()
    };
    wait(|c| client.save_settings(settings.clone(), c)).unwrap();
    assert_eq!(
        wait(|c| client.load_settings(c)).unwrap().settings,
        settings
    );
    let trashed = wait(|c| client.lifecycle(created.id, true, c)).unwrap();
    assert!(trashed.deleted_at.is_some());
    wait(|c| client.lifecycle(created.id, false, c)).unwrap();

    // Daemon stops: in-flight/next requests fail as Unavailable instead of hanging.
    daemon.shutdown();
    assert_eq!(
        wait(|c| client.submit(LibraryRequest::List(PaperList::default()), c)),
        Err(ServiceError::Unavailable)
    );
    // A new daemon on the same library: the same client reconnects with its credential.
    let _daemon = start(env.config()).unwrap();
    match wait(|c| client.submit(LibraryRequest::Get(created.id), c)).unwrap() {
        LibraryReply::Paper(p) => assert_eq!(p.title, edited.title),
        other => panic!("{other:?}"),
    }
}

#[test]
fn endpoint_and_credential_files_are_private_and_unsafe_dirs_are_refused() {
    let env = Env::new();
    let daemon = start(env.config()).unwrap();
    let paths = env.paths();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode(&paths.runtime_dir), 0o700);
    assert_eq!(mode(&paths.state_dir), 0o700);
    assert_eq!(mode(&paths.socket), 0o600);
    assert_eq!(mode(&paths.desktop_credential), 0o600);
    assert_eq!(mode(&paths.lock), 0o600);
    // The credential is a 256-bit hex secret; only its hash is stored in the database.
    let secret = std::fs::read_to_string(&paths.desktop_credential).unwrap();
    assert_eq!(secret.len(), 64);
    let db = std::fs::read(env.library.join("library.sqlite")).unwrap();
    assert!(!db.windows(64).any(|w| w == secret.as_bytes()));

    std::fs::set_permissions(&paths.runtime_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = Session::open(&env.connector("desktop")).err().unwrap();
    assert!(error.to_string().contains("unsafe"), "{error}");
    std::fs::set_permissions(&paths.runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut session = env.desktop();
    assert!(matches!(
        ok(session.call(Command::Status)),
        Reply::Status(s) if s.pid == std::process::id()
    ));
    drop(daemon);
}

fn raw(env: &Env) -> UnixStream {
    UnixStream::connect(env.paths().socket).unwrap()
}
fn rejection(stream: &mut UnixStream) -> Option<HandshakeRejection> {
    match read_frame::<_, ServerFrame>(stream, 1 << 20) {
        Ok(Some(ServerFrame::Rejected(r))) => Some(r),
        _ => None,
    }
}

#[test]
fn handshake_rejects_unknown_credentials_versions_libraries_and_oversized_frames() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let good = env.connector("desktop").hello().unwrap();
    let mut hello = good.clone();
    hello.credential = Credential("0".repeat(64));
    let mut stream = raw(&env);
    assert!(matches!(
        handshake(&mut stream, hello).unwrap_err(),
        cm_protocol::client::ClientError::Rejected(HandshakeRejection::Unauthenticated)
    ));
    let mut hello = good.clone();
    hello.protocol_version = PROTOCOL_VERSION + 1;
    assert!(matches!(
        handshake(&mut raw(&env), hello).unwrap_err(),
        cm_protocol::client::ClientError::Rejected(HandshakeRejection::VersionMismatch {
            supported: PROTOCOL_VERSION
        })
    ));
    // #85: a protocol-1 client (base64 page bitmaps) learns which version the daemon speaks;
    // the rejection is a plain JSON frame it can still read.
    let mut hello = good.clone();
    hello.protocol_version = 1;
    let mut stream = raw(&env);
    write_frame(&mut stream, &ClientFrame::Hello(hello), MAX_HANDSHAKE_FRAME).unwrap();
    let mut head = [0u8; 5];
    std::io::Read::read_exact(&mut stream, &mut head).unwrap();
    assert_eq!(head[4], b'{');
    let mut body = vec![0u8; u32::from_be_bytes(head[..4].try_into().unwrap()) as usize - 1];
    std::io::Read::read_exact(&mut stream, &mut body).unwrap();
    body.insert(0, b'{');
    assert!(matches!(
        serde_json::from_slice::<ServerFrame>(&body).unwrap(),
        ServerFrame::Rejected(HandshakeRejection::VersionMismatch { supported: 2 })
    ));
    assert_eq!(PROTOCOL_VERSION, 2);
    let mut hello = good.clone();
    hello.library = Some(env.temp.path().join("other"));
    assert!(matches!(
        handshake(&mut raw(&env), hello).unwrap_err(),
        cm_protocol::client::ClientError::Rejected(HandshakeRejection::WrongLibrary)
    ));
    // A request before Hello is never executed.
    let mut stream = raw(&env);
    write_frame(
        &mut stream,
        &ClientFrame::Request {
            id: 1,
            command: Command::Status,
        },
        MAX_HANDSHAKE_FRAME,
    )
    .unwrap();
    assert_eq!(rejection(&mut stream), Some(HandshakeRejection::Malformed));
    // Nor is one in the binary frame form, before or after authentication.
    let binary_status = {
        let json = serde_json::to_vec(&ClientFrame::Request {
            id: 1,
            command: Command::Status,
        })
        .unwrap();
        let mut body = vec![0u8];
        body.extend_from_slice(&(json.len() as u32).to_be_bytes());
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&3u32.to_be_bytes());
        body.extend_from_slice(&json);
        body.extend_from_slice(b"xyz");
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        frame
    };
    let mut stream = raw(&env);
    stream.write_all(&binary_status).unwrap();
    assert_eq!(rejection(&mut stream), Some(HandshakeRejection::Malformed));
    let mut stream = raw(&env);
    handshake(&mut stream, good.clone()).unwrap();
    // An unreferenced payload makes the frame malformed: the connection closes unanswered.
    stream.write_all(&binary_status).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    assert!(matches!(
        read_frame::<_, ServerFrame>(&mut stream, 1 << 20),
        Ok(None) | Err(_)
    ));
    // A declared pre-auth frame over the handshake limit is refused without reading it.
    let mut stream = raw(&env);
    stream
        .write_all(&(MAX_HANDSHAKE_FRAME + 1).to_be_bytes())
        .unwrap();
    assert_eq!(rejection(&mut stream), Some(HandshakeRejection::Malformed));
    // Silence: the bounded handshake timeout closes the connection.
    let mut idle = raw(&env);
    idle.set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    assert_eq!(rejection(&mut idle), Some(HandshakeRejection::Malformed));
    // The good credential still works.
    let mut session = Session::from_stream(Box::new(raw(&env)), good).unwrap();
    assert!(matches!(
        ok(session.call(Command::Status)),
        Reply::Status(_)
    ));
}

#[test]
fn authenticated_session_is_bounded_and_oversized_requests_close_it() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut stream = raw(&env);
    let welcome = handshake(&mut stream, env.connector("desktop").hello().unwrap()).unwrap();
    assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
    stream
        .write_all(&(welcome.max_request_frame + 1).to_be_bytes())
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    assert!(matches!(
        read_frame::<_, ServerFrame>(&mut stream, 1 << 20),
        Ok(None) | Err(_)
    ));
    assert_eq!(
        format!("{:?}", Credential("secret".into())),
        "Credential(<redacted>)"
    );
}

/// A client that keeps sending requests but never reads its replies cannot make the daemon
/// queue replies without bound: once its bounded outbound queue is full it is disconnected
/// (its writes start failing), and other clients are unaffected.
#[test]
fn a_client_that_stops_reading_replies_is_disconnected() {
    let env = Env::new();
    let _daemon = start(env.config()).unwrap();
    let mut stream = raw(&env);
    let welcome = handshake(&mut stream, env.connector("desktop").hello().unwrap()).unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    // Far more replies than the queue plus both socket buffers can hold.
    const REQUESTS: u64 = 50_000;
    let sent = (0..REQUESTS)
        .take_while(|id| {
            write_frame(
                &mut stream,
                &ClientFrame::Request {
                    id: *id,
                    command: Command::Status,
                },
                welcome.max_request_frame,
            )
            .is_ok()
        })
        .count() as u64;
    assert!(
        sent < REQUESTS,
        "all {sent} requests were accepted although no reply was read"
    );
    let mut session = env.desktop();
    assert!(matches!(
        ok(session.call(Command::Status)),
        Reply::Status(_)
    ));
}
