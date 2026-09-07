use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum VolumeError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("volume not found: {0}")]
    NotFound(Uuid),

    #[error("volume already exists: {0}")]
    AlreadyExists(Uuid),
}

pub type Result<T> = std::result::Result<T, VolumeError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeMeta {
    pub id: Uuid,
    pub sandbox_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub image_path: PathBuf,
    pub mount_path: String,
    pub size_mib: u32,
    pub created_at: DateTime<Utc>,
}

pub struct VolumeManager {
    storage_dir: PathBuf,
}

impl VolumeManager {
    pub fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    pub fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.storage_dir)?;
        Ok(())
    }

    #[tracing::instrument(skip(self), fields(size_mib = %size_mib))]
    pub fn create(
        &self,
        sandbox_id: Uuid,
        owner_id: Option<Uuid>,
        mount_path: &str,
        size_mib: u32,
    ) -> Result<VolumeMeta> {
        let id = Uuid::new_v4();
        let image_path = self.image_path(id);

        let status = std::process::Command::new("truncate")
            .args([
                "-s",
                &format!("{}M", size_mib),
                image_path.to_str().unwrap_or_default(),
            ])
            .status();

        match status {
            Ok(s) if s.success() => {
                let _ = std::process::Command::new("mkfs.ext4")
                    .arg("-F")
                    .arg(image_path.to_str().unwrap_or_default())
                    .status();
            }
            _ => {
                std::fs::write(&image_path, [])?;
            }
        }

        let meta = VolumeMeta {
            id,
            sandbox_id,
            owner_id,
            image_path: image_path.clone(),
            mount_path: mount_path.to_string(),
            size_mib,
            created_at: Utc::now(),
        };

        self.write_meta(&meta)?;
        tracing::info!(volume_id = %id, sandbox_id = %sandbox_id, "volume created");

        Ok(meta)
    }

    pub fn get(&self, id: Uuid) -> Result<VolumeMeta> {
        let meta_path = self.meta_path(id);
        if !meta_path.exists() {
            return Err(VolumeError::NotFound(id));
        }
        let data = std::fs::read_to_string(&meta_path)?;
        let meta: VolumeMeta = serde_json::from_str(&data)?;
        Ok(meta)
    }

    pub fn list_for_sandbox(&self, sandbox_id: Uuid) -> Result<Vec<VolumeMeta>> {
        let mut results = Vec::new();
        if !self.storage_dir.exists() {
            return Ok(results);
        }

        for entry in std::fs::read_dir(&self.storage_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.starts_with("volume-"))
                    .unwrap_or(false)
                && let Ok(data) = std::fs::read_to_string(&path)
                    && let Ok(meta) = serde_json::from_str::<VolumeMeta>(&data)
                        && meta.sandbox_id == sandbox_id {
                            results.push(meta);
                        }
        }

        Ok(results)
    }

    pub fn detach(&self, id: Uuid) -> Result<()> {
        let image_path = self.image_path(id);
        let meta_path = self.meta_path(id);

        if meta_path.exists() {
            std::fs::remove_file(&meta_path)?;
        }
        if image_path.exists() {
            std::fs::remove_file(&image_path)?;
        }

        tracing::info!(volume_id = %id, "volume detached");
        Ok(())
    }

    pub fn firecracker_drive_config(&self, id: &Uuid) -> Result<(String, String)> {
        let meta = self.get(*id)?;
        Ok((
            format!("vol-{}", meta.id),
            meta.image_path.to_string_lossy().into_owned(),
        ))
    }

    fn image_path(&self, id: Uuid) -> PathBuf {
        self.storage_dir.join(format!("volume-{}.ext4", id))
    }

    fn meta_path(&self, id: Uuid) -> PathBuf {
        self.storage_dir.join(format!("volume-{}.json", id))
    }

    fn write_meta(&self, meta: &VolumeMeta) -> Result<()> {
        let path = self.meta_path(meta.id);
        let json = serde_json::to_string_pretty(meta)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_manager() -> (VolumeManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = VolumeManager::new(dir.path().to_path_buf());
        mgr.ensure_dir().unwrap();
        (mgr, dir)
    }

    #[test]
    fn create_and_get_volume() {
        let (mgr, _dir) = temp_manager();
        let sandbox_id = Uuid::new_v4();

        let meta = mgr
            .create(sandbox_id, None, "/workspace", 64)
            .unwrap();
        assert_eq!(meta.sandbox_id, sandbox_id);
        assert_eq!(meta.mount_path, "/workspace");
        assert_eq!(meta.size_mib, 64);

        let loaded = mgr.get(meta.id).unwrap();
        assert_eq!(loaded, meta);
    }

    #[test]
    fn list_for_sandbox() {
        let (mgr, _dir) = temp_manager();
        let sid = Uuid::new_v4();

        mgr.create(sid, None, "/data", 32).unwrap();
        mgr.create(sid, None, "/cache", 16).unwrap();

        let volumes = mgr.list_for_sandbox(sid).unwrap();
        assert_eq!(volumes.len(), 2);
    }

    #[test]
    fn detach_removes_files() {
        let (mgr, _dir) = temp_manager();
        let meta = mgr.create(Uuid::new_v4(), None, "/ws", 8).unwrap();

        mgr.detach(meta.id).unwrap();
        assert!(matches!(mgr.get(meta.id), Err(VolumeError::NotFound(_))));
    }

    #[test]
    fn get_nonexistent_returns_not_found() {
        let (mgr, _dir) = temp_manager();
        assert!(matches!(
            mgr.get(Uuid::new_v4()),
            Err(VolumeError::NotFound(_))
        ));
    }
}
