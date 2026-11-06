//! Application contracts independent of UI, storage drivers and transports.
pub mod attachments;
pub mod collections;
pub mod export;
pub mod import;
pub mod library_query;
pub mod papers;
pub mod ports;
pub mod tags;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Unavailable,
    Conflict,
    NotFound,
    InvalidInput(String),
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("service unavailable"),
            Self::Conflict => f.write_str("record conflicts with existing data or state"),
            Self::NotFound => f.write_str("record not found"),
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
        }
    }
}
impl std::error::Error for ServiceError {}
