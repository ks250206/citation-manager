//! #30 hostile IPC input against the real `papersd` executable (deterministic seeded
//! mutation, no new dependency). Random bytes, bogus length prefixes, mutated Hello frames
//! and mutated/unauthorized request frames from a read-only grant must never crash the
//! daemon, never authenticate a mutated credential and never perform a write. Afterwards
//! the same daemon process still serves the owner with the library unchanged.
mod support;
use cm_protocol::{
    client::Session,
    ipc::{
        Capability, CapabilitySet, ClientFrame, Command, GrantCommand, GrantReply, Reply,
        ServerFrame, WireError, read_frame,
    },
    library::{LibraryRequest, NewPaper},
    library_query::LibraryQuery,
    organization::{CollectionInput, OrganizationRequest},
};
use std::{
    io::Write,
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};
use support::*;

/// xorshift64*: reproducible; failing seeds are printed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}
const SPLICES: &[&[u8]] = &[
    b"\"desktop\"",
    b"\"GrantAdmin\"",
    b"{\"Library\":{\"Create\":",
    b"null",
    b"[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[",
    b"\\u0000",
    b"99999999999999999999999",
    b"\xff\xfe",
];
fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(out.len() + 1);
        match rng.below(5) {
            0 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] = rng.next() as u8;
            }
            1 if at < out.len() => {
                let end = (at + 1 + rng.below(16)).min(out.len());
                out.drain(at..end);
            }
            2 => out.insert(at, rng.next() as u8),
            3 => out.truncate(at),
            _ => {
                let splice = SPLICES[rng.below(SPLICES.len())];
                out.splice(at..at, splice.iter().copied());
            }
        }
    }
    out
}
fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = (body.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(body);
    frame
}
fn count(session: &mut Session) -> usize {
    match ok(session.call(Command::Query(LibraryQuery {
        limit: 500,
        ..Default::default()
    }))) {
        Reply::Page(page) => page.papers.len(),
        other => panic!("{other:?}"),
    }
}
fn grants(session: &mut Session) -> usize {
    match ok(session.call(Command::Grants(GrantCommand::List))) {
        Reply::Grants(GrantReply::Grants(list)) => list.len(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn mutated_frames_never_crash_papersd_authenticate_or_write() {
    let env = Env::new();
    let mut daemon = env.spawn_papersd(120_000);
    let mut owner = None;
    assert!(wait_until(Duration::from_secs(20), || {
        owner = Session::open(&env.connector("desktop")).ok();
        owner.is_some()
    }));
    let mut owner = owner.unwrap();
    ok(owner.call(Command::Library(LibraryRequest::Create(Box::new(
        NewPaper::titled("Hostile baseline"),
    )))));
    let reader = issue(
        &mut owner,
        "read-only",
        [Capability::LibraryRead]
            .into_iter()
            .collect::<CapabilitySet>(),
    );
    let (papers, grant_count) = (count(&mut owner), grants(&mut owner));
    let hello = env.connector_with("mcp", reader).hello().unwrap();
    let hello_json = serde_json::to_vec(&ClientFrame::Hello(hello.clone())).unwrap();
    // Writes and admin operations a read-only grant must never perform.
    let writes: Vec<Vec<u8>> = [
        Command::Library(LibraryRequest::Create(Box::new(NewPaper::titled("ATTACK")))),
        Command::Organization(OrganizationRequest::CreateCollection(
            CollectionInput::named("ATTACK"),
        )),
        Command::Grants(GrantCommand::Issue {
            label: "ATTACK".into(),
            capabilities: [Capability::GrantAdmin].into_iter().collect(),
            expires_at_ms: None,
        }),
    ]
    .into_iter()
    .map(|command| serde_json::to_vec(&ClientFrame::Request { id: 7, command }).unwrap())
    .collect();
    let mut outcomes = [0usize; 4]; // rejected/closed, welcomed, denied, other
    let started = Instant::now();
    for round in 0..240u64 {
        let seed = 0x3000_0000 + round;
        let mut rng = Rng(seed);
        let mut stream = UnixStream::connect(&env.paths().socket)
            .unwrap_or_else(|e| panic!("seed {seed}: daemon unreachable: {e}"));
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mode = rng.below(4);
        let first = match mode {
            0 => {
                let n = rng.below(48);
                rng.bytes(n)
            }
            1 => {
                let mut frame = (rng.next() as u32 % 4096).to_be_bytes().to_vec();
                let n = rng.below(64);
                frame.extend(rng.bytes(n));
                frame
            }
            2 => framed(&mutate(&mut rng, &hello_json)),
            _ => framed(&hello_json),
        };
        let _ = stream.write_all(&first);
        if mode < 2 {
            // Unparseable pre-auth input: the daemon answers Rejected or closes.
            drop(stream);
            outcomes[0] += 1;
            continue;
        }
        match read_frame::<_, ServerFrame>(&mut stream, 1 << 20) {
            Ok(Some(ServerFrame::Welcome(_))) => outcomes[1] += 1,
            _ => {
                outcomes[0] += 1;
                continue;
            }
        }
        // Authenticated as the read-only grant: send a mutated or verbatim write request.
        let template = &writes[rng.below(writes.len())];
        let body = if rng.below(2) == 0 {
            template.clone()
        } else {
            mutate(&mut rng, template)
        };
        let _ = stream.write_all(&framed(&body));
        match read_frame::<_, ServerFrame>(&mut stream, 1 << 24) {
            Ok(Some(ServerFrame::Response { result, .. })) => match result {
                Err(WireError::Denied(_)) => outcomes[2] += 1,
                Ok(reply) => {
                    // Only a mutation into a read command may succeed.
                    assert!(
                        matches!(reply, Reply::Status(_) | Reply::Page(_) | Reply::Library(_)),
                        "seed {seed}: unexpected success {reply:?}"
                    );
                    outcomes[3] += 1;
                }
                Err(_) => outcomes[3] += 1,
            },
            _ => outcomes[3] += 1,
        }
    }
    assert!(started.elapsed() < Duration::from_secs(120));
    // The same process still runs and serves the owner; nothing was written.
    assert!(
        daemon.try_wait().unwrap().is_none(),
        "papersd exited under hostile input"
    );
    let mut owner = Session::open(&env.connector("desktop")).unwrap();
    assert_eq!(count(&mut owner), papers, "no write got through");
    assert_eq!(grants(&mut owner), grant_count, "no grant was issued");
    match ok(owner.call(Command::Organization(OrganizationRequest::List))) {
        Reply::Organization(org) => {
            assert!(!format!("{org:?}").contains("ATTACK"));
        }
        other => panic!("{other:?}"),
    }
    // Every class was exercised: rejected input, authenticated sessions and denials.
    assert!(
        outcomes[0] > 0 && outcomes[1] > 0 && outcomes[2] > 0,
        "{outcomes:?}"
    );
    eprintln!("outcomes (rejected/closed, welcomed, denied, other): {outcomes:?}");
    daemon.kill().unwrap();
    daemon.wait().unwrap();
}
