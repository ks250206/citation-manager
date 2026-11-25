//! Binary payloads (page bitmaps) carried next to the JSON of a frame instead of as base64
//! text inside it (#85, protocol version 2).
//!
//! A field marked `#[serde(with = "crate::ipc::blob")]` serializes, while `write_frame`
//! encodes a frame, as `{"blob": <index>}` and its bytes travel unencoded in the frame's
//! binary section (see `frame`). Everywhere else (REST, MCP, logs, tests) it is a standard
//! base64 string, so the JSON form of these types is unchanged. Decoding accepts both; a
//! blob reference resolves only against the binary section of the frame being read, and
//! every blob of a frame must be referenced exactly once.
use super::b64;
use serde::{
    Deserializer, Serializer,
    de::{self, MapAccess, Visitor},
    ser::SerializeMap,
};
use std::{cell::RefCell, sync::Arc};

/// Shared, immutable pixel data: the daemon hands its rendered buffer to the frame writer
/// and the GUI takes ownership of the received one without copying.
pub type Bytes = Arc<Vec<u8>>;

/// Field name of a blob reference in the JSON part of a binary frame.
const KEY: &str = "blob";

thread_local! {
    /// Blobs collected while `write_frame` serializes on this thread.
    static SINK: RefCell<Option<Vec<Bytes>>> = const { RefCell::new(None) };
    /// Blobs of the frame `read_frame` is decoding on this thread; taken when referenced.
    static SOURCE: RefCell<Option<Vec<Option<Bytes>>>> = const { RefCell::new(None) };
}

/// Restores the previous thread-local state even if (de)serialization panics.
struct Restore<T: 'static>(
    &'static std::thread::LocalKey<RefCell<Option<T>>>,
    Option<T>,
);
impl<T: 'static> Drop for Restore<T> {
    fn drop(&mut self) {
        let previous = self.1.take();
        self.0.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// Runs `encode` with blob collection on; returns its result and the collected blobs.
pub(crate) fn collect<R>(encode: impl FnOnce() -> R) -> (R, Vec<Bytes>) {
    let previous = SINK.with(|cell| cell.borrow_mut().replace(Vec::new()));
    let restore = Restore(&SINK, previous);
    let result = encode();
    let blobs = SINK
        .with(|cell| cell.borrow_mut().take())
        .unwrap_or_default();
    drop(restore);
    (result, blobs)
}

/// Runs `decode` with `blobs` available for references; also returns how many were left
/// unreferenced (the caller rejects such a frame).
pub(crate) fn provide<R>(blobs: Vec<Bytes>, decode: impl FnOnce() -> R) -> (R, usize) {
    let previous = SOURCE.with(|cell| {
        cell.borrow_mut()
            .replace(blobs.into_iter().map(Some).collect())
    });
    let restore = Restore(&SOURCE, previous);
    let result = decode();
    let unused = SOURCE.with(|cell| {
        cell.borrow_mut()
            .take()
            .map_or(0, |left| left.iter().filter(|b| b.is_some()).count())
    });
    drop(restore);
    (result, unused)
}

pub fn serialize<S: Serializer>(data: &Bytes, serializer: S) -> Result<S::Ok, S::Error> {
    let index = SINK.with(|cell| {
        cell.borrow_mut().as_mut().map(|blobs| {
            blobs.push(data.clone());
            blobs.len() - 1
        })
    });
    match index {
        Some(index) => {
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(KEY, &index)?;
            map.end()
        }
        None => serializer.serialize_str(&b64::encode(data)),
    }
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Bytes, D::Error> {
    deserializer.deserialize_any(BytesVisitor)
}

struct BytesVisitor;
impl<'de> Visitor<'de> for BytesVisitor {
    type Value = Bytes;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("base64 text or a blob reference")
    }
    fn visit_str<E: de::Error>(self, text: &str) -> Result<Bytes, E> {
        b64::decode(text)
            .map(Arc::new)
            .ok_or_else(|| E::custom("invalid base64"))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Bytes, A::Error> {
        let Some((key, index)) = map.next_entry::<String, usize>()? else {
            return Err(de::Error::custom("empty blob reference"));
        };
        if key != KEY || map.next_key::<de::IgnoredAny>()?.is_some() {
            return Err(de::Error::custom("malformed blob reference"));
        }
        SOURCE
            .with(|cell| {
                cell.borrow_mut()
                    .as_mut()
                    .and_then(|blobs| blobs.get_mut(index))
                    .and_then(Option::take)
            })
            .ok_or_else(|| de::Error::custom("blob reference without a matching blob"))
    }
}
