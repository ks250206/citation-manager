use super::files::{invalid, safe};
use cm_protocol::{library::ServiceError, settings::*};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};
fn private(path: &Path) -> Result<(), ServiceError> {
    safe(path, true)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = fs::symlink_metadata(path).map_err(|e| invalid(e.to_string()))?;
        if m.mode() & 0o7777 != 0o600 || m.uid() != rustix::process::geteuid().as_raw() {
            return Err(invalid("Settings must be an owned private 0600 file"));
        }
    }
    Ok(())
}
pub(super) fn load(path: &Path) -> Result<SettingsLoaded, ServiceError> {
    safe(
        path.parent()
            .ok_or_else(|| invalid("Settings require a private directory"))?,
        false,
    )?;
    let result = (|| {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(e) => return Err(invalid(e.to_string())),
            Ok(_) => (),
        }
        private(path)?;
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| invalid(e.to_string()))?
            .take(16385)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(e.to_string()))?;
        if bytes.len() > 16384 {
            return Err(invalid("Settings exceed 16 KiB"));
        }
        let settings: Settings = serde_json::from_slice(&bytes)
            .map_err(|e| invalid(format!("Invalid settings: {e}")))?;
        if !settings.valid() {
            return Err(invalid("Invalid layout values"));
        }
        Ok(settings)
    })();
    Ok(match result {
        Ok(settings) => SettingsLoaded {
            settings,
            warning: None,
        },
        Err(e) => SettingsLoaded {
            settings: Settings::default(),
            warning: Some(format!("Using default layout: {e}")),
        },
    })
}
pub(super) fn save(path: &Path, settings: &Settings) -> Result<(), ServiceError> {
    if !settings.valid() {
        return Err(invalid("Invalid layout values"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Settings require a directory"))?;
    safe(parent, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = fs::metadata(parent).map_err(|e| invalid(e.to_string()))?;
        if m.mode() & 0o7777 != 0o700 || m.uid() != rustix::process::geteuid().as_raw() {
            return Err(invalid("Settings directory must be private 0700"));
        }
    }
    match fs::symlink_metadata(path) {
        Ok(_) => private(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(invalid(e.to_string())),
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| invalid(e.to_string()))?;
    temp.write_all(&serde_json::to_vec(settings).map_err(|e| invalid(e.to_string()))?)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|e| invalid(e.to_string()))?;
    temp.persist(path).map_err(|e| invalid(e.to_string()))?;
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| invalid(e.to_string()))
}
