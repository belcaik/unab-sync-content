//! Per-course sync state: what has been fetched, and what failed trying.

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::io::AsyncReadExt;

/// Hash content the same way everywhere a `content_hash` is recorded: SHA-1,
/// hex-encoded lowercase. Not cryptographic — a cheap, stable fingerprint of
/// "did this content change".
pub fn content_hash(content: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(content);
    hex::encode(hasher.finalize())
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub items: BTreeMap<String, ItemState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemState {
    pub etag: Option<String>,
    pub updated_at: Option<String>,
    pub size: Option<u64>,
    pub content_hash: Option<String>,
    #[serde(default)] // For backward compatibility with existing state.json files
    pub last_error: Option<String>,
    #[serde(default)]
    pub error_count: Option<u32>,
}

impl State {
    pub async fn load(path: &Path) -> State {
        if let Ok(mut f) = tokio::fs::File::open(path).await {
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).await.is_ok() {
                if let Ok(s) = serde_json::from_slice(&buf) {
                    return s;
                }
            }
        }
        State::default()
    }

    pub async fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let tmp = path.with_extension("json.part");
        let data = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, path).await
    }

    pub fn get(&self, key: &str) -> Option<&ItemState> {
        self.items.get(key)
    }
    pub fn set(&mut self, key: String, st: ItemState) {
        self.items.insert(key, st);
    }

    /// Records a failure against `key`, incrementing its attempt count.
    ///
    /// Preserves whatever is already known about the item — a failed refresh must
    /// not discard the etag and size of the copy already on disk.
    pub fn record_error(&mut self, key: String, err: &dyn std::fmt::Display) {
        let prev = self.items.get(&key);
        let entry = ItemState {
            etag: prev.and_then(|s| s.etag.clone()),
            updated_at: prev.and_then(|s| s.updated_at.clone()),
            size: prev.and_then(|s| s.size),
            content_hash: prev.and_then(|s| s.content_hash.clone()),
            last_error: Some(err.to_string()),
            error_count: Some(prev.and_then(|s| s.error_count).unwrap_or(0) + 1),
        };
        self.items.insert(key, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced() -> ItemState {
        ItemState {
            etag: Some("abc".into()),
            updated_at: Some("2026-01-01".into()),
            size: Some(1024),
            content_hash: None,
            last_error: None,
            error_count: None,
        }
    }

    #[test]
    fn content_hash_is_sha1_hex() {
        // Known SHA-1 vectors: the output stays lowercase hex, byte-identical
        // to what the old per-module copies produced.
        assert_eq!(
            content_hash(b""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            content_hash(b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            content_hash("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".as_bytes()),
            "8b506967e3fe6f7878a37106c9c3ff102684e991"
        );
    }

    #[test]
    fn record_error_starts_the_count_at_one() {
        let mut s = State::default();
        s.record_error("file:1".into(), &"boom");
        assert_eq!(s.get("file:1").unwrap().error_count, Some(1));
    }

    #[test]
    fn record_error_increments_an_existing_count() {
        let mut s = State::default();
        s.record_error("file:1".into(), &"boom");
        s.record_error("file:1".into(), &"boom again");
        assert_eq!(s.get("file:1").unwrap().error_count, Some(2));
    }

    #[test]
    fn record_error_preserves_what_is_already_on_disk() {
        let mut s = State::default();
        s.set("file:1".into(), synced());
        s.record_error("file:1".into(), &"refresh failed");
        let got = s.get("file:1").unwrap();
        assert_eq!(got.etag.as_deref(), Some("abc"));
        assert_eq!(got.size, Some(1024));
        assert_eq!(got.last_error.as_deref(), Some("refresh failed"));
    }
}
