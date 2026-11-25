//! Length-prefixed JSON frames with an explicit per-direction size limit.
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    /// Declared or encoded length exceeds the limit; the stream must be closed.
    TooLarge(u64),
    Malformed(String),
}
impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "ipc i/o: {e}"),
            Self::TooLarge(n) => write!(f, "ipc frame of {n} bytes exceeds limit"),
            Self::Malformed(e) => write!(f, "malformed ipc frame: {e}"),
        }
    }
}
impl std::error::Error for FrameError {}
impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Encodes first so an oversized value is rejected before any byte is written.
pub fn write_frame<W: Write + ?Sized, T: Serialize>(
    writer: &mut W,
    value: &T,
    max: u32,
) -> Result<(), FrameError> {
    let body = serde_json::to_vec(value).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if body.len() as u64 > u64::from(max) {
        return Err(FrameError::TooLarge(body.len() as u64));
    }
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// `Ok(None)` on a clean EOF at a frame boundary. Never allocates beyond `max`.
pub fn read_frame<R: Read + ?Sized, T: DeserializeOwned>(
    reader: &mut R,
    max: u32,
) -> Result<Option<T>, FrameError> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < 4 {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into())),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let length = u32::from_be_bytes(header);
    if length > max {
        return Err(FrameError::TooLarge(u64::from(length)));
    }
    let mut body = vec![0u8; length as usize];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| FrameError::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_limits_and_eof() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &vec!["a"; 3], 64).unwrap();
        let mut cursor = io::Cursor::new(buffer.clone());
        assert_eq!(
            read_frame::<_, Vec<String>>(&mut cursor, 64).unwrap(),
            Some(vec!["a".to_string(); 3])
        );
        assert!(
            read_frame::<_, Vec<String>>(&mut cursor, 64)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            read_frame::<_, Vec<String>>(&mut io::Cursor::new(buffer), 4),
            Err(FrameError::TooLarge(_))
        ));
        assert!(matches!(
            write_frame(&mut Vec::new(), &"x".repeat(100), 10),
            Err(FrameError::TooLarge(_))
        ));
        let truncated = [0u8, 0, 0, 9, b'{'];
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut io::Cursor::new(truncated), 64),
            Err(FrameError::Io(_))
        ));
        let mut huge = (u32::MAX).to_be_bytes().to_vec();
        huge.push(0);
        assert!(matches!(
            read_frame::<_, serde_json::Value>(&mut io::Cursor::new(huge), 1024),
            Err(FrameError::TooLarge(_))
        ));
    }
}
