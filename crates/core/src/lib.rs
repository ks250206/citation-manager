//! Application contracts independent of UI, storage drivers and transports.
pub mod import;
pub mod ports;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Unavailable,
    Conflict,
    InvalidInput(String),
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("service unavailable"),
            Self::Conflict => f.write_str("state changed; preview again"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
        }
    }
}
impl std::error::Error for ServiceError {}
