//! Installer/user subcommands: `register`, `pair`, `refresh`, `unregister`, `status`,
//! `manifest`. These run from a terminal, an installer or the app, never from a browser.
use crate::{
    caller::Allowlist,
    config::{CONFIG_VERSION, HostConfig, ManifestRecord, Store, euid},
    daemon::{self, HostConnector},
    manifest::{self, Browser},
};
use cm_protocol::{
    client::{
        ClientError, Session,
        local::{CredentialSource, DaemonPaths},
    },
    ipc::{Command, GrantCommand, GrantId, GrantReply, HandshakeRejection, Reply},
};
use serde_json::json;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Pinned development extension IDs for the smoke-test extension in
/// `browser-extension/native-manifests/dev-extension/`. Chrome's ID is derived from the
/// public `key` committed there (the private key is not kept). Release IDs of the real
/// extension (#24) are passed with `--chrome-extension-id` / `--firefox-extension-id`.
pub const DEV_CHROME_EXTENSION_ID: &str = "baapgkfjfjjagdkpkmocjhfopmaohcdc";
pub const DEV_FIREFOX_EXTENSION_ID: &str = "citation-manager-dev@citation-manager.invalid";
pub const GRANT_LABEL: &str = "browser native host";

pub const USAGE: &str = "\
papers-native-host: Citation Manager browser Native Messaging host (#23)

Browsers start this program themselves. Setup subcommands:
  register --library ABS [--runtime-dir ABS] [--browser NAME[=DIR]]...
           [--chrome-extension-id ID]... [--firefox-extension-id ID]...
           [--development-extensions] [--allow-development-build] [--pair]
      Write the host configuration and per-user browser manifests (NAME: chrome, chromium,
      firefox; DIR overrides the manifest directory) pointing at this executable's absolute
      path. Without --browser, browsers with an existing profile directory are registered.
      Re-running keeps earlier manifests, IDs and pairing unless overridden.
  pair [--force]
      Ask the library daemon (started on demand) for the limited browser-capture grant using
      the owner's desktop credential, and store its secret privately.
  refresh [--allow-development-build]
      Re-point recorded manifests at this executable after the app was moved or updated.
      Does nothing when not registered. Never contacts the daemon.
  unregister [--keep-grant]
      Remove the manifests, revoke the grant and delete the configuration. If revocation
      fails, the configuration keeps the grant id (marked unregistered) and exits 1: run it
      again. --keep-grant only removes the manifests and keeps configuration and credential
      without contacting the daemon (`register` restores access).
  status
      Print the registration as JSON; exit 1 when a manifest is missing or stale.
  manifest --browser NAME [--host-path ABS] [--chrome-extension-id ID]... [--firefox-extension-id ID]...
      Print one manifest document (for packaging).
";

fn fail(message: impl std::fmt::Display) -> u8 {
    eprintln!("papers-native-host: {message}");
    1
}

struct Args {
    list: Vec<OsString>,
    at: usize,
}
impl Args {
    fn next_flag(&mut self) -> Option<Result<String, String>> {
        let flag = self.list.get(self.at)?;
        self.at += 1;
        Some(
            flag.to_str()
                .map(str::to_owned)
                .ok_or_else(|| "arguments must be UTF-8".to_owned()),
        )
    }
    fn value(&mut self, flag: &str) -> Result<OsString, String> {
        let value = self
            .list
            .get(self.at)
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        self.at += 1;
        Ok(value)
    }
    fn absolute(&mut self, flag: &str) -> Result<PathBuf, String> {
        let path = PathBuf::from(self.value(flag)?);
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(format!("{flag} must be an absolute path"))
        }
    }
    fn text(&mut self, flag: &str) -> Result<String, String> {
        self.value(flag)?
            .into_string()
            .map_err(|_| format!("{flag} must be UTF-8"))
    }
}

#[derive(Default)]
struct Options {
    library: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    browsers: Vec<(Browser, Option<PathBuf>)>,
    chrome: BTreeSet<String>,
    firefox: BTreeSet<String>,
    development_extensions: bool,
    allow_development_build: bool,
    pair: bool,
    force: bool,
    keep_grant: bool,
    host_path: Option<PathBuf>,
}

fn parse_options(args: &[OsString], allowed: &[&str]) -> Result<Options, String> {
    let mut args = Args {
        list: args.to_vec(),
        at: 0,
    };
    let mut o = Options::default();
    while let Some(flag) = args.next_flag() {
        let flag = flag?;
        if !allowed.contains(&flag.as_str()) {
            return Err(format!("unknown option {flag}"));
        }
        match flag.as_str() {
            "--library" => o.library = Some(args.absolute(&flag)?),
            "--runtime-dir" => o.runtime_dir = Some(args.absolute(&flag)?),
            "--host-path" => o.host_path = Some(args.absolute(&flag)?),
            "--browser" => {
                let spec = args.text(&flag)?;
                let (name, dir) = match spec.split_once('=') {
                    Some((name, dir)) => (name, Some(PathBuf::from(dir))),
                    None => (spec.as_str(), None),
                };
                let browser =
                    Browser::parse(name).ok_or_else(|| format!("unknown browser {name:?}"))?;
                if dir.as_ref().is_some_and(|d| !d.is_absolute()) {
                    return Err("--browser directory must be absolute".into());
                }
                o.browsers.push((browser, dir));
            }
            "--chrome-extension-id" => {
                o.chrome.insert(args.text(&flag)?);
            }
            "--firefox-extension-id" => {
                o.firefox.insert(args.text(&flag)?);
            }
            "--development-extensions" => o.development_extensions = true,
            "--allow-development-build" => o.allow_development_build = true,
            "--pair" => o.pair = true,
            "--force" => o.force = true,
            "--keep-grant" => o.keep_grant = true,
            _ => unreachable!("checked against the allowed list"),
        }
    }
    Ok(o)
}

/// Dispatches a subcommand. `args[0]` is the subcommand name.
pub fn main(args: &[OsString]) -> u8 {
    let name = args.first().and_then(|a| a.to_str()).unwrap_or("help");
    let rest = &args[1.min(args.len())..];
    let result = match name {
        "register" => parse_options(
            rest,
            &[
                "--library",
                "--runtime-dir",
                "--browser",
                "--chrome-extension-id",
                "--firefox-extension-id",
                "--development-extensions",
                "--allow-development-build",
                "--pair",
            ],
        )
        .map(register),
        "pair" => parse_options(rest, &["--force"]).map(|o| pair(&o)),
        "refresh" => parse_options(rest, &["--allow-development-build"]).map(|o| refresh(&o)),
        "unregister" => parse_options(rest, &["--keep-grant"]).map(|o| unregister(&o)),
        "status" => parse_options(rest, &[]).map(|_| status()),
        "manifest" => parse_options(
            rest,
            &[
                "--browser",
                "--host-path",
                "--chrome-extension-id",
                "--firefox-extension-id",
            ],
        )
        .map(print_manifest),
        _ => {
            print!("{USAGE}");
            return if matches!(name, "help" | "--help" | "-h") {
                0
            } else {
                2
            };
        }
    };
    result.unwrap_or_else(|e| {
        eprintln!("papers-native-host: {e}\n\n{USAGE}");
        2
    })
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot resolve this executable: {e}"))
}

/// The executable browsers will start, checked before it is published in a manifest.
fn publishable_exe(allow_development_build: bool) -> Result<PathBuf, String> {
    let exe = current_exe()?;
    if manifest::in_development_tree(&exe) && !allow_development_build {
        return Err(format!(
            "{} is inside a source checkout's target directory; install it first \
             (scripts/install_linux.py or the .app) or pass --allow-development-build",
            exe.display()
        ));
    }
    if exe.to_str().is_none() {
        return Err(format!(
            "{} is not valid UTF-8; browsers cannot start it from a JSON manifest",
            exe.display()
        ));
    }
    manifest::check_executable(&exe, euid())?;
    let papersd = daemon::sibling_papersd(&exe);
    manifest::check_executable(&papersd, euid())
        .map_err(|e| format!("papersd must be installed next to the host: {e}"))?;
    Ok(exe)
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .ok_or_else(|| "HOME must be an absolute path".into())
}

fn allowed_ids<'a>(config: &'a HostConfig, record: &ManifestRecord) -> &'a BTreeSet<String> {
    if record.browser.is_firefox() {
        &config.allowed.firefox
    } else {
        &config.allowed.chrome
    }
}

/// The browser profile root (the manifest directory's parent) still exists.
fn profile_exists(record: &ManifestRecord) -> bool {
    record
        .path
        .parent()
        .and_then(Path::parent)
        .is_some_and(Path::is_dir)
}

/// Whether `refresh` keeps `record` current: its browser has allowed IDs and its profile
/// was not deleted.
fn refreshable(config: &HostConfig, record: &ManifestRecord) -> bool {
    !allowed_ids(config, record).is_empty() && profile_exists(record)
}

/// Writes the manifests. A browser without allowed IDs gets none (an empty
/// `allowed_origins` is useless; a stale one of ours is removed). `refresh` never recreates
/// a browser profile that was deleted.
fn write_manifests(config: &HostConfig, refreshing: bool) -> Result<(), String> {
    for record in &config.manifests {
        let ids = allowed_ids(config, record);
        if ids.is_empty() {
            if !refreshing {
                let removed = manifest::remove(&record.path).unwrap_or(false);
                eprintln!(
                    "papers-native-host: warning: no {} extension ID allowed; {} {}",
                    record.browser.name(),
                    record.path.display(),
                    if removed { "removed" } else { "not written" }
                );
            }
            continue;
        }
        if refreshing && !profile_exists(record) {
            eprintln!(
                "papers-native-host: skipping {}: browser profile no longer exists",
                record.path.display()
            );
            continue;
        }
        let dir = record
            .path
            .parent()
            .ok_or("manifest path has no directory")?;
        manifest::write(dir, record.browser, &config.host_path, ids)
            .map_err(|e| format!("{}: {e}", record.path.display()))?;
    }
    Ok(())
}

fn register(o: Options) -> u8 {
    let run = || -> Result<u8, String> {
        let store = Store::default_location().map_err(|e| e.to_string())?;
        let previous = store.load_optional()?;
        let host_path = publishable_exe(o.allow_development_build)?;
        let library = o
            .library
            .clone()
            .or_else(|| previous.as_ref().map(|p| p.library.clone()))
            .ok_or("--library is required for the first registration")?;
        let runtime_dir = o
            .runtime_dir
            .clone()
            .or_else(|| previous.as_ref().and_then(|p| p.runtime_dir.clone()));
        let mut allowed = Allowlist {
            chrome: o.chrome.clone(),
            firefox: o.firefox.clone(),
        };
        if o.development_extensions {
            allowed.chrome.insert(DEV_CHROME_EXTENSION_ID.into());
            allowed.firefox.insert(DEV_FIREFOX_EXTENSION_ID.into());
            eprintln!(
                "papers-native-host: warning: allowing the development smoke-test extension IDs \
                 ({DEV_CHROME_EXTENSION_ID}, {DEV_FIREFOX_EXTENSION_ID}). Anyone can load an \
                 unpacked/temporary extension with these IDs; use them only for testing."
            );
        }
        if allowed.chrome.is_empty() && allowed.firefox.is_empty() {
            allowed = previous.as_ref().map(|p| p.allowed.clone()).ok_or(
                "pass --chrome-extension-id/--firefox-extension-id or --development-extensions",
            )?;
        }
        allowed.validate()?;

        let home = home()?;
        let mut records: Vec<ManifestRecord> = previous
            .as_ref()
            .map(|p| p.manifests.clone())
            .unwrap_or_default();
        let mut selected = o.browsers.clone();
        if selected.is_empty() {
            selected = Browser::ALL
                .into_iter()
                .filter(|b| b.user_dirs(&home).1.is_dir())
                .map(|b| (b, None))
                .collect();
        }
        for (browser, dir) in selected {
            let dir = dir.unwrap_or_else(|| browser.user_dirs(&home).0);
            let path = dir.join(manifest::file_name());
            if !records.iter().any(|r| r.path == path) {
                records.push(ManifestRecord { browser, path });
            }
        }
        if let Some(p) = &previous
            && p.library != library
            && (p.grant.is_some() || !p.revoke_pending.is_empty())
        {
            // The grant lives in the previous library's daemon: revoke it there first.
            return Err(format!(
                "registered for {} with a grant; run `unregister` first (revokes it), \
                 then register the new library",
                p.library.display()
            ));
        }
        let mut config = HostConfig {
            version: CONFIG_VERSION,
            library,
            runtime_dir,
            host_path,
            allowed,
            manifests: records,
            grant: previous.as_ref().and_then(|p| p.grant),
            revoke_pending: previous
                .as_ref()
                .map(|p| p.revoke_pending.clone())
                .unwrap_or_default(),
            unregistered: false,
        };
        write_manifests(&config, false)?;
        store.save(&config).map_err(|e| e.to_string())?;
        if config.manifests.is_empty() {
            eprintln!(
                "papers-native-host: warning: no browser profile found; pass --browser chrome|chromium|firefox"
            );
        }
        for record in &config.manifests {
            println!(
                "registered {} manifest {}",
                record.browser.name(),
                record.path.display()
            );
        }
        println!("host {}", config.host_path.display());
        if o.pair {
            return Ok(pair_with(&store, &mut config, false));
        }
        if store.credential().is_err() {
            println!("not paired yet: run `{} pair`", config.host_path.display());
        }
        Ok(0)
    };
    run().unwrap_or_else(fail)
}

fn desktop_connector(config: &HostConfig, exe: &Path) -> Result<HostConnector, String> {
    let paths = DaemonPaths::new(&config.library, config.runtime_dir.as_deref(), euid())
        .map_err(|e| e.to_string())?;
    Ok(HostConnector::new(
        paths,
        euid(),
        CredentialSource::DesktopBootstrap,
        exe,
    ))
}

fn pair(o: &Options) -> u8 {
    let Ok(store) = Store::default_location() else {
        return fail("HOME is not set");
    };
    match store.load_optional() {
        Ok(Some(mut config)) => pair_with(&store, &mut config, o.force),
        Ok(None) => fail("not registered; run register first"),
        Err(e) => fail(e),
    }
}

/// Native-host grants have no expiry: the capture-only grant would otherwise stop browser
/// saving with no renewal flow (consent/rotation UI is #26). Instead every grant id is always
/// recorded somewhere (`grant` or `revoke_pending`) until a revocation succeeded.
fn pair_with(store: &Store, config: &mut HostConfig, force: bool) -> u8 {
    let mut run = || -> Result<u8, String> {
        let exe = current_exe()?;
        let paths = DaemonPaths::new(&config.library, config.runtime_dir.as_deref(), euid())
            .map_err(|e| e.to_string())?;
        let connector = desktop_connector(config, &exe)?;
        if !force && let Ok(credential) = store.credential() {
            let capture = HostConnector::capture(paths.clone(), euid(), credential, &exe);
            match Session::open(&capture) {
                Ok(session) if Some(session.welcome.grant) == config.grant => {
                    println!("already paired (grant {})", session.welcome.grant);
                    if !config.revoke_pending.is_empty() {
                        let mut admin = Session::open(&connector)
                            .map_err(|e| format!("cannot open the library as its owner: {e}"))?;
                        revoke_pending(&mut admin, store, config)?;
                    }
                    return Ok(0);
                }
                Ok(_) | Err(ClientError::Rejected(HandshakeRejection::Unauthenticated)) => {}
                Err(e) => return Err(format!("cannot reach the library daemon: {e}")),
            }
        }
        let mut admin = Session::open(&connector)
            .map_err(|e| format!("cannot open the library as its owner: {e}"))?;
        let issued = admin
            .call(Command::Grants(GrantCommand::Issue {
                label: GRANT_LABEL.into(),
                capabilities: daemon::capture_capabilities(),
                expires_at_ms: None,
            }))
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("grant refused: {e:?}"))?;
        let Reply::Grants(GrantReply::Issued { grant, credential }) = issued else {
            return Err("unexpected daemon reply".into());
        };
        // The new grant exists now. Record it as pending revocation before its secret is
        // stored, so a failure at any later step cannot leave it live and unrecorded.
        let mut recorded = config.clone();
        recorded.revoke_pending.push(grant.id);
        if let Err(e) = store.save(&recorded) {
            let revoked = revoke(&mut admin, grant.id);
            return Err(format!(
                "cannot record the new grant ({e}); {}",
                if revoked {
                    "it was revoked again".to_owned()
                } else {
                    format!("revoke grant {} from the app", grant.id)
                }
            ));
        }
        *config = recorded;
        if let Err(e) = store.save_credential(&credential) {
            let _ = revoke_pending(&mut admin, store, config);
            return Err(format!(
                "cannot store the credential ({e}); the new grant was not kept"
            ));
        }
        config.revoke_pending.retain(|id| *id != grant.id);
        if let Some(old) = config
            .grant
            .replace(grant.id)
            .filter(|old| *old != grant.id)
        {
            config.revoke_pending.push(old);
        }
        store.save(config).map_err(|e| e.to_string())?;
        println!("paired: grant {} ({:?})", grant.id, grant.capabilities);
        revoke_pending(&mut admin, store, config)?;
        Ok(0)
    };
    run().unwrap_or_else(fail)
}

fn revoke(admin: &mut Session, grant: GrantId) -> bool {
    match admin.call(Command::Grants(GrantCommand::Revoke(grant))) {
        Ok(Ok(_)) => true,
        other => {
            eprintln!("papers-native-host: warning: could not revoke grant {grant}: {other:?}");
            false
        }
    }
}

/// Revokes `config.revoke_pending`, keeping (and saving) the ids that failed.
fn revoke_pending(
    admin: &mut Session,
    store: &Store,
    config: &mut HostConfig,
) -> Result<(), String> {
    let before = config.revoke_pending.len();
    let pending = std::mem::take(&mut config.revoke_pending);
    for id in pending {
        if revoke(admin, id) {
            println!("revoked grant {id}");
        } else {
            config.revoke_pending.push(id);
        }
    }
    if config.revoke_pending.len() != before {
        store.save(config).map_err(|e| e.to_string())?;
    }
    if config.revoke_pending.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "grant(s) {:?} still need revocation; run the command again",
            config.revoke_pending
        ))
    }
}

fn refresh(o: &Options) -> u8 {
    let run = || -> Result<u8, String> {
        let store = Store::default_location().map_err(|e| e.to_string())?;
        let Some(mut config) = store.load_optional()? else {
            return Ok(0);
        };
        if config.unregistered {
            return Ok(0);
        }
        let exe = publishable_exe(o.allow_development_build)?;
        let stale = config.host_path != exe
            || config.manifests.iter().any(|r| {
                refreshable(&config, r)
                    && manifest::recorded_host_path(&r.path).as_deref() != Some(exe.as_path())
            });
        if !stale {
            return Ok(0);
        }
        config.host_path = exe;
        write_manifests(&config, true)?;
        store.save(&config).map_err(|e| e.to_string())?;
        println!("re-registered host at {}", config.host_path.display());
        Ok(0)
    };
    run().unwrap_or_else(fail)
}

/// Removes the manifests first (browsers can no longer start the host), then revokes the
/// grant. Without revocation the configuration is kept, marked unregistered, holding the
/// grant id(s) to revoke, so nothing is left live and forgotten; run it again to retry.
/// `--keep-grant`: remove the manifests only and keep config + credential (no daemon
/// contact); a later `register` restores browser access with the same grant.
fn unregister(o: &Options) -> u8 {
    let run = || -> Result<u8, String> {
        let store = Store::default_location().map_err(|e| e.to_string())?;
        let config = store.load_optional()?;
        let home = home()?;
        let mut paths: BTreeSet<PathBuf> = Browser::ALL
            .into_iter()
            .map(|b| b.user_dirs(&home).0.join(manifest::file_name()))
            .collect();
        if let Some(config) = &config {
            paths.extend(config.manifests.iter().map(|r| r.path.clone()));
        }
        for path in paths {
            if manifest::remove(&path).map_err(|e| format!("{}: {e}", path.display()))? {
                println!("removed {}", path.display());
            }
        }
        let Some(mut config) = config else {
            store.remove().map_err(|e| e.to_string())?;
            return Ok(0);
        };
        if o.keep_grant {
            config.unregistered = true;
            store.save(&config).map_err(|e| e.to_string())?;
            println!(
                "kept the configuration and grant; `register` restores browser access, \
                 `unregister` revokes the grant"
            );
            return Ok(0);
        }
        config.revoke_pending.extend(config.grant.take());
        if !config.revoke_pending.is_empty() {
            let admin = current_exe()
                .and_then(|exe| desktop_connector(&config, &exe))
                .and_then(|c| Session::open(&c).map_err(|e| e.to_string()));
            let result = match admin {
                Ok(mut admin) => {
                    config.unregistered = true;
                    revoke_pending(&mut admin, &store, &mut config)
                }
                Err(e) => Err(format!("cannot open the library as its owner: {e}")),
            };
            if let Err(e) = result {
                // The secret is no longer needed to revoke; the ids are what must survive.
                config.unregistered = true;
                store.save(&config).map_err(|e| e.to_string())?;
                store.remove_credential().map_err(|e| e.to_string())?;
                eprintln!(
                    "papers-native-host: grant(s) {:?} not revoked: {e}. {} keeps their ids; \
                     run `papers-native-host unregister` again when the library can be opened",
                    config.revoke_pending,
                    store.config_path().display()
                );
                return Ok(1);
            }
        }
        store.remove().map_err(|e| e.to_string())?;
        Ok(0)
    };
    run().unwrap_or_else(fail)
}

fn status() -> u8 {
    let run = || -> Result<u8, String> {
        let store = Store::default_location().map_err(|e| e.to_string())?;
        let exe = current_exe()?;
        let Some(config) = store.load_optional()? else {
            println!("{}", json!({ "registered": false, "executable": exe }));
            return Ok(1);
        };
        let manifests: Vec<_> = config
            .manifests
            .iter()
            .map(|r| {
                let recorded = manifest::recorded_host_path(&r.path);
                json!({
                    "browser": r.browser,
                    "path": r.path,
                    "present": recorded.is_some(),
                    "points_to_this_executable": recorded.as_deref() == Some(exe.as_path()),
                })
            })
            .collect();
        let healthy = config.host_path == exe
            && !config.unregistered
            && config.revoke_pending.is_empty()
            && manifests
                .iter()
                .all(|m| m["points_to_this_executable"] == json!(true));
        let report = json!({
            "registered": true,
            "executable": exe,
            "host_path": config.host_path,
            "library": config.library,
            "allowed": config.allowed,
            "manifests": manifests,
            "paired": store.credential().is_ok(),
            "grant": config.grant,
            "revoke_pending": config.revoke_pending,
            "unregistered": config.unregistered,
            "healthy": healthy,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
        Ok(if healthy { 0 } else { 1 })
    };
    run().unwrap_or_else(fail)
}

fn print_manifest(o: Options) -> u8 {
    let run = || -> Result<u8, String> {
        let [(browser, None)] = o.browsers.as_slice() else {
            return Err("pass exactly one --browser NAME".into());
        };
        let host = match o.host_path.clone() {
            Some(path) => path,
            None => current_exe()?,
        };
        if host.to_str().is_none() {
            return Err("the host path must be valid UTF-8".into());
        }
        let ids = if browser.is_firefox() {
            &o.firefox
        } else {
            &o.chrome
        };
        let doc = manifest::document(*browser, &host, ids);
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?
        );
        Ok(0)
    };
    run().unwrap_or_else(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caller::{valid_chrome_id, valid_firefox_id};

    #[test]
    fn pinned_development_ids_are_well_formed() {
        assert!(valid_chrome_id(DEV_CHROME_EXTENSION_ID));
        assert!(valid_firefox_id(DEV_FIREFOX_EXTENSION_ID));
    }

    #[test]
    fn options_are_validated_per_subcommand() {
        let args = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();
        let o = parse_options(
            &args(&[
                "--library",
                "/l",
                "--browser",
                "chromium=/d",
                "--browser",
                "firefox",
                "--pair",
            ]),
            &["--library", "--browser", "--pair"],
        )
        .unwrap();
        assert_eq!(o.library, Some(PathBuf::from("/l")));
        assert_eq!(
            o.browsers,
            vec![
                (Browser::Chromium, Some(PathBuf::from("/d"))),
                (Browser::Firefox, None)
            ]
        );
        assert!(o.pair);
        for bad in [
            &["--library", "relative"][..],
            &["--browser", "safari"],
            &["--browser", "chrome=rel"],
            &["--library"],
            &["--pair"],
        ] {
            assert!(
                parse_options(&args(bad), &["--library", "--browser"]).is_err(),
                "{bad:?}"
            );
        }
    }
}
