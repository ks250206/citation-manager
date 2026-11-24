//! Browser Native Messaging framing (Chrome and Firefox use the same wire format).
//!
//! Each message is a 4-byte length in the host's **native** byte order followed by that many
//! bytes of UTF-8 JSON. Browsers allow 64 MiB towards the host; this host accepts at most
//! [`MAX_INCOMING`]. Messages towards the browser may be at most 1 MiB ([`MAX_OUTGOING`]).
use std::io::{self, Read, Write};

/// Largest browser -> host message accepted. A capture request is at most 4 MiB of page data
/// (#22) plus JSON escaping; it must also fit the 8 MiB IPC request frame after mapping.
pub const MAX_INCOMING: u32 = 8 * 1024 * 1024;
/// Chrome and Firefox reject host -> browser messages over 1 MiB.
pub const MAX_OUTGOING: usize = 1024 * 1024;

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    /// Declared length over the limit. The stream cannot be resynchronized.
    TooLarge(u32),
    /// EOF inside a length prefix or body.
    Truncated,
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "stdin: {e}"),
            Self::TooLarge(n) => write!(f, "message of {n} bytes exceeds the host limit"),
            Self::Truncated => f.write_str("truncated message"),
        }
    }
}

/// `Ok(None)` on a clean EOF between messages (the browser closed the port).
/// Never allocates more than `max` bytes.
pub fn read_message<R: Read + ?Sized>(
    reader: &mut R,
    max: u32,
) -> Result<Option<Vec<u8>>, ReadError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(ReadError::Truncated),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ReadError::Io(e)),
        }
    }
    let length = u32::from_ne_bytes(header);
    if length > max {
        return Err(ReadError::TooLarge(length));
    }
    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => ReadError::Truncated,
        _ => ReadError::Io(e),
    })?;
    Ok(Some(body))
}

/// Writes one message. Bodies over [`MAX_OUTGOING`] are refused before any byte is written,
/// so the browser never receives a partial frame.
pub fn write_message<W: Write + ?Sized>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_OUTGOING {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native messaging response over 1 MiB",
        ));
    }
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_ne_bytes());
    frame.extend_from_slice(body);
    writer.write_all(&frame)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn framed(body: &[u8]) -> Vec<u8> {
        let mut out = (body.len() as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn native_endian_round_trip_and_clean_eof() {
        let mut out = Vec::new();
        write_message(&mut out, br#"{"v":1}"#).unwrap();
        assert_eq!(out, framed(br#"{"v":1}"#));
        assert_eq!(&out[..4], &7u32.to_ne_bytes());
        let mut input = Cursor::new([out.clone(), out].concat());
        assert_eq!(
            read_message(&mut input, 64).unwrap().unwrap(),
            br#"{"v":1}"#
        );
        assert_eq!(
            read_message(&mut input, 64).unwrap().unwrap(),
            br#"{"v":1}"#
        );
        assert!(read_message(&mut input, 64).unwrap().is_none());
    }

    #[test]
    fn oversized_declared_length_is_rejected_before_allocation() {
        let mut input = Cursor::new(u32::MAX.to_ne_bytes().to_vec());
        assert!(matches!(
            read_message(&mut input, MAX_INCOMING),
            Err(ReadError::TooLarge(u32::MAX))
        ));
        let mut input = Cursor::new(framed(&[b'x'; 65]));
        assert!(matches!(
            read_message(&mut input, 64),
            Err(ReadError::TooLarge(65))
        ));
        // Exactly at the limit is accepted.
        let mut input = Cursor::new(framed(&[b'x'; 64]));
        assert_eq!(read_message(&mut input, 64).unwrap().unwrap().len(), 64);
    }

    #[test]
    fn truncated_prefix_or_body_is_an_error() {
        assert!(matches!(
            read_message(&mut Cursor::new(vec![1u8, 0]), 64),
            Err(ReadError::Truncated)
        ));
        let mut partial = framed(b"{\"v\":1}");
        partial.truncate(6);
        assert!(matches!(
            read_message(&mut Cursor::new(partial), 64),
            Err(ReadError::Truncated)
        ));
    }

    #[test]
    fn responses_over_one_mebibyte_are_never_written() {
        let mut out = Vec::new();
        assert!(write_message(&mut out, &vec![b'a'; MAX_OUTGOING + 1]).is_err());
        assert!(out.is_empty());
        write_message(&mut out, &vec![b'a'; MAX_OUTGOING]).unwrap();
        assert_eq!(out.len(), MAX_OUTGOING + 4);
    }
}
