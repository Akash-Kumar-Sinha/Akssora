use std::path::PathBuf;

use akssora_core::VmManager;
use uuid::Uuid;

use crate::error::{Result, SessionError};

type SessionId = Uuid;

pub struct Session {
    pub session_id: SessionId,
    pub vm_manager: VmManager,
}

impl Session {
    pub async fn new() -> Result<Self> {
        let session_id = Uuid::new_v4();
        let base = std::env::temp_dir().join(session_id.to_string());
        let _ = std::fs::create_dir_all(&base);

        let config = akssora_core::VmManagerConfig {
            rootfs_path: PathBuf::from(
                "/home/aks/vs_stuff/Development/rust_devs/akssora/images/rootfs.ext4",
            ),
            kernel_path: PathBuf::from(
                "/home/aks/vs_stuff/Development/rust_devs/akssora/images/vmlinux",
            ),
            vcpu_count: 2,
            mem_size_mib: 512,
            socket_path: base.join("firecracker.socket"),
            vsock_uds_path: base.join("vsock.socket"),
        };
        let vm_manager = VmManager::boot(config).await.map_err(|e| {
            SessionError::BootError(format!("Fail to boot the virtualManager {}", e))
        })?;

        Ok(Self {
            session_id: Uuid::new_v4(),
            vm_manager,
        })
    }

    pub async fn close(self) -> Result<()> {
        self.vm_manager.destroy().await.map_err(|e| {
            SessionError::CloseError(format!("Fail to destroy the virtualManager {}", e))
        })?;

        Ok(())
    }
}
