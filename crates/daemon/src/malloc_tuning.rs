//! #85: glibc allocator setting for the long-lived processes (papersd and the GUI).
//!
//! glibc starts with a 128 KiB mmap threshold and, whenever an mmapped block is freed,
//! raises the threshold to that block's size (up to 32 MiB). After the first page bitmap
//! (4–16 MiB) is freed, every later bitmap-sized buffer therefore comes from the heap and
//! stays resident after it is freed, and `malloc_trim` cannot return most of it. Measured
//! on Linux (docs/validation/ISSUE_30.md, #85): papersd grew from 11.6 to 74.6 MiB over
//! 20 page renders and stayed there; with a fixed 256 KiB threshold it ended at 24.6 MiB.
//!
//! A fixed threshold (which also turns off the dynamic adjustment) can only be set at
//! process start without `unsafe` (`mallopt` is FFI), through the `GLIBC_TUNABLES`
//! environment variable. `apply` therefore re-executes the running binary once with the
//! tunable added. The process keeps its PID, arguments and open descriptors. A value the
//! user already set for this tunable is respected, and if the re-exec fails the process
//! simply continues with glibc's defaults. Child processes inherit the variable, which
//! only changes their allocator threshold. Other platforms and C libraries: no-op.

/// The tunable `apply` adds (256 KiB: large buffers are always mmapped and returned on free).
pub const MMAP_THRESHOLD_TUNABLE: &str = "glibc.malloc.mmap_threshold=262144";
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const VARIABLE: &str = "GLIBC_TUNABLES";

/// The `GLIBC_TUNABLES` value to re-execute with, or `None` when the current value already
/// sets an mmap threshold (ours, after the re-exec, or the user's) or is not UTF-8.
pub fn tunables_with_threshold(current: Option<&std::ffi::OsStr>) -> Option<String> {
    let current = match current {
        None => "",
        Some(value) => value.to_str()?,
    };
    if current
        .split(':')
        .any(|entry| entry.starts_with("glibc.malloc.mmap_threshold="))
    {
        return None;
    }
    Some(if current.is_empty() {
        MMAP_THRESHOLD_TUNABLE.to_owned()
    } else {
        format!("{current}:{MMAP_THRESHOLD_TUNABLE}")
    })
}

/// Call first thing in `main`. Returns only when no re-exec happened (already applied, not
/// glibc Linux, or the exec failed).
pub fn apply() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        use std::os::unix::process::CommandExt;
        let Some(value) = tunables_with_threshold(std::env::var_os(VARIABLE).as_deref()) else {
            return;
        };
        let mut args = std::env::args_os();
        let Some(arg0) = args.next() else { return };
        // /proc/self/exe is the running image itself, even if its path was replaced since.
        let _failed = std::process::Command::new("/proc/self/exe")
            .arg0(arg0)
            .args(args)
            .env(VARIABLE, value)
            .exec();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    #[test]
    fn adds_the_threshold_once_and_respects_existing_settings() {
        assert_eq!(
            tunables_with_threshold(None).as_deref(),
            Some(MMAP_THRESHOLD_TUNABLE)
        );
        assert_eq!(
            tunables_with_threshold(Some(OsStr::new(""))).as_deref(),
            Some(MMAP_THRESHOLD_TUNABLE)
        );
        assert_eq!(
            tunables_with_threshold(Some(OsStr::new("glibc.malloc.arena_max=2"))),
            Some(format!("glibc.malloc.arena_max=2:{MMAP_THRESHOLD_TUNABLE}"))
        );
        // After the re-exec (no loop), or a threshold the user chose.
        assert_eq!(
            tunables_with_threshold(Some(OsStr::new(MMAP_THRESHOLD_TUNABLE))),
            None
        );
        assert_eq!(
            tunables_with_threshold(Some(OsStr::new(
                "glibc.malloc.arena_max=2:glibc.malloc.mmap_threshold=65536"
            ))),
            None
        );
        // A different tunable with a similar name does not count.
        assert!(
            tunables_with_threshold(Some(OsStr::new("glibc.malloc.mmap_threshold_x=1"))).is_some()
        );
    }
}
