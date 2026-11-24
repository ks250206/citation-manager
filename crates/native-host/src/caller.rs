//! Who launched the host. The browser enforces the manifest's `allowed_origins` /
//! `allowed_extensions`; the host checks the caller again against its own configured
//! allowlist (defence in depth, DESIGN §12.3). Neither check is daemon authority: the daemon
//! grant decides what any request may do.
//!
//! - Chrome/Chromium: `argv[1] = "chrome-extension://<id>/"`; on Windows also
//!   `--parent-window=<handle>`.
//! - Firefox: `argv[1] = <absolute path of the host manifest>`, `argv[2] = <extension id>`.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
};

const CHROME_ORIGIN_PREFIX: &str = "chrome-extension://";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Chrome {
        extension_id: String,
    },
    Firefox {
        extension_id: String,
        manifest: PathBuf,
    },
}
impl Caller {
    pub fn browser(&self) -> &'static str {
        match self {
            Self::Chrome { .. } => "chrome",
            Self::Firefox { .. } => "firefox",
        }
    }
}

/// Chrome extension IDs: 32 characters `a`..=`p` (base-16 of the key hash, shifted).
pub fn valid_chrome_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Firefox (gecko) add-on IDs: `{GUID}` or an email-like `name@domain`, at most 80 bytes.
pub fn valid_firefox_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 80 {
        return false;
    }
    if let Some(inner) = id.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let groups: Vec<&str> = inner.split('-').collect();
        return groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
            && groups
                .iter()
                .all(|g| g.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    let Some((local, domain)) = id.split_once('@') else {
        return false;
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    };
    ok(local) && ok(domain) && !domain.contains('@')
}

/// Parses the browser-supplied arguments (without argv[0]). `None` for anything that is not
/// exactly one of the two documented shapes.
pub fn parse(args: &[OsString]) -> Option<Caller> {
    let args: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    match args.as_slice() {
        [origin, rest @ ..] if origin.starts_with(CHROME_ORIGIN_PREFIX) => {
            let id = origin
                .strip_prefix(CHROME_ORIGIN_PREFIX)?
                .strip_suffix('/')?;
            let windows_only = rest.iter().all(|a| {
                a.strip_prefix("--parent-window=")
                    .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_digit()))
            });
            (valid_chrome_id(id) && rest.len() <= 1 && windows_only).then(|| Caller::Chrome {
                extension_id: id.to_owned(),
            })
        }
        [manifest, id] => {
            let manifest = Path::new(manifest);
            (manifest.is_absolute()
                && manifest.extension().is_some_and(|e| e == "json")
                && valid_firefox_id(id))
            .then(|| Caller::Firefox {
                extension_id: (*id).to_owned(),
                manifest: manifest.to_owned(),
            })
        }
        _ => None,
    }
}

/// Extension IDs this installation accepts, written by `register`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    pub chrome: BTreeSet<String>,
    pub firefox: BTreeSet<String>,
}
impl Allowlist {
    pub fn allows(&self, caller: &Caller) -> bool {
        match caller {
            Caller::Chrome { extension_id } => self.chrome.contains(extension_id),
            Caller::Firefox { extension_id, .. } => self.firefox.contains(extension_id),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Some(bad) = self.chrome.iter().find(|id| !valid_chrome_id(id)) {
            return Err(format!("invalid Chrome extension ID: {bad:?}"));
        }
        if let Some(bad) = self.firefox.iter().find(|id| !valid_firefox_id(id)) {
            return Err(format!("invalid Firefox extension ID: {bad:?}"));
        }
        if self.chrome.is_empty() && self.firefox.is_empty() {
            return Err("at least one allowed extension ID is required".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn chrome_origin_and_windows_parent_window() {
        let origin = format!("chrome-extension://{ID}/");
        assert_eq!(
            parse(&args(&[&origin])),
            Some(Caller::Chrome {
                extension_id: ID.into()
            })
        );
        assert!(parse(&args(&[&origin, "--parent-window=1234"])).is_some());
        for bad in [
            format!("chrome-extension://{ID}"),
            format!("chrome-extension://{}/", &ID[..31]),
            format!("chrome-extension://{}q/", &ID[..31]),
            format!("chrome-extension://{}/", ID.to_uppercase()),
            format!("chrome-extension://{ID}/x"),
            format!("moz-extension://{ID}/"),
        ] {
            assert_eq!(parse(&args(&[&bad])), None, "{bad}");
        }
        assert_eq!(parse(&args(&[&origin, "--parent-window=x"])), None);
        assert_eq!(parse(&args(&[&origin, "--other"])), None);
        assert_eq!(
            parse(&args(&[&origin, "--parent-window=1", "--parent-window=2"])),
            None
        );
    }

    #[test]
    fn firefox_manifest_path_and_gecko_id() {
        let caller = parse(&args(&[
            "/home/u/.mozilla/native-messaging-hosts/dev.citation_manager.native_host.json",
            "citation-manager-dev@citation-manager.invalid",
        ]))
        .unwrap();
        assert_eq!(caller.browser(), "firefox");
        assert!(
            parse(&args(&[
                "/m.json",
                "{12345678-1234-1234-1234-123456789abc}"
            ]))
            .is_some()
        );
        for (manifest, id) in [
            ("relative.json", "a@b"),
            ("/m.txt", "a@b"),
            ("/m.json", "no-at-sign"),
            ("/m.json", "a@b@c"),
            ("/m.json", "a b@c"),
            ("/m.json", "{1234-5678}"),
            ("/m.json", ""),
        ] {
            assert_eq!(parse(&args(&[manifest, id])), None, "{manifest} {id}");
        }
        assert!(!valid_firefox_id(&format!("{}@x", "a".repeat(80))));
        assert_eq!(parse(&args(&[])), None);
        assert_eq!(parse(&args(&["register", "x", "y"])), None);
    }

    #[test]
    fn allowlist_matches_per_browser_only() {
        let allow = Allowlist {
            chrome: [ID.to_owned()].into(),
            firefox: ["a@b".to_owned()].into(),
        };
        assert!(allow.validate().is_ok());
        assert!(allow.allows(&Caller::Chrome {
            extension_id: ID.into()
        }));
        assert!(!allow.allows(&Caller::Chrome {
            extension_id: "p".repeat(32)
        }));
        // A Firefox caller cannot borrow a Chrome entry and vice versa.
        assert!(!allow.allows(&Caller::Firefox {
            extension_id: ID.into(),
            manifest: "/m.json".into()
        }));
        assert!(allow.allows(&Caller::Firefox {
            extension_id: "a@b".into(),
            manifest: "/m.json".into()
        }));
        assert!(Allowlist::default().validate().is_err());
        let bad = Allowlist {
            chrome: ["UPPER".into()].into(),
            ..Allowlist::default()
        };
        assert!(bad.validate().is_err());
    }
}
