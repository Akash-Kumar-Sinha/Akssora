use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{AkssoraCoreError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateRecord {
    pub id: String,
    pub name: String,
    pub rootfs_path: PathBuf,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct TemplateStore {
    db: sled::Db,
}

impl TemplateStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = sled::open(path).map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        Ok(Self { db })
    }

    pub fn open_default() -> Result<Self> {
        let path = std::env::temp_dir().join("akssora-templates.db");
        Self::open(path)
    }

    pub fn compute_hash(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let result = hasher.finalize();
        format!("sha256:{:x}", result)
    }

    pub fn insert(&self, record: &TemplateRecord) -> Result<()> {
        let value = serde_json::to_vec(record)?;
        self.db
            .insert(record.id.as_bytes(), value)
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<TemplateRecord>> {
        let maybe_bytes = self
            .db
            .get(id.as_bytes())
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        match maybe_bytes {
            Some(bytes) => {
                let record = serde_json::from_slice::<TemplateRecord>(&bytes)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<TemplateRecord>> {
        let mut list = Vec::new();

        for item in self.db.iter() {
            let (_, value) = item.map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
            let record = serde_json::from_slice::<TemplateRecord>(&value)?;
            list.push(record);
        }

        Ok(list)
    }

    pub fn delete(&self, id: &str) -> Result<Option<TemplateRecord>> {
        let maybe_bytes = self
            .db
            .remove(id.as_bytes())
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        self.db
            .flush()
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        match maybe_bytes {
            Some(bytes) => {
                let record = serde_json::from_slice::<TemplateRecord>(&bytes)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }
}
