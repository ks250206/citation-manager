//! Diagnostics go to **stderr only**: stdout carries MCP protocol frames exclusively.
//! `PAPERS_MCP_LOG=off|error|warn|info|debug` (default `info`). Never log secrets or
//! library content (titles, page text, annotation text).
use std::{
    io::Write,
    sync::atomic::{AtomicU8, Ordering},
};

pub const ENV_LOG: &str = "PAPERS_MCP_LOG";
static LEVEL: AtomicU8 = AtomicU8::new(3);

pub fn init_from_env() {
    let level = match std::env::var(ENV_LOG).as_deref() {
        Ok("off") => 0,
        Ok("error") => 1,
        Ok("warn") => 2,
        Ok("debug") => 4,
        _ => 3,
    };
    LEVEL.store(level, Ordering::Relaxed);
}

fn emit(level: u8, tag: &str, message: &str) {
    if LEVEL.load(Ordering::Relaxed) >= level {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "papers-mcp {tag}: {message}");
    }
}
pub fn error(message: &str) {
    emit(1, "error", message);
}
pub fn warn(message: &str) {
    emit(2, "warn", message);
}
pub fn info(message: &str) {
    emit(3, "info", message);
}
pub fn debug(message: &str) {
    emit(4, "debug", message);
}
