use cm_storage::{LibraryDb, LibraryDirectory};
use serde_json::Value;
use std::time::Instant;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

pub fn private_library() -> Result<(tempfile::TempDir, LibraryDirectory, LibraryDb)> {
    let temp = tempfile::Builder::new().prefix("cm-bench-47-").tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let directory = LibraryDirectory::prepare(temp.path().canonicalize()?.join("library"))?;
    let db = LibraryDb::open(&directory)?;
    Ok((temp, directory, db))
}

pub fn entry(run: impl FnOnce(usize, usize) -> Result<Value>, max: usize) -> Result<()> {
    if cfg!(debug_assertions) {
        return Err(
            "benchmark executable requires --release (small unit tests remain debug)".into(),
        );
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("expected: RECORDS SAMPLES; run via scripts/benchmarks/run.py".into());
    }
    let records = args[0].parse::<usize>()?;
    let samples = args[1].parse::<usize>()?;
    if records == 0 || records > max || !(1..=100).contains(&samples) {
        return Err("record/sample bounds exceeded".into());
    }
    println!("{}", serde_json::to_string_pretty(&run(records, samples)?)?);
    Ok(())
}
