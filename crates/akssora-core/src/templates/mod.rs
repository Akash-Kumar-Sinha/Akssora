pub mod build;
pub mod official;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::Result;
pub use build::{DockerImageBuilder, ImageBuilder, TemplateBuilder};
pub use store::{TemplateRecord, TemplateStore};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TemplateSpec {
    pub name: String,
    pub dockerfile: String,
}

#[derive(Clone)]
pub struct TemplateManager {
    store: Arc<TemplateStore>,
    storage_dir: PathBuf,
    builder: Arc<dyn ImageBuilder>,
}

impl TemplateManager {
    pub fn new(store: TemplateStore, storage_dir: PathBuf) -> Self {
        Self {
            store: Arc::new(store),
            storage_dir,
            builder: Arc::new(DockerImageBuilder),
        }
    }

    pub fn with_builder(
        store: TemplateStore,
        storage_dir: PathBuf,
        builder: Arc<dyn ImageBuilder>,
    ) -> Self {
        Self {
            store: Arc::new(store),
            storage_dir,
            builder,
        }
    }

    pub fn default_manager() -> Result<Self> {
        let store = TemplateStore::open_default()?;
        let storage_dir = std::env::temp_dir().join("akssora-templates");
        Ok(Self::new(store, storage_dir))
    }

    pub async fn create_template(&self, spec: TemplateSpec) -> Result<TemplateRecord> {
        let hash = TemplateStore::compute_hash(spec.dockerfile.as_bytes());

        // CRITICAL: content-addressed deduplication check
        if let Some(existing) = self.store.get(&hash)?
            && tokio::fs::metadata(&existing.rootfs_path).await.is_ok()
        {
            return Ok(existing);
        }

        tokio::fs::create_dir_all(&self.storage_dir).await?;
        let clean_hash = hash.replace("sha256:", "");
        let image_filename = format!("{clean_hash}.ext4");
        let output_image_path = self.storage_dir.join(image_filename);

        let size_bytes = self
            .builder
            .build(&spec.name, &spec.dockerfile, &output_image_path)
            .await?;

        let record = TemplateRecord {
            id: hash,
            name: spec.name,
            rootfs_path: output_image_path,
            size_bytes,
            created_at: Utc::now(),
        };

        self.store.insert(&record)?;
        Ok(record)
    }

    pub fn get_template(&self, id: &str) -> Result<Option<TemplateRecord>> {
        self.store.get(id)
    }

    pub fn list_templates(&self) -> Result<Vec<TemplateRecord>> {
        self.store.list()
    }

    pub fn delete_template(&self, id: &str) -> Result<Option<TemplateRecord>> {
        self.store.delete(id)
    }
}
