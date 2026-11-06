//! Opt-in #34 fixture composition, not a product resource/installer API.
//! Views still use LibraryClient; all fixture I/O/PDF work runs off the UI thread.
use cm_pdf::worker::{Cancellation, Operation, Output, PdfWorker, Source, bundled_library};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// The only entry point requires an existing private, empty synthetic output directory.
/// No environment, PATH, working directory or real-user-library discovery is used.
pub fn prepare(out: PathBuf) -> Result<PathBuf> {
    std::thread::Builder::new()
        .name("layout-smoke-io".into())
        .spawn(move || run(out))?
        .join()
        .map_err(|_| "layout worker panicked")?
}

fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(format!("missing or non-regular resource: {}", path.display()).into());
    }
    Ok(())
}

fn run(out: PathBuf) -> Result<PathBuf> {
    if !out.is_absolute()
        || !fs::symlink_metadata(&out)?.is_dir()
        || fs::metadata(&out)?.permissions().mode() & 0o077 != 0
        || fs::read_dir(&out)?.next().is_some()
    {
        return Err("smoke output must be an existing empty private absolute directory".into());
    }
    // /tmp is a macOS symlink; pass storage its canonical dedicated path.
    let out = out.canonicalize()?;
    let exe = std::env::current_exe()?;
    let library = bundled_library(&exe)?; // preserve #11's resolution and original hash gate
    let contents = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("invalid bundle")?;
    let resources = contents.join("Resources");
    for dir in [
        contents,
        resources.as_path(),
        contents.join("Frameworks").as_path(),
    ] {
        if !fs::symlink_metadata(dir)?.is_dir() {
            return Err("bundle directories must not be symlinks".into());
        }
    }
    regular(&library)?;
    let inventory = resources.join("layout-integrity.json");
    regular(&inventory)?;
    let hashes: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(inventory)?)?;
    for required in [
        "Resources/layout-fixture.pdf",
        "Resources/lifecycle-config.txt",
        "Resources/licenses/pdfium/LICENSE",
        "Resources/licenses/pdfium-render/LICENSE-APACHE-2.0.txt",
        "Resources/licenses/pdfium-render/NOTICE.txt",
        "Resources/host-manifests/chromium.json.in",
        "Resources/host-manifests/firefox.json.in",
        "MacOS/cm34-probe-host",
        "MacOS/cm34-probe-daemon",
    ] {
        if !hashes.contains_key(required) {
            return Err(format!("required inventory entry absent: {required}").into());
        }
    }
    for (relative, expected) in &hashes {
        let relative = Path::new(relative);
        if relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err("unsafe inventory path".into());
        }
        let path = contents.join(relative);
        // Reject symlink components as well as symlink leaves; never escape the bundle.
        let mut prefix = contents.to_path_buf();
        for component in relative.components() {
            prefix.push(component);
            if fs::symlink_metadata(&prefix)?.file_type().is_symlink() {
                return Err("symlink in bundle inventory".into());
            }
        }
        regular(&path)?;
        if format!("{:x}", Sha256::digest(fs::read(&path)?)) != *expected {
            return Err(format!("resource checksum mismatch: {}", path.display()).into());
        }
    }
    let worker = PdfWorker::start(library.clone())?;
    let fixture = resources.join("layout-fixture.pdf");
    let before = Sha256::digest(fs::read(&fixture)?);
    let output = worker
        .submit(
            Operation::Page {
                source: Source {
                    path: fixture.clone(),
                    password: None,
                },
                page_index: 0u32.try_into()?,
                zoom: Some(1.),
            },
            Cancellation::default(),
            Duration::from_secs(15),
        )?
        .wait(Duration::from_secs(20))?;
    let Output::Page(page) = output else {
        return Err("expected page".into());
    };
    if !page.text.contains("日本語") || page.characters.is_empty() {
        return Err("synthetic Unicode text extraction failed".into());
    }
    let bitmap = page.bitmap.as_ref().ok_or("missing rendered bitmap")?;
    if bitmap.rgba.len() != (bitmap.width * bitmap.height * 4) as usize
        || !bitmap.rgba.as_chunks::<4>().0.iter().any(|p| p[0] < 200)
    {
        return Err("empty/invalid raster".into());
    }
    let mut ppm = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("page.ppm"))?;
    write!(ppm, "P6\n{} {}\n255\n", bitmap.width, bitmap.height)?;
    for p in bitmap.rgba.as_chunks::<4>().0 {
        ppm.write_all(&p[..3])?;
    }
    let after = Sha256::digest(fs::read(&fixture)?);
    if before != after {
        return Err("fixture changed".into());
    }
    let report = serde_json::json!({
        "executable": exe, "contents": contents, "resources": resources,
        "pdfium_library": library, "pdfium_build": cm_pdf::worker::PDFIUM_BUILD,
        "pdfium_abi": cm_pdf::worker::PDFIUM_ABI,
        "pdfium_sha256": cm_pdf::worker::PDFIUM_SHA256,
        "host": contents.join("MacOS/cm34-probe-host"),
        "daemon": contents.join("MacOS/cm34-probe-daemon"),
        "fixture_sha256_before": format!("{before:x}"),
        "fixture_sha256_after": format!("{after:x}"),
        "width": bitmap.width, "height": bitmap.height,
        "text": page.text, "characters": page.characters.len(),
        "cwd": std::env::current_dir()?, "inventory_entries": hashes.len(),
        "io_thread": std::thread::current().name(),
    });
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out.join("layout-report.json"))?
        .write_all(&serde_json::to_vec_pretty(&report)?)?;
    worker.shutdown()?;
    // This worker starts before GPUI's event loop and only returns a dedicated synthetic path.
    Ok(out.join("synthetic-library"))
}
