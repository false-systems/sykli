//! Strict JSON decoding, canonical serialization, domain-separated identities
//! and immutable file publication. Shared by typed production and candidate
//! assessment; existing identity domains never change.
use serde::Serialize;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub static NONCE: AtomicU64 = AtomicU64::new(0);

fn err(e: impl fmt::Display) -> String {
    e.to_string()
}

/// Milliseconds since the Unix epoch; saturates instead of failing.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

// serde_json::Value normally keeps the last duplicate key. Reject duplicates
// before deserializing any identity-bearing document, including nested maps.
struct Unique(Value);
impl<'de> serde::Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys or floating-point identities")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Unique, M::Error> {
                let mut values = serde_json::Map::new();
                while let Some((key, value)) = map.next_entry::<String, Unique>()? {
                    if values.insert(key.clone(), value.0).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
                    }
                }
                Ok(Unique(Value::Object(values)))
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Unique, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<Unique>()? {
                    values.push(value.0);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(v.into())))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Decode JSON, rejecting duplicate keys and floating-point numbers.
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let value: Unique = serde_json::from_slice(bytes).map_err(err)?;
    serde_json::from_value(value.0).map_err(err)
}

/// Canonical bytes: sorted object keys, no insignificant whitespace.
pub fn canonical(value: &impl Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec(&serde_json::to_value(value).map_err(err)?).map_err(err)
}

/// Domain-separated SHA-256 over the canonical form. Each schema has its own domain.
pub fn identity(domain: &str, value: &impl Serialize) -> Result<String, String> {
    let mut bytes = domain.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend(canonical(value)?);
    Ok(crate::sha256(&bytes))
}

pub fn digest(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("invalid SHA-256 identity {value:?}"));
    }
    Ok(())
}

/// Write an immutable file: a durable temporary followed by a hard link, so a
/// torn write never becomes a record. An existing identical file is accepted;
/// a differing one is a conflict.
pub fn publish(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("missing parent")?;
    fs::create_dir_all(parent).map_err(err)?;
    let temporary = parent.join(format!(
        ".tmp-{}-{}-{}",
        std::process::id(),
        now(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(err)?;
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(err);
    let linked = written.and_then(|()| match fs::hard_link(&temporary, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match fs::read(path) {
            Ok(existing) if existing == bytes => Ok(()),
            Ok(_) => Err(format!("conflicting immutable record: {}", path.display())),
            Err(e) => Err(err(e)),
        },
        Err(e) => Err(err(e)),
    });
    // The temporary never outlives this call, whatever happened.
    let _ = fs::remove_file(&temporary);
    linked?;
    // Make the directory entry durable. Windows cannot open a directory for
    // flushing without backup semantics, and NTFS journals the metadata.
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(err)?;
    Ok(())
}
