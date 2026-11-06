//! Stable future CLI integration point. No transport, handshake or tools yet.
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StdioError {
    Unavailable,
}

impl fmt::Display for StdioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => {
                f.write_str("MCP stdio is unavailable until issue #29 is implemented")
            }
        }
    }
}
impl Error for StdioError {}

/// Future `papers mcp` entrypoint. Does not read stdin or write protocol stdout.
/// #29 will implement serving here while retaining this CLI integration boundary.
pub fn run_stdio() -> Result<(), StdioError> {
    Err(StdioError::Unavailable)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unavailable_is_an_error_not_a_fake_handshake() {
        assert_eq!(super::run_stdio(), Err(super::StdioError::Unavailable));
        assert!(super::StdioError::Unavailable.to_string().contains("#29"));
    }
}
