use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AkssoraCoreError, Result};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PoolEntryStatus {
    Creating,
    Ready,
    Claimed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PoolEntryRecord {
    pub id: String,
    pub template_id: String,
    pub snapshot_path: PathBuf,
    pub mem_file_path: PathBuf,
    pub vsock_uds_path: PathBuf,
    pub status: PoolEntryStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PoolStatsRecord {
    pub hits: u64,
    pub misses: u64,
}

#[derive(Clone)]
pub struct PoolStore {
    db: sled::Db,
}

impl PoolStore {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = sled::open(path).map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        Ok(Self { db })
    }

    pub fn open_default() -> Result<Self> {
        let path = std::env::temp_dir().join("akssora-pool.db");
        Self::open(path)
    }

    pub fn insert(&self, record: &PoolEntryRecord) -> Result<()> {
        let value = serde_json::to_vec(record)?;
        self.db
            .insert(record.id.as_bytes(), value)
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<PoolEntryRecord>> {
        let maybe_bytes = self
            .db
            .get(id.as_bytes())
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        match maybe_bytes {
            Some(bytes) => {
                let record = serde_json::from_slice::<PoolEntryRecord>(&bytes)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<PoolEntryRecord>> {
        let mut list = Vec::new();

        for item in self.db.iter() {
            let (key, value) = item.map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
            // INFO: ignore internal metric and configuration keys
            if key.starts_with(b"__") {
                continue;
            }
            if let Ok(record) = serde_json::from_slice::<PoolEntryRecord>(&value) {
                list.push(record);
            }
        }

        Ok(list)
    }

    pub fn list_by_template(&self, template_id: &str) -> Result<Vec<PoolEntryRecord>> {
        let all = self.list()?;
        Ok(all
            .into_iter()
            .filter(|r| r.template_id == template_id)
            .collect())
    }

    pub fn list_available(&self, template_id: &str) -> Result<Vec<PoolEntryRecord>> {
        let all = self.list()?;
        Ok(all
            .into_iter()
            .filter(|r| r.template_id == template_id && r.status == PoolEntryStatus::Ready)
            .collect())
    }

    pub fn update_status(
        &self,
        id: &str,
        status: PoolEntryStatus,
    ) -> Result<Option<PoolEntryRecord>> {
        if let Some(mut record) = self.get(id)? {
            record.status = status;
            record.updated_at = Utc::now();
            self.insert(&record)?;
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    pub fn delete(&self, id: &str) -> Result<Option<PoolEntryRecord>> {
        let maybe_bytes = self
            .db
            .remove(id.as_bytes())
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        self.db
            .flush()
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        match maybe_bytes {
            Some(bytes) => {
                let record = serde_json::from_slice::<PoolEntryRecord>(&bytes)?;
                Ok(Some(record))
            }
            None => Ok(None),
        }
    }

    pub fn get_metrics(&self) -> Result<PoolStatsRecord> {
        let maybe_bytes = self
            .db
            .get(b"__metrics__")
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;

        match maybe_bytes {
            Some(bytes) => {
                let record = serde_json::from_slice::<PoolStatsRecord>(&bytes)?;
                Ok(record)
            }
            None => Ok(PoolStatsRecord::default()),
        }
    }

    pub fn save_metrics(&self, metrics: &PoolStatsRecord) -> Result<()> {
        let value = serde_json::to_vec(metrics)?;
        self.db
            .insert(b"__metrics__", value)
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        self.db
            .flush()
            .map_err(|e| AkssoraCoreError::Database(e.to_string()))?;
        Ok(())
    }
}
