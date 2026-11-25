//! Length-prefixed frames with an explicit per-direction size limit.
//!
//! Every frame is a 4-byte big-endian length `L` followed by `L` bytes of body. The limit
//! applies to `L`, the whole body. Two body forms exist (protocol version 2):
//!
//! - JSON: UTF-8 JSON text (always begins with `{` or `"`). Handshake frames always use
//!   this form, so a version-1 peer still reads a `VersionMismatch` rejection.
//! - Binary (first byte `BINARY_TAG`, 0x00), for values with `blob` fields (page bitmaps):
//!   `0x00 | json_len: u32 | count: u32 | count x blob_len: u32 | json | blob 0 | blob 1 ...`
//!   (all integers big-endian). The JSON refers to blob `i` as `{"blob": i}`. The parts
//!   must add up to exactly `L`, `count` is at most `MAX_BLOBS`, and every blob must be
//!   referenced exactly once; anything else is `Malformed`.
use super::blob;
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

/// First body byte of a binary frame (never the first byte of JSON text).
pub const BINARY_TAG: u8 = 0;
/// Blobs one frame may carry.
pub const MAX_BLOBS: usize = 8;

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
fn malformed(reason: impl Into<String>) -> FrameError {
    FrameError::Malformed(reason.into())
}

/// Encodes first so an oversized value is rejected before any byte is written. Blob
/// payloads are written from their own buffers, never copied into the frame.
pub fn write_frame<W: Write + ?Sized, T: Serialize>(
    writer: &mut W,
    value: &T,
    max: u32,
) -> Result<(), FrameError> {
    let (json, blobs) = blob::collect(|| serde_json::to_vec(value));
    let json = json.map_err(|e| malformed(e.to_string()))?;
    if blobs.is_empty() {
        if json.len() as u64 > u64::from(max) {
            return Err(FrameError::TooLarge(json.len() as u64));
        }
        let mut frame = Vec::with_capacity(json.len() + 4);
        frame.extend_from_slice(&(json.len() as u32).to_be_bytes());
        frame.extend_from_slice(&json);
        writer.write_all(&frame)?;
        writer.flush()?;
        return Ok(());
    }
    if blobs.len() > MAX_BLOBS {
        return Err(malformed("too many binary payloads in one frame"));
    }
    let header = 9 + 4 * blobs.len();
    let total = (header + json.len()) as u64 + blobs.iter().map(|b| b.len() as u64).sum::<u64>();
    if total > u64::from(max) {
        return Err(FrameError::TooLarge(total));
    }
    let mut head = Vec::with_capacity(4 + header + json.len());
    head.extend_from_slice(&(total as u32).to_be_bytes());
    head.push(BINARY_TAG);
    head.extend_from_slice(&(json.len() as u32).to_be_bytes());
    head.extend_from_slice(&(blobs.len() as u32).to_be_bytes());
    for b in &blobs {
        head.extend_from_slice(&(b.len() as u32).to_be_bytes());
    }
    head.extend_from_slice(&json);
    writer.write_all(&head)?;
    for b in &blobs {
        writer.write_all(b)?;
    }
    writer.flush()?;
    Ok(())
}

fn read_u32<R: Read + ?Sized>(reader: &mut R) -> Result<u32, FrameError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

/// `Ok(None)` on a clean EOF at a frame boundary. Never allocates beyond `max`: a binary
/// frame's parts are checked against the declared length before any blob is allocated, and
/// each blob is read straight into the buffer the decoded value keeps.
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
    if length == 0 {
        return Err(malformed("empty frame"));
    }
    let mut tag = [0u8; 1];
    reader.read_exact(&mut tag)?;
    if tag[0] != BINARY_TAG {
        let mut body = vec![0u8; length as usize];
        body[0] = tag[0];
        reader.read_exact(&mut body[1..])?;
        return serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| malformed(e.to_string()));
    }
    // Binary frame: the remaining `length - 1` bytes are fully read before any error that
    // depends only on their content, so a rejected frame never desynchronizes the stream
    // (the connection is closed on any FrameError anyway).
    let mut rest = u64::from(length) - 1;
    if rest < 8 {
        return Err(malformed("truncated binary frame header"));
    }
    let json_len = read_u32(reader)?;
    let count = read_u32(reader)? as usize;
    rest -= 8;
    if count == 0 || count > MAX_BLOBS || rest < 4 * count as u64 {
        return Err(malformed("invalid binary payload count"));
    }
    let mut sizes = Vec::with_capacity(count);
    for _ in 0..count {
        sizes.push(read_u32(reader)?);
    }
    rest -= 4 * count as u64;
    let declared = u64::from(json_len) + sizes.iter().map(|s| u64::from(*s)).sum::<u64>();
    if declared != rest {
        return Err(malformed("binary frame parts do not match its length"));
    }
    let mut json = vec![0u8; json_len as usize];
    reader.read_exact(&mut json)?;
    let mut blobs = Vec::with_capacity(count);
    for size in sizes {
        let mut data = vec![0u8; size as usize];
        reader.read_exact(&mut data)?;
        blobs.push(std::sync::Arc::new(data));
    }
    let (value, unused) = blob::provide(blobs, || serde_json::from_slice(&json));
    let value = value.map_err(|e| malformed(e.to_string()))?;
    if unused != 0 {
        return Err(malformed("unreferenced binary payload"));
    }
    Ok(Some(value))
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

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Pixels {
        name: String,
        #[serde(with = "crate::ipc::blob")]
        a: blob::Bytes,
        #[serde(with = "crate::ipc::blob")]
        b: blob::Bytes,
    }
    fn pixels() -> Pixels {
        Pixels {
            name: "page".into(),
            a: std::sync::Arc::new((0..=255).collect()),
            b: std::sync::Arc::new(Vec::new()),
        }
    }
    /// Frame bytes from parts, for hand-made (hostile) binary frames.
    fn binary(json: &[u8], blobs: &[&[u8]], sizes: Option<&[u32]>, extra: &[u8]) -> Vec<u8> {
        let mut body = vec![BINARY_TAG];
        body.extend_from_slice(&(json.len() as u32).to_be_bytes());
        let lens: Vec<u32> = match sizes {
            Some(s) => s.to_vec(),
            None => blobs.iter().map(|b| b.len() as u32).collect(),
        };
        body.extend_from_slice(&(lens.len() as u32).to_be_bytes());
        for l in &lens {
            body.extend_from_slice(&l.to_be_bytes());
        }
        body.extend_from_slice(json);
        for b in blobs {
            body.extend_from_slice(b);
        }
        body.extend_from_slice(extra);
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);
        frame
    }

    #[test]
    fn blob_fields_travel_as_raw_bytes_and_stay_base64_elsewhere() {
        let value = pixels();
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &value, 4096).unwrap();
        // The pixels are not base64 text inside the frame: the JSON part only references them.
        assert_eq!(buffer[4], BINARY_TAG);
        let text = String::from_utf8_lossy(&buffer);
        assert!(text.contains(r#""a":{"blob":0}"#), "{text}");
        assert!(text.contains(r#""b":{"blob":1}"#), "{text}");
        assert_eq!(
            u32::from_be_bytes(buffer[..4].try_into().unwrap()) as usize,
            buffer.len() - 4
        );
        let mut cursor = io::Cursor::new(buffer.clone());
        assert_eq!(
            read_frame::<_, Pixels>(&mut cursor, 4096).unwrap(),
            Some(pixels())
        );
        assert!(
            read_frame::<_, Pixels>(&mut cursor, 4096)
                .unwrap()
                .is_none()
        );
        // The size limit covers the binary payloads too, on both sides.
        assert!(matches!(
            write_frame(&mut Vec::new(), &value, 256),
            Err(FrameError::TooLarge(_))
        ));
        assert!(matches!(
            read_frame::<_, Pixels>(&mut io::Cursor::new(buffer), 256),
            Err(FrameError::TooLarge(_))
        ));
        // Outside a frame (REST, MCP, logs) the JSON form is unchanged base64.
        let json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["b"], "");
        assert_eq!(json["a"], crate::ipc::b64::encode(&value.a));
        assert_eq!(
            serde_json::from_value::<Pixels>(json.clone()).unwrap(),
            pixels()
        );
        // A version-1 style JSON frame with base64 text is still decoded.
        let mut v1 = Vec::new();
        write_frame(&mut v1, &json, 4096).unwrap();
        assert_ne!(v1[4], BINARY_TAG);
        assert_eq!(
            read_frame::<_, Pixels>(&mut io::Cursor::new(v1), 4096).unwrap(),
            Some(pixels())
        );
    }

    #[test]
    fn malformed_binary_frames_are_rejected_without_overallocation() {
        let json = br#"{"name":"p","a":{"blob":0},"b":{"blob":1}}"#;
        let ok = binary(json, &[b"xy", b""], None, b"");
        assert!(read_frame::<_, Pixels>(&mut io::Cursor::new(ok), 1024).is_ok());
        let cases: Vec<(&str, Vec<u8>)> = vec![
            // Parts that do not add up to the frame length, either way.
            ("short", binary(json, &[b"xy", b""], Some(&[3, 0]), b"")),
            ("long", binary(json, &[b"xy", b""], None, b"!")),
            // A declared blob far larger than the frame is refused before allocation.
            (
                "huge",
                binary(json, &[b"xy", b""], Some(&[u32::MAX, 0]), b""),
            ),
            ("no blobs", binary(json, &[], None, b"")),
            (
                "too many",
                binary(json, &[b"x".as_slice(); MAX_BLOBS + 1], None, b""),
            ),
            // References must resolve, once each, and every payload must be used.
            (
                "dangling",
                binary(
                    br#"{"name":"p","a":{"blob":0},"b":{"blob":7}}"#,
                    &[b"x", b""],
                    None,
                    b"",
                ),
            ),
            (
                "twice",
                binary(
                    br#"{"name":"p","a":{"blob":0},"b":{"blob":0}}"#,
                    &[b"x"],
                    None,
                    b"",
                ),
            ),
            ("unused", binary(json, &[b"x", b"", b"z"], None, b"")),
            (
                "bad key",
                binary(
                    br#"{"name":"p","a":{"blob":0,"x":1},"b":{"blob":1}}"#,
                    &[b"x", b""],
                    None,
                    b"",
                ),
            ),
            ("truncated header", {
                let mut f = 3u32.to_be_bytes().to_vec();
                f.extend_from_slice(&[BINARY_TAG, 0, 0]);
                f
            }),
            ("empty", 0u32.to_be_bytes().to_vec()),
        ];
        for (name, frame) in cases {
            assert!(
                matches!(
                    read_frame::<_, Pixels>(&mut io::Cursor::new(frame), 1024),
                    Err(FrameError::Malformed(_))
                ),
                "{name}"
            );
        }
        // A reference inside a plain JSON frame has nothing to resolve against.
        let mut plain = Vec::new();
        write_frame(
            &mut plain,
            &serde_json::json!({"name": "p", "a": {"blob": 0}, "b": ""}),
            1024,
        )
        .unwrap();
        assert!(matches!(
            read_frame::<_, Pixels>(&mut io::Cursor::new(plain), 1024),
            Err(FrameError::Malformed(_))
        ));
        // A truncated payload is an I/O error, not a hang or a partial value.
        let mut cut = binary(json, &[b"xy", b""], None, b"");
        cut.pop();
        assert!(matches!(
            read_frame::<_, Pixels>(&mut io::Cursor::new(cut), 1024),
            Err(FrameError::Io(_))
        ));
    }
}
