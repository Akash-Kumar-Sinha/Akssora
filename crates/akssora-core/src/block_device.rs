
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum BlockDeviceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("command failed: {0}")]
    CommandFailed(String),

    #[error("unsupported backend: {0}")]
    UnsupportedBackend(String),

    #[error("overlay setup failed: {0}")]
    OverlaySetup(String),
}

pub type Result<T> = std::result::Result<T, BlockDeviceError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum StorageBackend {
    FlatFile,
    #[default]
    SparseFile,
    CowOverlay,
    RawBlock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockDeviceConfig {
    pub backend: StorageBackend,
    pub image_path: PathBuf,
    pub size_mib: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_image_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_path: Option<PathBuf>,
    pub multi_queue: bool,
    pub queue_depth: u32,
}

impl BlockDeviceConfig {
    pub fn flat_file(image_path: PathBuf, size_mib: u32) -> Self {
        Self {
            backend: StorageBackend::FlatFile,
            image_path,
            size_mib,
            base_image_path: None,
            device_path: None,
            multi_queue: false,
            queue_depth: 128,
        }
    }

    pub fn sparse_file(image_path: PathBuf, size_mib: u32) -> Self {
        Self {
            backend: StorageBackend::SparseFile,
            image_path,
            size_mib,
            base_image_path: None,
            device_path: None,
            multi_queue: false,
            queue_depth: 128,
        }
    }

    pub fn cow_overlay(
        overlay_path: PathBuf,
        base_image_path: PathBuf,
        size_mib: u32,
    ) -> Self {
        Self {
            backend: StorageBackend::CowOverlay,
            image_path: overlay_path,
            size_mib,
            base_image_path: Some(base_image_path),
            device_path: None,
            multi_queue: false,
            queue_depth: 128,
        }
    }

    pub fn raw_block(device_path: PathBuf, size_mib: u32) -> Self {
        Self {
            backend: StorageBackend::RawBlock,
            image_path: device_path.clone(),
            size_mib,
            base_image_path: None,
            device_path: Some(device_path),
            multi_queue: true,
            queue_depth: 128,
        }
    }

    pub fn with_multi_queue(mut self, enabled: bool) -> Self {
        self.multi_queue = enabled;
        self
    }

    pub fn with_queue_depth(mut self, depth: u32) -> Self {
        self.queue_depth = depth;
        self
    }
}

pub struct BlockDeviceManager {
    storage_dir: PathBuf,
    base_images_dir: PathBuf,
    default_backend: StorageBackend,
}

impl BlockDeviceManager {
    pub fn new(storage_dir: PathBuf, base_images_dir: PathBuf) -> Self {
        Self {
            storage_dir,
            base_images_dir,
            default_backend: StorageBackend::default(),
        }
    }

    pub fn with_backend(mut self, backend: StorageBackend) -> Self {
        self.default_backend = backend;
        self
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.storage_dir)?;
        std::fs::create_dir_all(&self.base_images_dir)?;
        Ok(())
    }

    #[tracing::instrument(skip(self), fields(size_mib = %size_mib))]
    pub fn create_rootfs(
        &self,
        sandbox_id: Uuid,
        size_mib: u32,
    ) -> Result<BlockDeviceConfig> {
        match self.default_backend {
            StorageBackend::FlatFile => self.create_flat_file(sandbox_id, size_mib),
            StorageBackend::SparseFile => self.create_sparse_file(sandbox_id, size_mib),
            StorageBackend::CowOverlay => self.create_cow_overlay(sandbox_id, size_mib),
            StorageBackend::RawBlock => Err(BlockDeviceError::UnsupportedBackend(
                "raw block device requires manual setup".into(),
            )),
        }
    }

    pub fn destroy_rootfs(&self, config: &BlockDeviceConfig) -> Result<()> {
        match config.backend {
            StorageBackend::CowOverlay => {
                let upper_dir = config.image_path.join("upper");
                let work_dir = config.image_path.join("work");
                let merged_dir = config.image_path.join("merged");

                let _ = std::process::Command::new("sudo")
                    .args(["umount", "-l"])
                    .arg(&merged_dir)
                    .status();

                let _ = std::fs::remove_dir_all(&upper_dir);
                let _ = std::fs::remove_dir_all(&work_dir);
                let _ = std::fs::remove_dir_all(&merged_dir);
                let _ = std::fs::remove_dir_all(&config.image_path);
            }
            _ => {
                if config.image_path.exists() {
                    std::fs::remove_file(&config.image_path)?;
                }
            }
        }
        Ok(())
    }

    pub fn firecracker_drive(&self, config: &BlockDeviceConfig) -> (String, String, bool, bool) {
        match config.backend {
            StorageBackend::CowOverlay => {
                let base = config
                    .base_image_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| config.image_path.to_string_lossy().into_owned());
                ("rootfs".to_string(), base, true, false)
            }
            _ => {
                (
                    "rootfs".to_string(),
                    config.image_path.to_string_lossy().into_owned(),
                    true,
                    false,
                )
            }
        }
    }

    fn create_flat_file(&self, sandbox_id: Uuid, size_mib: u32) -> Result<BlockDeviceConfig> {
        let image_path = self.image_path(sandbox_id);

        let status = std::process::Command::new("dd")
            .args(["if=/dev/zero", &format!("of={}", image_path.to_string_lossy())])
            .args(["bs=1M", &size_mib.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return Err(BlockDeviceError::CommandFailed("dd failed".into()));
        }

        let status = std::process::Command::new("mkfs.ext4")
            .arg("-F")
            .arg(&image_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return Err(BlockDeviceError::CommandFailed("mkfs.ext4 failed".into()));
        }

        tracing::info!(backend = "flat_file", size_mib, "rootfs created");

        Ok(BlockDeviceConfig::flat_file(image_path, size_mib))
    }

    fn create_sparse_file(&self, sandbox_id: Uuid, size_mib: u32) -> Result<BlockDeviceConfig> {
        let image_path = self.image_path(sandbox_id);

        let status = std::process::Command::new("fallocate")
            .args(["-l", &format!("{}M", size_mib)])
            .arg(&image_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return self.create_flat_file(sandbox_id, size_mib);
        }

        let status = std::process::Command::new("mkfs.ext4")
            .arg("-F")
            .arg(&image_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return Err(BlockDeviceError::CommandFailed("mkfs.ext4 failed".into()));
        }

        tracing::info!(backend = "sparse_file", size_mib, "rootfs created");

        Ok(BlockDeviceConfig::sparse_file(image_path, size_mib))
    }

    fn create_cow_overlay(&self, sandbox_id: Uuid, size_mib: u32) -> Result<BlockDeviceConfig> {
        let overlay_dir = self.storage_dir.join(format!("overlay-{}", sandbox_id));
        let upper_dir = overlay_dir.join("upper");
        let work_dir = overlay_dir.join("work");
        let merged_dir = overlay_dir.join("merged");

        std::fs::create_dir_all(&upper_dir)?;
        std::fs::create_dir_all(&work_dir)?;
        std::fs::create_dir_all(&merged_dir)?;

        let base_image_path = self.find_base_image().ok_or_else(|| {
            BlockDeviceError::OverlaySetup("no base image found in base_images_dir".into())
        })?;

        let lower_arg = format!("lowerdir={}", base_image_path.to_string_lossy());
        let upper_arg = format!("upperdir={}", upper_dir.to_string_lossy());
        let work_arg = format!("workdir={}", work_dir.to_string_lossy());

        let status = std::process::Command::new("sudo")
            .args(["mount", "-t", "overlay", "overlay"])
            .args(["-o", &format!("{},{},{}", lower_arg, upper_arg, work_arg)])
            .arg(&merged_dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;

        if !status.success() {
            return Err(BlockDeviceError::OverlaySetup(
                "mount -t overlay failed".into(),
            ));
        }

        tracing::info!(
            backend = "cow_overlay",
            base = %base_image_path.display(),
            sandbox_id = %sandbox_id,
            "COW overlay created"
        );

        Ok(BlockDeviceConfig::cow_overlay(
            overlay_dir,
            base_image_path,
            size_mib,
        ))
    }

    fn image_path(&self, sandbox_id: Uuid) -> PathBuf {
        self.storage_dir.join(format!("rootfs-{}.ext4", sandbox_id))
    }

    fn find_base_image(&self) -> Option<PathBuf> {
        if !self.base_images_dir.exists() {
            return None;
        }
        std::fs::read_dir(&self.base_images_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path()).find(|p| p.extension().and_then(|e| e.to_str()) == Some("ext4"))
    }
}

pub fn generate_fio_job_file(output_path: &Path, test_dir: &str) -> Result<()> {
    let job = format!(
        r#"[global]
directory={test_dir}
ioengine=libaio
direct=1
size=64m
time_based
runtime=10

[seq-write]
bs=128k
rw=write
iodepth=32
name=seq-write

[rand-read-4k]
bs=4k
rw=randread
iodepth=64
name=rand-read-4k

[rand-write-4k]
bs=4k
rw=randwrite
iodepth=32
name=rand-write-4k
"#,
        test_dir = test_dir
    );

    std::fs::write(output_path, job)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serialization() {
        let config = BlockDeviceConfig::sparse_file(PathBuf::from("/tmp/test.ext4"), 256);
        let json = serde_json::to_string(&config).unwrap();
        let parsed: BlockDeviceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.backend, StorageBackend::SparseFile);
        assert_eq!(parsed.size_mib, 256);
    }

    #[test]
    fn cow_overlay_config() {
        let config = BlockDeviceConfig::cow_overlay(
            PathBuf::from("/tmp/overlay"),
            PathBuf::from("/tmp/base.ext4"),
            512,
        );
        assert_eq!(config.backend, StorageBackend::CowOverlay);
        assert!(config.base_image_path.is_some());
    }

    #[test]
    fn firecracker_drive_returns_correct_tuple() {
        let mgr = BlockDeviceManager::new(
            PathBuf::from("/tmp/akssora"),
            PathBuf::from("/tmp/akssora/base"),
        );
        let config = BlockDeviceConfig::sparse_file(PathBuf::from("/tmp/test.ext4"), 128);
        let (id, path, is_root, is_ro) = mgr.firecracker_drive(&config);
        assert_eq!(id, "rootfs");
        assert_eq!(path, "/tmp/test.ext4");
        assert!(is_root);
        assert!(!is_ro);
    }

    #[test]
    fn fio_job_file_generation() {
        let dir = tempfile::tempdir().unwrap();
        let job_path = dir.path().join("test.fio");
        generate_fio_job_file(&job_path, "/mnt/test").unwrap();
        let content = std::fs::read_to_string(&job_path).unwrap();
        assert!(content.contains("ioengine=libaio"));
        assert!(content.contains("direct=1"));
        assert!(content.contains("randread"));
    }

    #[test]
    fn default_backend_is_sparse_file() {
        assert_eq!(StorageBackend::default(), StorageBackend::SparseFile);
    }
}
