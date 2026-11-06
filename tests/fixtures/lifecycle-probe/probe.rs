//! Test-only native host/daemon fixture. Compile with rustc, never a product crate.
#![forbid(unsafe_code)]
use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const IDLE: Duration = Duration::from_secs(2);
struct Config {
    dir: PathBuf,
    nonce: String,
    exe: PathBuf,
}
impl Config {
    fn load() -> Result<Self> {
        let exe = std::env::current_exe()?;
        let macos = exe.parent().ok_or("missing MacOS")?;
        let contents = macos.parent().ok_or("missing Contents")?;
        if macos.file_name().is_none_or(|n| n != "MacOS")
            || contents.file_name().is_none_or(|n| n != "Contents")
        {
            return Err("probe requires an actual bundle".into());
        }
        let file = contents.join("Resources/lifecycle-config.txt");
        if !fs::symlink_metadata(&file)?.is_file() {
            return Err("unsafe config".into());
        }
        let config = fs::read_to_string(file)?;
        let mut lines = config.lines();
        let nonce = lines.next().ok_or("no nonce")?.to_string();
        let dir = PathBuf::from(lines.next().ok_or("no private path")?);
        if lines.next().is_some()
            || nonce.len() != 24
            || !nonce.bytes().all(|b| b.is_ascii_hexdigit())
            || !dir.is_absolute()
            || dir.as_os_str().len() > 75
            || !fs::symlink_metadata(&dir)?.is_dir()
            || fs::metadata(&dir)?.permissions().mode() & 0o077 != 0
            || fs::read_to_string(dir.join("nonce"))? != nonce
        {
            return Err("invalid private test config".into());
        }
        Ok(Self { dir, nonce, exe })
    }
    fn socket(&self) -> PathBuf {
        self.dir.join("probe.sock")
    }
    fn event(&self, event: &str) -> io::Result<()> {
        // One file per process, never shared append interleaving.
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(self.dir.join(format!("events-{}.txt", process::id())))?;
        writeln!(f, "{event}")
    }
}
use std::os::unix::fs::OpenOptionsExt;
fn main() {
    if let Err(e) = run() {
        eprintln!("cm34 test probe failed: {e}");
        process::exit(1);
    }
}
fn run() -> Result<()> {
    let cfg = Config::load()?;
    if cfg
        .exe
        .file_name()
        .is_some_and(|n| n == "cm34-probe-daemon")
    {
        daemon(cfg)
    } else {
        host(cfg)
    }
}
fn daemon(cfg: Config) -> Result<()> {
    // OS-released advisory lock covers stale-socket removal, bind, and orderly unlink.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(cfg.dir.join("instance.lock"))?;
    match lock.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(e) => return Err(e.into()),
    }
    if cfg.socket().exists() {
        fs::remove_file(cfg.socket())?;
    }
    let listener = UnixListener::bind(cfg.socket())?;
    fs::set_permissions(cfg.socket(), fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    cfg.event("daemon-start")?;
    let active = Arc::new(AtomicUsize::new(0));
    let mut idle = Instant::now();
    let mut joins = Vec::new();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                active.fetch_add(1, Ordering::SeqCst);
                let count = active.clone();
                let nonce = cfg.nonce.clone();
                joins.push(thread::spawn(move || {
                    let _ = serve(stream, &nonce);
                    count.fetch_sub(1, Ordering::SeqCst);
                }));
                idle = Instant::now();
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
            Err(e) => return Err(e.into()),
        }
        if active.load(Ordering::SeqCst) > 0 {
            idle = Instant::now();
        } else if idle.elapsed() >= IDLE {
            break;
        }
        // Bounded polling belongs only to this tiny fixture, not a service implementation.
        thread::sleep(Duration::from_millis(20));
    }
    drop(listener);
    fs::remove_file(cfg.socket())?;
    cfg.event("daemon-idle-exit")?;
    for j in joins {
        let _ = j.join();
    }
    drop(lock);
    Ok(())
}
fn serve(mut stream: UnixStream, nonce: &str) -> Result<()> {
    // Only fixed nonce ping is accepted. No paths, commands, library/API data or CRUD.
    // macOS accept inherits O_NONBLOCK from the listener; client reads must wait.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut input = BufReader::new(stream.try_clone()?);
    loop {
        let mut line = String::new();
        let n = input.by_ref().take(128).read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if line != format!("{nonce} ping\n") {
            return Err("invalid socket request".into());
        }
        writeln!(stream, "{}", process::id())?;
    }
}
fn connect(cfg: &Config) -> Result<UnixStream> {
    if let Ok(s) = UnixStream::connect(cfg.socket()) {
        return Ok(s);
    }
    let helper = cfg.exe.with_file_name("cm34-probe-daemon");
    if !fs::symlink_metadata(&helper)?.is_file() {
        return Err("unsafe daemon helper".into());
    }
    let mut child = Command::new(helper)
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    // Reap while host is alive; after host death the OS adopts the independent daemon.
    thread::spawn(move || {
        let _ = child.wait();
    });
    let start = Instant::now();
    loop {
        if let Ok(s) = UnixStream::connect(cfg.socket()) {
            return Ok(s);
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err("daemon startup timeout".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn host(cfg: Config) -> Result<()> {
    cfg.event("host-start")?;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut output = io::stdout().lock();
    let mut stream: Option<UnixStream> = None;
    let mut last_pid: Option<u32> = None;
    loop {
        let mut header = [0u8; 4];
        // Clean EOF is recorded only at message boundary; truncated frames fail closed.
        match input.read(&mut header[..1])? {
            0 => {
                cfg.event("host-eof")?;
                return Ok(());
            }
            1 => input.read_exact(&mut header[1..])?,
            _ => unreachable!(),
        }
        let length = u32::from_ne_bytes(header) as usize;
        if length > 4096 {
            return Err("frame limit".into());
        }
        let mut message = vec![0; length];
        input.read_exact(&mut message)?;
        if let Some(pid) = last_pid {
            let ack = format!(
                "{{\"nonce\":\"{}\",\"op\":\"ack\",\"host_pid\":{},\"daemon_pid\":{}}}",
                cfg.nonce,
                process::id(),
                pid
            );
            if message == ack.as_bytes() {
                cfg.event(&format!("client-ack daemon={pid}"))?;
                continue;
            }
        }
        if message != format!("{{\"nonce\":\"{}\",\"op\":\"ping\"}}", cfg.nonce).as_bytes() {
            return Err("only the exact nonce ping fixture request is supported".into());
        }
        if stream.is_none() {
            stream = Some(connect(&cfg)?);
        }
        let s = stream.as_mut().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.set_write_timeout(Some(Duration::from_secs(2)))?;
        writeln!(s, "{} ping", cfg.nonce)?;
        let mut reader = BufReader::new(s.try_clone()?);
        let mut reply = String::new();
        reader.by_ref().take(64).read_line(&mut reply)?;
        let pid: u32 = reply.trim().parse()?;
        last_pid = Some(pid);
        // JSON-safe paths: encoded by a small test-only string escaper.
        let response = format!(
            "{{\"nonce\":\"{}\",\"host_pid\":{},\"daemon_pid\":{},\"host_path\":{},\"daemon_path\":{}}}",
            cfg.nonce,
            process::id(),
            pid,
            json_path(&cfg.exe),
            json_path(&cfg.exe.with_file_name("cm34-probe-daemon"))
        );
        output.write_all(&(response.len() as u32).to_ne_bytes())?;
        output.write_all(response.as_bytes())?;
        output.flush()?;
        cfg.event(&format!("reply daemon={pid}"))?;
    }
}
fn json_path(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}
