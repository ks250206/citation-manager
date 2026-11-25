use super::test_server::{Response, TestServer};
use super::*;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Maps fixture host names to addresses; everything else fails to resolve.
struct Names(HashMap<&'static str, Vec<IpAddr>>);
impl HostResolver for Names {
    fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        self.0
            .get(host)
            .cloned()
            .ok_or_else(|| std::io::Error::other("unknown fixture host"))
    }
}
fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}
fn fetcher() -> PdfFetcher {
    PdfFetcher::with_resolver(
        FetchPolicy::loopback_test_server(),
        Arc::new(Names(HashMap::from([
            ("pdf.example.org", vec![ip("127.0.0.1")]),
            ("mirror.example.org", vec![ip("127.0.0.1")]),
            ("metadata.example.org", vec![ip("169.254.169.254")]),
            ("mixed.example.org", vec![ip("127.0.0.1"), ip("10.1.2.3")]),
            ("v6private.example.org", vec![ip("fd00::1")]),
            ("mapped.example.org", vec![ip("::ffff:192.168.1.1")]),
        ]))),
    )
}
const PDF: &[u8] = b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\ntrailer\n<<>>\n%%EOF\n";
fn get(f: &PdfFetcher, url: &str) -> Result<(FetchedPdf, Vec<u8>), FetchError> {
    let mut sink = Vec::new();
    f.fetch(url, &mut sink, &Cancellation::default(), &mut |_| true)
        .map(|r| (r, sink))
}

#[test]
fn default_policy_rejects_non_public_targets_syntactically() {
    let p = FetchPolicy::default();
    for url in [
        "file:///etc/passwd",
        "ftp://example.org/a.pdf",
        "javascript:alert(1)",
        "data:application/pdf;base64,JVBERi0=",
        "http://example.org/a.pdf",
        "https://localhost/a.pdf",
        "https://LOCALHOST./a.pdf",
        "https://intranet/a.pdf",
        "https://printer.internal/a.pdf",
        "https://box.local/a.pdf",
        "https://router.home.arpa/a.pdf",
        "https://127.0.0.1/a.pdf",
        "https://2130706433/a.pdf",
        "https://0x7f.1/a.pdf",
        "https://0.0.0.0/a.pdf",
        "https://10.0.0.1/a.pdf",
        "https://172.16.5.4/a.pdf",
        "https://192.168.0.10/a.pdf",
        "https://100.64.0.1/a.pdf",
        "https://169.254.169.254/latest/meta-data",
        "https://198.18.0.1/a.pdf",
        "https://224.0.0.1/a.pdf",
        "https://255.255.255.255/a.pdf",
        "https://[::1]/a.pdf",
        "https://[::]/a.pdf",
        "https://[fe80::1]/a.pdf",
        "https://[fd12:3456::1]/a.pdf",
        "https://[::ffff:127.0.0.1]/a.pdf",
        "https://[::ffff:10.0.0.1]/a.pdf",
        "https://[64:ff9b::a00:1]/a.pdf",
        "https://[2002:a00:1::1]/a.pdf",
        "https://[2001:db8::1]/a.pdf",
        "https://user:pw@example.org/a.pdf",
        "https://user@example.org/a.pdf",
        "https://example.org:8443/a.pdf",
        "https://exa mple.org/a.pdf",
    ] {
        assert!(
            matches!(p.check_url(url), Err(FetchError::Blocked(_))),
            "{url} must be blocked"
        );
    }
    for url in [
        "https://example.org/a.pdf",
        "https://example.org:443/a.pdf",
        "https://93.184.216.34/a.pdf",
        "https://[2606:4700::1111]/a.pdf",
    ] {
        assert!(p.check_url(url).is_ok(), "{url} must be allowed");
    }
    // Even the test policy keeps private ranges and local names blocked.
    let t = FetchPolicy::loopback_test_server();
    assert!(t.check_url("http://127.0.0.1:9/a.pdf").is_ok());
    for url in [
        "http://10.0.0.1:9/a.pdf",
        "http://169.254.169.254/",
        "http://localhost:9/",
    ] {
        assert!(t.check_url(url).is_err(), "{url}");
    }
}

#[test]
fn fetches_pdf_and_hashes_without_cookies_or_credentials() {
    let server = TestServer::start(|r| match r.path.as_str() {
        "/start" => Response::redirect(302, "/hop2").header("Set-Cookie", "session=secret; Path=/"),
        "/hop2" => Response::redirect(
            301,
            &format!(
                "http://mirror.example.org:{}/paper.pdf",
                r.header("host").unwrap().rsplit(':').next().unwrap()
            ),
        ),
        "/paper.pdf" => Response::pdf(PDF).header("Set-Cookie", "tracker=1"),
        _ => Response::new(404),
    });
    let f = fetcher();
    let (fetched, body) = get(&f, &server.url("pdf.example.org", "/start")).unwrap();
    assert_eq!(body, PDF);
    assert_eq!(fetched.bytes, PDF.len() as u64);
    assert_eq!(fetched.sha256, <[u8; 32]>::from(Sha256::digest(PDF)));
    assert_eq!(fetched.redirects, 2);
    assert!(fetched.final_url.ends_with("/paper.pdf"));
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    for r in &requests {
        for forbidden in ["cookie", "authorization", "referer", "proxy-authorization"] {
            assert!(r.header(forbidden).is_none(), "{forbidden} sent: {r:?}");
        }
        assert_eq!(r.header("accept-encoding"), Some("identity"));
        assert!(
            r.header("user-agent")
                .unwrap()
                .starts_with("citation-manager/")
        );
    }
    assert!(
        requests[2]
            .header("host")
            .unwrap()
            .starts_with("mirror.example.org")
    );
}

#[test]
fn redirects_and_dns_answers_are_rechecked_every_hop() {
    let server = TestServer::start(|r| {
        let target = match r.path.as_str() {
            "/to-metadata" => "http://169.254.169.254/latest/meta-data/",
            "/to-private-name" => "http://metadata.example.org/x.pdf",
            "/to-mixed" => "http://mixed.example.org/x.pdf",
            "/to-v6" => "http://v6private.example.org/x.pdf",
            "/to-mapped" => "http://mapped.example.org/x.pdf",
            "/to-file" => "file:///etc/passwd",
            "/to-localhost" => "http://localhost/x.pdf",
            "/loop" => "/loop",
            _ => return Response::new(404),
        };
        Response::redirect(302, target)
    });
    let f = fetcher();
    for path in [
        "/to-metadata",
        "/to-private-name",
        "/to-mixed",
        "/to-v6",
        "/to-mapped",
        "/to-file",
        "/to-localhost",
    ] {
        let error = get(&f, &server.url("pdf.example.org", path)).unwrap_err();
        assert!(matches!(error, FetchError::Blocked(_)), "{path}: {error:?}");
        assert!(!error.retryable());
    }
    assert_eq!(
        get(&f, &server.url("pdf.example.org", "/loop")).unwrap_err(),
        FetchError::TooManyRedirects
    );
    // Direct DNS answers too: a public-looking name that resolves privately is refused
    // before any connection, and unknown names fail to resolve.
    for host in ["metadata.example.org", "mixed.example.org"] {
        assert!(matches!(
            get(&f, &format!("http://{host}/x.pdf")),
            Err(FetchError::Blocked(_))
        ));
    }
    assert_eq!(
        get(&f, "http://unknown.example.org/x.pdf").unwrap_err(),
        FetchError::Network
    );
    // Only the allowed redirect hops reached the fixture server.
    assert!(
        server
            .requests()
            .iter()
            .all(|r| r.path.starts_with("/to-") || r.path == "/loop")
    );
}

#[test]
fn rejects_non_pdf_auth_size_and_classifies_retries() {
    let big = {
        let mut b = PDF.to_vec();
        b.resize(70 * 1024, b' ');
        b
    };
    let server = TestServer::start(move |r| match r.path.as_str() {
        "/login" => Response::new(200)
            .header("Content-Type", "text/html; charset=utf-8")
            .body(b"<html>Please sign in</html>"),
        "/octet-not-pdf" => Response::new(200)
            .header("Content-Type", "application/octet-stream")
            .body(b"<html>not a pdf</html>"),
        "/gzip" => Response::pdf(PDF).header("Content-Encoding", "gzip"),
        "/forbidden" => Response::new(403),
        "/unauthorized" => Response::new(401),
        "/gone" => Response::new(410),
        "/busy" => Response::new(503).header("Retry-After", "1"),
        "/slow-down" => Response::new(429),
        "/teapot" => Response::new(418),
        "/big-declared" => Response::pdf(&big),
        "/big-chunked" => Response {
            chunked: true,
            ..Response::pdf(&big)
        },
        "/stall" => Response {
            stall: Some(Duration::from_secs(6)),
            ..Response::pdf(PDF)
        },
        "/late-magic" => {
            let mut b = vec![b' '; 100];
            b.extend_from_slice(PDF);
            Response::pdf(&b)
        }
        "/empty" => Response::pdf(b""),
        _ => Response::new(404),
    });
    let mut policy = FetchPolicy::loopback_test_server();
    policy.max_bytes = 64 * 1024;
    let f = PdfFetcher::with_resolver(policy, fetcher().resolver.clone());
    let url = |p: &str| server.url("pdf.example.org", p);
    let err = |p: &str| get(&f, &url(p)).unwrap_err();
    // Regression (#22 review P3): a compressed body is its own error with its own guidance.
    let gzip = err("/gzip");
    assert_eq!(gzip, FetchError::Encoded("gzip".into()));
    assert!(!gzip.retryable() && !gzip.needs_user_download());
    assert_eq!(gzip.guidance(), Some(ENCODED_GUIDANCE));
    assert!(gzip.to_string().contains("Content-Encoding gzip"));
    assert_eq!(err("/login").guidance(), Some(USER_DOWNLOAD_GUIDANCE));
    for p in ["/login", "/octet-not-pdf", "/empty"] {
        let e = err(p);
        assert!(matches!(e, FetchError::NotPdf(_)), "{p}: {e:?}");
        assert!(!e.retryable());
    }
    assert!(err("/login").needs_user_download());
    assert_eq!(err("/forbidden"), FetchError::AuthRequired(403));
    assert_eq!(err("/unauthorized"), FetchError::AuthRequired(401));
    assert!(err("/forbidden").needs_user_download());
    assert_eq!(err("/gone"), FetchError::NotFound(410));
    assert!(err("/busy").retryable());
    assert!(err("/slow-down").retryable());
    assert!(!err("/teapot").retryable());
    assert_eq!(err("/big-declared"), FetchError::TooLarge);
    assert_eq!(err("/big-chunked"), FetchError::TooLarge);
    let stalled = err("/stall");
    assert_eq!(stalled, FetchError::Timeout);
    assert!(stalled.retryable());
    assert!(get(&f, &url("/late-magic")).is_ok());
}

#[test]
fn cancellation_and_invalid_configuration() {
    let server = TestServer::start(|_| Response::pdf(PDF));
    let f = fetcher();
    let cancel = Cancellation::default();
    cancel.cancel();
    let mut sink = Vec::new();
    assert_eq!(
        f.fetch(
            &server.url("pdf.example.org", "/a.pdf"),
            &mut sink,
            &cancel,
            &mut |_| true
        ),
        Err(FetchError::Cancelled)
    );
    assert_eq!(
        f.fetch(
            &server.url("pdf.example.org", "/a.pdf"),
            &mut sink,
            &Cancellation::default(),
            &mut |_| false
        ),
        Err(FetchError::Cancelled)
    );
    let bad = FetchPolicy {
        max_bytes: MAX_PDF_BYTES + 1,
        ..FetchPolicy::default()
    };
    assert!(matches!(
        PdfFetcher::new(bad).fetch(
            "https://example.org/a.pdf",
            &mut sink,
            &Cancellation::default(),
            &mut |_| true
        ),
        Err(FetchError::InvalidConfig(_))
    ));
}

/// Resolves `slow.example.org` to loopback after `delay`, unless released earlier; counts
/// lookups. Other names behave like `fetcher()`'s fixtures.
struct Slow {
    delay: Duration,
    calls: AtomicUsize,
    release: std::sync::atomic::AtomicBool,
}
impl HostResolver for Slow {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        while started.elapsed() < self.delay && !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(vec![ip("127.0.0.1")])
    }
}
fn slow(delay: Duration) -> Arc<Slow> {
    Arc::new(Slow {
        delay,
        calls: AtomicUsize::new(0),
        release: false.into(),
    })
}

#[test]
fn dns_wait_is_cancellable_bounded_and_counts_against_the_deadline() {
    // Regression (#22 review P2).
    let resolver = slow(Duration::from_secs(5));
    let f = PdfFetcher::with_resolver(FetchPolicy::loopback_test_server(), resolver.clone());
    // Cancellation is observed while the lookup is still blocked.
    let cancel = Cancellation::default();
    let canceller = {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            cancel.cancel();
        })
    };
    let started = Instant::now();
    let mut sink = Vec::new();
    assert_eq!(
        f.fetch(
            "http://slow.example.org:9/a.pdf",
            &mut sink,
            &cancel,
            &mut |_| true
        ),
        Err(FetchError::Cancelled)
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    canceller.join().unwrap();

    // Stuck lookups are bounded: once MAX_DNS_IN_FLIGHT are outstanding, further fetches
    // fail fast without starting another lookup thread.
    let policy = FetchPolicy {
        io_timeout: Duration::from_millis(50),
        ..FetchPolicy::loopback_test_server()
    };
    let resolver = slow(Duration::from_secs(10));
    let f = PdfFetcher::with_resolver(policy, resolver.clone());
    for _ in 0..MAX_DNS_IN_FLIGHT {
        assert_eq!(
            get(&f, "http://slow.example.org:9/a.pdf").unwrap_err(),
            FetchError::Timeout
        );
    }
    let started = Instant::now();
    let refused = get(&f, "http://slow.example.org:9/a.pdf").unwrap_err();
    assert_eq!(refused, FetchError::Network);
    assert!(refused.retryable());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), MAX_DNS_IN_FLIGHT);
    // When the stuck lookups return, capacity comes back.
    resolver.release.store(true, Ordering::SeqCst);
    let mut freed = false;
    for _ in 0..200 {
        if f.dns_in_flight.load(Ordering::SeqCst) == 0 {
            freed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(freed);

    // Time spent resolving is taken from the whole-fetch budget for the HTTP hop.
    let server = TestServer::start(|_| Response {
        stall: Some(Duration::from_secs(3)),
        ..Response::pdf(PDF)
    });
    let policy = FetchPolicy {
        total_timeout: Duration::from_millis(1000),
        io_timeout: Duration::from_millis(1000),
        ..FetchPolicy::loopback_test_server()
    };
    let f = PdfFetcher::with_resolver(policy, slow(Duration::from_millis(800)));
    let started = Instant::now();
    assert_eq!(
        get(&f, &server.url("slow.example.org", "/a.pdf")).unwrap_err(),
        FetchError::Timeout
    );
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_millis(1500), "{elapsed:?}");
}
