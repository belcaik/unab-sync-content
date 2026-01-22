use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::io::AsyncReadExt;

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
        let data = serde_json::to_vec_pretty(self).unwrap();
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, path).await
    }

    pub fn get(&self, key: &str) -> Option<&ItemState> {
        self.items.get(key)
    }
    pub fn set(&mut self, key: String, st: ItemState) {
        self.items.insert(key, st);
    }
}
