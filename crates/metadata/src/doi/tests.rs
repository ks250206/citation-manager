use super::*;
use cm_core::metadata_resolve::MetadataResolveService;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::UNIX_EPOCH,
};
#[derive(Default)]
struct Clock {
    now: RefCell<Duration>,
    waits: RefCell<Vec<Duration>>,
}
impl WorkerClock for Clock {
    fn now(&self) -> Duration {
        *self.now.borrow()
    }
    fn unix_time(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000) + self.now()
    }
    fn wait(&self, d: Duration, c: &Cancellation) -> bool {
        self.waits.borrow_mut().push(d);
        *self.now.borrow_mut() += d;
        !c.is_cancelled()
    }
}
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    stall: bool,
    close: bool,
    cancel: Option<Cancellation>,
}
impl Reply {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&value).unwrap(),
            stall: false,
            close: false,
            cancel: None,
        }
    }
    fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }
}
struct Server {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    done: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(vec![]));
        let captured = requests.clone();
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let task = thread::spawn(move || {
            let mut replies = replies.into_iter();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        // Accepted streams can inherit O_NONBLOCK on macOS. A readiness
                        // race must not turn an incomplete request into a fixture lookup.
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(200)))
                            .unwrap();
                        let mut request = vec![];
                        let mut chunk = [0u8; 1024];
                        while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
                            match socket.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => request.extend_from_slice(&chunk[..n]),
                            }
                        }
                        if !request.ends_with(b"\r\n\r\n") {
                            continue;
                        }
                        captured
                            .lock()
                            .unwrap()
                            .push(String::from_utf8(request).unwrap());
                        let Some(reply) = replies.next() else {
                            continue;
                        };
                        if let Some(c) = reply.cancel {
                            c.cancel();
                        }
                        if reply.stall {
                            let start = Instant::now();
                            while !stop.load(Ordering::Relaxed)
                                && start.elapsed() < Duration::from_millis(150)
                            {
                                thread::park_timeout(Duration::from_millis(2));
                            }
                            continue;
                        }
                        if reply.close {
                            continue;
                        }
                        let mut headers =
                            format!("HTTP/1.1 {} mock\r\nConnection: close\r\n", reply.status);
                        for (k, v) in &reply.headers {
                            headers += &format!("{k}: {v}\r\n");
                        }
                        // `chunked` fixtures test the stream budget without Content-Length.
                        if reply.headers.iter().any(|(k, _)| k == "Transfer-Encoding") {
                            headers += "\r\n";
                            let mut wire = headers.into_bytes();
                            wire.extend_from_slice(
                                format!("{:x}\r\n", reply.body.len()).as_bytes(),
                            );
                            wire.extend_from_slice(&reply.body);
                            wire.extend_from_slice(b"\r\n0\r\n\r\n");
                            let _ = socket.write_all(&wire);
                        } else {
                            let body = if reply.status == 200 {
                                &reply.body[..]
                            } else {
                                &[]
                            };
                            headers += &format!("Content-Length: {}\r\n\r\n", body.len());
                            let mut wire = headers.into_bytes();
                            wire.extend_from_slice(body);
                            let _ = socket.write_all(&wire);
                        }
                        let _ = socket.shutdown(std::net::Shutdown::Write);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(1))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            base,
            requests,
            done,
            task: Some(task),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    fn client(&self) -> HttpProviders<Clock> {
        let mut client =
            HttpProviders::with_clock(HttpConfig::default(), Clock::default()).unwrap();
        client.endpoints = Some([
            format!("{}/works", self.base),
            format!("{}/dois", self.base),
        ]);
        client
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        self.task.take().unwrap().join().unwrap();
    }
}
fn cr(doi: &str) -> Value {
    json!({"status":"ok","message-type":"work","message":{
    "DOI":doi,"type":"journal-article","title":[" Ω  日本語 title "],
    "author":[{"given":"Zoë","family":"Ångström","ORCID":"https://orcid.org/0000-0001-2345-6789"},{"name":"研究機構"}],"editor":[{"given":"É","family":"Editor"}],
    "issued":{"date-parts":[[2024,2,29]]},"container-title":["Journal Ω"],"volume":"8","issue":"2","page":"S1--S12","abstract":"<jats:p>Abstract</jats:p>","URL":"https://publisher.example.org/article","language":"ja",
    "link":[{"URL":"https://publisher.example.org/paper.pdf","content-type":"application/pdf"}],"publisher":"Publisher","subject":["材料"]}})
}
fn dc(doi: &str) -> Value {
    json!({"data":{"id":doi,"type":"dois","attributes":{
    "doi":doi,"titles":[{"title":"DataCite title"}],"creators":[{"name":"Ångström, Zoë","givenName":"Zoë","familyName":"Ångström","nameIdentifiers":[{"nameIdentifierScheme":"ORCID","nameIdentifier":"https://orcid.org/0000-0001-2345-6789"}]},{"name":"研究機構","nameType":"Organizational"}],
    "contributors":[{"name":"Editor, É","givenName":"É","familyName":"Editor","contributorType":"Editor"},{"name":"Data team","contributorType":"DataCurator","nameType":"Organizational"}],
    "publicationYear":2024,"dates":[{"date":"2024-02","dateType":"Issued"}],"types":{"resourceTypeGeneral":"JournalArticle"},"container":{"title":"Journal DC","volume":"9","issue":"3","firstPage":"10","lastPage":"20"},
    "descriptions":[{"description":"DC abstract","descriptionType":"Abstract"}],"url":"https://data.example.org/record","language":"en","publisher":"Archive",
    "identifiers":[{"identifier":"00042","identifierType":"PMID"},{"identifier":"https://doi.org/10.99/TEST","identifierType":"DOI"},{"identifier":"arXiv:2401.00001v2","identifierType":"arXiv"}],"contentUrl":["https://data.example.org/paper.pdf","file:///bad.pdf","http://127.0.0.1/bad.pdf","https://data.example.org/record"]}}})
}
fn lookup(client: &mut HttpProviders<Clock>) -> ProviderReply {
    client.lookup(Provider::Crossref, "10.99/test", &Cancellation::default())
}
#[test]
fn crossref_full_success_skips_datacite_preserves_unicode_and_common_normalization() {
    let server = Server::start(vec![Reply::json(200, cr("10.99/TEST"))]);
    let mut service = MetadataResolveService::new(server.client());
    let r = service
        .resolve("https://doi.org/10.99/TEST?x=y", &Cancellation::default())
        .unwrap();
    assert!(!r.manual_add);
    assert_eq!(r.outcomes[1].status, ProviderStatus::NotNeeded);
    assert_eq!(server.count(), 1);
    let d = cm_core::import::normalize::normalize(&r.draft).unwrap();
    assert_eq!(d.title, "Ω 日本語 title");
    let b = &d.bibliography;
    assert_eq!(b.year, Some(2024));
    assert_eq!(
        (
            b.paper_type.as_deref(),
            b.journal.as_deref(),
            b.volume.as_deref(),
            b.issue.as_deref(),
            b.language.as_deref()
        ),
        (
            Some("journal-article"),
            Some("Journal Ω"),
            Some("8"),
            Some("2"),
            Some("ja")
        )
    );
    assert_eq!(
        b.abstract_text.as_deref(),
        Some("<jats:p>Abstract</jats:p>")
    );
    assert_eq!(
        b.url.as_deref(),
        Some("https://publisher.example.org/article")
    );
    assert_eq!(d.bibliography.pages.as_deref(), Some("S1–S12"));
    assert_eq!(
        d.bibliography.publication_date.as_deref(),
        Some("2024-02-29")
    );
    assert_eq!(
        d.bibliography
            .authors
            .iter()
            .map(|a| a.role.as_str())
            .collect::<Vec<_>>(),
        vec!["author", "author", "editor"]
    );
    assert_eq!(d.bibliography.authors[0].given_name.as_deref(), Some("Zoë"));
    assert_eq!(
        d.bibliography.authors[1].literal_name.as_deref(),
        Some("研究機構")
    );
    assert_eq!(r.draft.external_ids[0].value, "10.99/TEST");
    assert!(
        r.draft
            .bibliography
            .extra_json
            .unwrap()
            .to_string()
            .contains("Publisher")
    );
    assert_eq!(r.pdf_candidates[0].source.adapter, "crossref");
    use sha2::Digest;
    assert_eq!(
        r.pdf_candidates[0].source.sha256,
        Some(sha2::Sha256::digest(serde_json::to_vec(&cr("10.99/TEST")).unwrap()).into())
    );
    let requests = server.requests.lock().unwrap();
    let wire = requests[0].to_ascii_lowercase();
    assert!(
        wire.contains("get /works/10.99%2ftest http/1.1"),
        "requests: {:?}",
        *requests
    );
    assert!(wire.contains(
        "user-agent: citation-manager/0.1 (+https://github.com/ks250206/citation-manager)"
    ));
    assert!(!wire.contains("authorization:"));
}
#[test]
fn datacite_complements_only_absent_fields_and_keeps_conflicts_with_provenance() {
    let mut crossref = cr("10.99/test");
    crossref["message"]
        .as_object_mut()
        .unwrap()
        .remove("abstract");
    let server = Server::start(vec![
        Reply::json(200, crossref),
        Reply::json(200, dc("10.99/test")),
    ]);
    let r = MetadataResolveService::new(server.client())
        .resolve("doi:10.99/test", &Cancellation::default())
        .unwrap();
    assert_eq!(server.count(), 2);
    assert_eq!(
        r.draft.bibliography.abstract_text.as_deref(),
        Some("DC abstract")
    );
    assert_eq!(r.draft.bibliography.volume.as_deref(), Some("8"));
    assert_eq!(r.draft.bibliography.authors.len(), 3);
    assert_eq!(r.pdf_candidates.len(), 2);
    assert!(
        r.fields
            .iter()
            .any(|f| f.field == "title" && f.provider == Provider::DataCite && !f.selected)
    );
    assert!(
        r.fields
            .iter()
            .any(|f| f.field == "abstract" && f.provider == Provider::DataCite && f.selected)
    );
    assert!(
        r.draft
            .external_ids
            .iter()
            .any(|i| i.namespace == "PMID" && i.value == "00042")
    );
    assert!(
        r.draft
            .bibliography
            .extra_json
            .unwrap()
            .to_string()
            .contains("https://doi.org/10.99/TEST")
    );
}
#[test]
fn fallback_after_not_found_maps_full_datacite_authors_roles_precision_and_ids() {
    let server = Server::start(vec![
        Reply::json(404, json!({})),
        Reply::json(200, dc("10.99/test")),
    ]);
    let r = MetadataResolveService::new(server.client())
        .resolve("10.99/test", &Cancellation::default())
        .unwrap();
    assert_eq!(r.outcomes[0].status, ProviderStatus::NotFound);
    assert_eq!(r.outcomes[1].status, ProviderStatus::Found);
    let d = cm_core::import::normalize::normalize(&r.draft).unwrap();
    assert_eq!(
        d.bibliography
            .authors
            .iter()
            .map(|a| a.role.as_str())
            .collect::<Vec<_>>(),
        vec!["author", "author", "editor", "datacurator"]
    );
    assert_eq!(d.bibliography.publication_date.as_deref(), Some("2024-02"));
    let b = &d.bibliography;
    assert_eq!(b.year, Some(2024));
    assert_eq!(
        (
            b.paper_type.as_deref(),
            b.journal.as_deref(),
            b.volume.as_deref(),
            b.issue.as_deref(),
            b.language.as_deref()
        ),
        (
            Some("journal-article"),
            Some("Journal DC"),
            Some("9"),
            Some("3"),
            Some("en")
        )
    );
    assert_eq!(b.url.as_deref(), Some("https://data.example.org/record"));
    assert!(
        d.external_ids
            .iter()
            .any(|i| i.namespace == "arxiv" && i.value == "2401.00001v2")
    );
    assert_eq!(
        d.bibliography.authors[0].orcid.as_deref(),
        Some("https://orcid.org/0000-0001-2345-6789")
    );
    assert_eq!(d.external_ids[1].value, "42");
    assert_eq!(d.bibliography.pages.as_deref(), Some("10–20"));
    assert_eq!(r.pdf_candidates.len(), 1);
}
#[test]
fn unregistered_returns_editable_manual_draft_and_explicit_diagnostics() {
    let server = Server::start(vec![
        Reply::json(404, json!({})),
        Reply::json(404, json!({})),
    ]);
    let r = MetadataResolveService::new(server.client())
        .resolve("10.99/test", &Cancellation::default())
        .unwrap();
    assert!(r.manual_add);
    assert!(r.draft.title.is_empty());
    assert_eq!(r.draft.diagnostics.len(), 2);
    let mut d = r.draft;
    d.title = "Manual Ω".into();
    assert!(cm_core::import::normalize::normalize(&d).is_ok());
}
#[test]
fn rate_limit_backoff_http_dates_and_advertised_rate_are_deterministic() {
    let server = Server::start(vec![
        Reply::json(429, json!({})).header("Retry-After", "1"),
        Reply::json(200, cr("10.99/test"))
            .header("X-Rate-Limit-Limit", "2")
            .header("X-Rate-Limit-Interval", "1s"),
        Reply::json(404, json!({})),
    ]);
    let mut client = server.client();
    let r = lookup(&mut client);
    assert_eq!(r.outcome.status, ProviderStatus::Found);
    assert_eq!(r.outcome.attempts, 2);
    assert!(
        client
            .clock
            .waits
            .borrow()
            .contains(&Duration::from_secs(1))
    );
    client.lookup(
        Provider::Crossref,
        "10.99/another",
        &Cancellation::default(),
    );
    assert!(
        client
            .clock
            .waits
            .borrow()
            .contains(&Duration::from_millis(500))
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        "retry-after",
        httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(1_700_000_002))
            .parse()
            .unwrap(),
    );
    assert_eq!(
        retry_after(&headers, UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        Some(Duration::from_secs(2))
    );
}
#[test]
fn long_retry_after_is_not_shortened_and_suppresses_later_fetches() {
    let server = Server::start(vec![
        Reply::json(429, json!({})).header("Retry-After", "120"),
    ]);
    let mut client = server.client();
    let r = lookup(&mut client);
    assert_eq!(r.outcome.status, ProviderStatus::RateLimited);
    assert_eq!(r.outcome.retry_after_seconds, Some(120));
    assert_eq!(lookup(&mut client).outcome.attempts, 0);
    assert_eq!(server.count(), 1);
}
#[test]
fn cache_canonical_key_provider_isolation_ttl_negative_ttl_and_eviction() {
    let server = Server::start(vec![
        Reply::json(200, cr("10.99/test")),
        Reply::json(404, json!({})),
        Reply::json(200, cr("10.99/test")),
        Reply::json(404, json!({})),
        Reply::json(404, json!({})),
    ]);
    let mut client = server.client();
    client.config.cache_entries = 2;
    let first = lookup(&mut client);
    assert_eq!(first.outcome.status, ProviderStatus::Found);
    assert!(!first.outcome.cached);
    assert!(lookup(&mut client).outcome.cached);
    let c = Cancellation::default();
    let negative = client.lookup(Provider::DataCite, "10.99/test", &c);
    assert_eq!(negative.outcome.status, ProviderStatus::NotFound);
    assert!(!negative.outcome.cached);
    assert!(
        client
            .lookup(Provider::DataCite, "10.99/test", &c)
            .outcome
            .cached
    );
    *client.clock.now.borrow_mut() += Duration::from_secs(601);
    assert!(!lookup(&mut client).outcome.cached);
    client.lookup(Provider::Crossref, "10.99/other", &c);
    client.lookup(Provider::Crossref, "10.99/third", &c);
    assert_eq!(client.cache.len(), 2);
    assert!(
        !client
            .cache
            .contains_key(&(Provider::Crossref, "10.99/test".into()))
    );
    assert_eq!(server.count(), 5);
}
#[test]
fn response_failures_are_distinct_and_never_cached_or_redirected() {
    for (reply, status) in [
        (
            Reply::json(200, json!({"bad":true})),
            ProviderStatus::InvalidResponse,
        ),
        (
            Reply::json(200, cr("10.99/wrong")),
            ProviderStatus::IdentityMismatch,
        ),
        (
            Reply::json(302, json!({})).header("Location", "http://127.0.0.1:1/private"),
            ProviderStatus::RedirectRejected,
        ),
        (Reply::json(401, json!({})), ProviderStatus::Http(401)),
    ] {
        let server = Server::start(vec![reply]);
        let mut client = server.client();
        assert_eq!(lookup(&mut client).outcome.status, status);
        assert!(client.cache.is_empty());
        assert_eq!(server.count(), 1);
    }
    let mut malformed = Reply::json(200, json!({}));
    malformed.body = b"{not json".to_vec();
    let server = Server::start(vec![malformed]);
    assert_eq!(
        lookup(&mut server.client()).outcome.status,
        ProviderStatus::InvalidResponse
    );
}
#[test]
fn stream_and_declared_length_body_limits_reject_without_unbounded_reads() {
    for chunked in [false, true] {
        let mut reply = Reply::json(200, json!({"large":"x".repeat(2000)}));
        if chunked {
            reply = reply.header("Transfer-Encoding", "chunked");
        }
        let server = Server::start(vec![reply]);
        let mut client = server.client();
        client.config.body_bytes = 100;
        assert_eq!(lookup(&mut client).outcome.status, ProviderStatus::TooLarge);
        assert!(client.cache.is_empty());
    }
}
#[test]
fn timeouts_network_and_cancellation_are_bounded_and_retry_only_transient_failures() {
    let mut stall = Reply::json(200, json!({}));
    stall.stall = true;
    let server = Server::start(vec![stall]);
    // Client deadline was set at construction, so build a short-deadline transport.
    let mut client = HttpProviders::with_clock(
        HttpConfig {
            timeout: Duration::from_millis(40),
            attempts: 1,
            ..Default::default()
        },
        Clock::default(),
    )
    .unwrap();
    client.endpoints = Some([
        format!("{}/works", server.base),
        format!("{}/dois", server.base),
    ]);
    let start = Instant::now();
    assert_eq!(lookup(&mut client).outcome.status, ProviderStatus::Timeout);
    assert!(start.elapsed() < Duration::from_secs(2));
    let mut close = Reply::json(200, json!({}));
    close.close = true;
    let server = Server::start(vec![close, Reply::json(200, cr("10.99/test"))]);
    let mut client = server.client();
    assert_eq!(lookup(&mut client).outcome.attempts, 2);
    assert_eq!(server.count(), 2);
    assert!(
        client
            .clock
            .waits
            .borrow()
            .contains(&Duration::from_millis(250))
    );
    let cancel = Cancellation::default();
    let mut reply = Reply::json(200, json!({}));
    reply.stall = true;
    reply.cancel = Some(cancel.clone());
    let server = Server::start(vec![reply]);
    let mut client = server.client();
    assert_eq!(
        client
            .lookup(Provider::Crossref, "10.99/test", &cancel)
            .outcome
            .status,
        ProviderStatus::Cancelled
    );
    assert!(client.cache.is_empty());
    assert_eq!(
        client
            .lookup(Provider::Crossref, "10.99/test", &cancel)
            .outcome
            .attempts,
        0
    );
    assert_eq!(server.count(), 1);
}
#[test]
fn safe_endpoint_building_and_candidate_urls_cannot_trigger_requests() {
    let server = Server::start(vec![]);
    let client = server.client();
    let url = client.endpoint(Provider::Crossref, "10.99/a?x=#fragment/%2f");
    assert_eq!(url.query(), None);
    assert_eq!(url.fragment(), None);
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert!(url.path().contains("%3F"));
    assert!(url.path().contains("%252f"));
    for url in [
        "file:///tmp/a.pdf",
        "javascript:alert(1)",
        "ftp://example.org/a.pdf",
        "https://user:pass@example.org/a.pdf",
        "http://127.1/a.pdf",
        "http://[::1]/a.pdf",
        "http://[::ffff:127.0.0.1]/a.pdf",
        "https://localhost/a.pdf",
        "http://10.0.0.1/a.pdf",
        "https://example.org:9999/a.pdf",
    ] {
        assert!(crate::providers::public_url(url).is_none(), "{url}");
    }
    assert!(crate::providers::public_url("https://publisher.example.org/a.pdf").is_some());
    assert_eq!(server.count(), 0);
}
#[test]
fn schema_and_record_budgets_reject_wrong_types_dates_and_identity_conflicts() {
    let mut wrong_type = cr("10.99/test");
    wrong_type["message"]["title"] = json!("not array");
    let mut wrong_date = cr("10.99/test");
    wrong_date["message"]["issued"] = json!({"date-parts":[[2023,2,29]]});
    let mut many = cr("10.99/test");
    many["message"]["author"] = json!((0..257).map(|_| json!({"name":"n"})).collect::<Vec<_>>());
    for value in [wrong_type, wrong_date, many] {
        let server = Server::start(vec![Reply::json(200, value)]);
        assert_eq!(
            lookup(&mut server.client()).outcome.status,
            ProviderStatus::InvalidResponse
        );
    }
    let mut value = dc("10.99/test");
    value["data"]["attributes"]["identifiers"][1]["identifier"] = json!("10.99/wrong");
    let server = Server::start(vec![Reply::json(200, value)]);
    assert_eq!(
        server
            .client()
            .lookup(Provider::DataCite, "10.99/test", &Cancellation::default())
            .outcome
            .status,
        ProviderStatus::IdentityMismatch
    );
}

#[test]
fn cache_expiry_negative_ttl_canonical_wrappers_and_failure_retry_are_explicit() {
    let server = Server::start(vec![
        Reply::json(404, json!({})),
        Reply::json(404, json!({})),
        Reply::json(200, cr("10.99/test")),
    ]);
    let mut client = server.client();
    assert_eq!(lookup(&mut client).outcome.status, ProviderStatus::NotFound);
    *client.clock.now.borrow_mut() += Duration::from_secs(29);
    assert!(lookup(&mut client).outcome.cached);
    *client.clock.now.borrow_mut() += Duration::from_secs(1);
    assert!(!lookup(&mut client).outcome.cached);
    assert_eq!(server.count(), 2);
    let mut service = MetadataResolveService::new(client);
    // The Crossref negative cache is intentionally still live; DataCite's distinct cache
    // does not alias it. A separate complete fixture exercises canonical input reuse.
    let full = Server::start(vec![Reply::json(200, cr("10.99/test"))]);
    let mut full_service = MetadataResolveService::new(full.client());
    full_service
        .resolve("https://doi.org/10.99/TEST", &Cancellation::default())
        .unwrap();
    assert!(
        full_service
            .resolve("doi:10.99/test", &Cancellation::default())
            .unwrap()
            .outcomes[0]
            .cached
    );
    assert_eq!(full.count(), 1);
    let c = Cancellation::default();
    c.cancel();
    assert!(
        service
            .resolve("10.99/test", &c)
            .unwrap()
            .outcomes
            .iter()
            .all(|o| o.status == ProviderStatus::Cancelled)
    );
}
#[test]
fn persistent_network_failure_and_invalid_json_fall_back_without_caching_errors() {
    // Keep ownership of the port: dropping a listener before lookup can race another
    // parallel server reusing that port and consume its fixture responses.
    let offline = Server::start(
        (0..4)
            .map(|_| {
                let mut reply = Reply::json(200, json!({}));
                reply.close = true;
                reply
            })
            .collect(),
    );
    let r = MetadataResolveService::new(offline.client())
        .resolve("10.99/test", &Cancellation::default())
        .unwrap();
    assert!(r.manual_add);
    assert!(
        r.outcomes
            .iter()
            .all(|o| o.status == ProviderStatus::Network && o.attempts == 2)
    );
    let server = Server::start(vec![
        Reply::json(200, json!({"invalid":true})),
        Reply::json(200, dc("10.99/test")),
    ]);
    let r = MetadataResolveService::new(server.client())
        .resolve("10.99/test", &Cancellation::default())
        .unwrap();
    assert!(!r.manual_add);
    assert_eq!(r.outcomes[0].status, ProviderStatus::InvalidResponse);
}
#[test]
fn retry_after_http_date_and_exhausted_server_errors_have_bounded_attempts() {
    let date = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(1_700_000_001));
    let server = Server::start(vec![
        Reply::json(429, json!({})).header("Retry-After", &date),
        Reply::json(200, cr("10.99/test")),
    ]);
    let mut client = server.client();
    assert_eq!(lookup(&mut client).outcome.status, ProviderStatus::Found);
    assert!(
        client
            .clock
            .waits
            .borrow()
            .contains(&Duration::from_secs(1))
    );
    let server = Server::start(vec![
        Reply::json(500, json!({})),
        Reply::json(500, json!({})),
    ]);
    let mut client = server.client();
    let r = lookup(&mut client);
    assert_eq!(r.outcome.status, ProviderStatus::Http(500));
    assert_eq!(r.outcome.attempts, 2);
    assert_eq!(server.count(), 2);
    assert!(client.cache.is_empty());
}
#[test]
fn shared_normalizer_preserves_raw_percent_identity_and_rejects_provider_substitution() {
    let server = Server::start(vec![
        Reply::json(200, cr("10.99/a/b")),
        Reply::json(200, cr("10.99/a%2fb")),
    ]);
    let mut client = server.client();
    assert_eq!(
        client
            .lookup(Provider::Crossref, "10.99/a%2fb", &Cancellation::default())
            .outcome
            .status,
        ProviderStatus::IdentityMismatch
    );
    assert_eq!(
        client
            .lookup(Provider::Crossref, "10.99/a%2fb", &Cancellation::default())
            .outcome
            .status,
        ProviderStatus::Found
    );
    let wires = server.requests.lock().unwrap();
    assert!(wires[0].contains("a%252fb"), "requests: {:?}", *wires);
    drop(wires);
    let mut data = dc("10.99/test");
    data["data"]["id"] = json!("10.99/other");
    let server = Server::start(vec![Reply::json(200, data)]);
    assert_eq!(
        server
            .client()
            .lookup(Provider::DataCite, "10.99/test", &Cancellation::default())
            .outcome
            .status,
        ProviderStatus::IdentityMismatch
    );
}
#[test]
fn config_bounds_and_async_context_are_rejected_without_fetching() {
    for config in [
        HttpConfig {
            attempts: 4,
            ..Default::default()
        },
        HttpConfig {
            body_bytes: 1024 * 1024,
            ..Default::default()
        },
        HttpConfig {
            cache_entries: 65,
            ..Default::default()
        },
        HttpConfig {
            user_agent: "anonymous".into(),
            ..Default::default()
        },
        HttpConfig {
            timeout: Duration::from_secs(11),
            ..Default::default()
        },
    ] {
        assert!(HttpProviders::new(config).is_err());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert!(HttpProviders::new(Default::default()).is_err());
    });
}
