//! #26 local REST API exposure settings. The listener itself is #27; the daemon stores and
//! validates these. Off by default, loopback only, unprivileged port.
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::PathBuf,
};

/// Default port when the user enables the API without choosing one.
pub const DEFAULT_PORT: u16 = 39271;
pub const MIN_PORT: u16 = 1024;

/// The only addresses the listener may bind. LAN exposure is not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BindAddress {
    #[default]
    #[serde(rename = "127.0.0.1")]
    LoopbackV4,
    #[serde(rename = "::1")]
    LoopbackV6,
}
impl BindAddress {
    pub fn ip(self) -> IpAddr {
        match self {
            Self::LoopbackV4 => IpAddr::V4(Ipv4Addr::LOCALHOST),
            Self::LoopbackV6 => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LoopbackV4 => "127.0.0.1",
            Self::LoopbackV6 => "::1",
        }
    }
    /// Accepts exactly the two loopback literals (no hostnames, no other addresses).
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "127.0.0.1" => Some(Self::LoopbackV4),
            "::1" | "[::1]" => Some(Self::LoopbackV6),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiSettings {
    pub enabled: bool,
    pub bind: BindAddress,
    pub port: u16,
    /// Directories `exports:write` tokens may export into (absolute, plain paths).
    pub export_dirs: Vec<PathBuf>,
}
impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: BindAddress::LoopbackV4,
            port: DEFAULT_PORT,
            export_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiSettingsError {
    PrivilegedPort,
    ExportDirNotAbsolute,
    TooManyExportDirs,
    DuplicateExportDir,
}
impl std::fmt::Display for ApiSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::PrivilegedPort => "port must be between 1024 and 65535",
            Self::ExportDirNotAbsolute => {
                "export folders must be absolute paths without . or .. components"
            }
            Self::TooManyExportDirs => "at most 16 export folders",
            Self::DuplicateExportDir => "export folder listed twice",
        })
    }
}

impl ApiSettings {
    pub fn validate(&self) -> Result<(), ApiSettingsError> {
        if self.port < MIN_PORT {
            return Err(ApiSettingsError::PrivilegedPort);
        }
        if self.export_dirs.len() > super::export::MAX_EXPORT_DIRS {
            return Err(ApiSettingsError::TooManyExportDirs);
        }
        for (i, dir) in self.export_dirs.iter().enumerate() {
            if !super::export::plain_absolute(dir) {
                return Err(ApiSettingsError::ExportDirNotAbsolute);
            }
            if self.export_dirs[..i].contains(dir) {
                return Err(ApiSettingsError::DuplicateExportDir);
            }
        }
        Ok(())
    }
    pub fn listen_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind.ip(), self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_loopback() {
        let s = ApiSettings::default();
        assert!(!s.enabled);
        assert!(s.listen_addr().ip().is_loopback());
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(
            serde_json::to_string(&BindAddress::LoopbackV6).unwrap(),
            "\"::1\""
        );
    }

    #[test]
    fn only_loopback_literals_and_unprivileged_ports() {
        for bad in ["0.0.0.0", "localhost", "192.168.1.2", "::", "127.0.0.2", ""] {
            assert_eq!(BindAddress::parse(bad), None, "{bad}");
        }
        assert_eq!(BindAddress::parse("[::1]"), Some(BindAddress::LoopbackV6));
        assert!(serde_json::from_str::<BindAddress>("\"0.0.0.0\"").is_err());
        let mut s = ApiSettings {
            port: 80,
            ..Default::default()
        };
        assert_eq!(s.validate(), Err(ApiSettingsError::PrivilegedPort));
        s.port = 65535;
        s.export_dirs = vec!["relative".into()];
        assert_eq!(s.validate(), Err(ApiSettingsError::ExportDirNotAbsolute));
        s.export_dirs = vec!["/a".into(), "/a".into()];
        assert_eq!(s.validate(), Err(ApiSettingsError::DuplicateExportDir));
        s.export_dirs = (0..17).map(|i| format!("/d{i}").into()).collect();
        assert_eq!(s.validate(), Err(ApiSettingsError::TooManyExportDirs));
    }
}
