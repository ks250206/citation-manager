//! Minimal local HTTP/1.1 fixture server for offline fetch tests. Compiled only for tests or
//! with the `loopback-test-server` feature (enabled by dependent crates' dev-dependencies).
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Lowercase names.
    pub headers: Vec<(String, String)>,
}
impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Send the body with chunked encoding (no Content-Length).
    pub chunked: bool,
    /// Pause after the headers, before any body byte.
    pub stall: Option<Duration>,
}
impl Response {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }
    pub fn pdf(body: &[u8]) -> Self {
        Self::new(200)
            .header("Content-Type", "application/pdf")
            .body(body)
    }
    pub fn redirect(status: u16, location: &str) -> Self {
        Self::new(status).header("Location", location)
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    pub fn body(mut self, body: &[u8]) -> Self {
        self.body = body.to_vec();
        self
    }
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

pub struct TestServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl TestServer {
    pub fn start(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let thread = {
            let requests = requests.clone();
            let stop = stop.clone();
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let handler = handler.clone();
                            let requests = requests.clone();
                            thread::spawn(move || serve(stream, &*handler, &requests));
                        }
                        Err(_) => thread::sleep(Duration::from_millis(5)),
                    }
                }
            })
        };
        Self {
            addr,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    /// `http://<host>:<port><path>`.
    pub fn url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{path}", self.port())
    }
    pub fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(mut stream: TcpStream, handler: &Handler, requests: &Mutex<Vec<Request>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    while !data.windows(4).any(|w| w == b"\r\n\r\n") && data.len() < 64 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => data.extend_from_slice(&buf[..n]),
        }
    }
    let text = String::from_utf8_lossy(&data);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let request = Request {
        method: first.next().unwrap_or("").into(),
        path: first.next().unwrap_or("").into(),
        headers: lines
            .take_while(|l| !l.is_empty())
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
            .collect(),
    };
    let response = handler(&request);
    requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(request);
    let mut head = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", response.status);
    for (n, v) in &response.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    if response.chunked {
        head.push_str("Transfer-Encoding: chunked\r\n");
    } else {
        head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    head.push_str("\r\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();
    if let Some(stall) = response.stall {
        thread::sleep(stall);
    }
    if response.chunked {
        for chunk in response.body.chunks(64 * 1024) {
            if stream
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .and_then(|()| stream.write_all(chunk))
                .and_then(|()| stream.write_all(b"\r\n"))
                .is_err()
            {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n");
    } else {
        let _ = stream.write_all(&response.body);
    }
    let _ = stream.flush();
}
