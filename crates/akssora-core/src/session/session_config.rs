use std::env;
use std::fs::{Permissions, create_dir_all, set_permissions};
use std::io::{Error, ErrorKind, Result as IoResult};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use crate::session::SessionId;

pub struct SessionConfig {
    pub rootfs_path: PathBuf,
    pub kernel_path: PathBuf,
    pub vcpu_count: u8,
    pub mem_size_mib: u32,

    pub socket_path: PathBuf,

    pub vsock_uds_path: PathBuf,
}

impl SessionConfig {
    pub fn new(session_id: SessionId) -> IoResult<Self> {
        Self::with_rootfs(session_id, None)
    }

    pub fn with_rootfs(
        session_id: SessionId,
        custom_rootfs: Option<PathBuf>,
    ) -> IoResult<Self> {
        let base = env::temp_dir().join(session_id.to_string());

        let _ = dotenvy::dotenv();

        // CRITICAL: create directory with restricted permissions (0700)
        create_dir_all(&base)?;
        set_permissions(&base, Permissions::from_mode(0o700))?;

        let rootfs_path = match custom_rootfs {
            Some(path) => {
                if !path.exists() {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        format!("Custom rootfs image not found at `{}`", path.display()),
                    ));
                }
                path
            }
            None => {
                let env_path = env::var("AKSSORA_ROOTFS_PATH").map_err(|_| {
                    Error::new(
                        ErrorKind::NotFound,
                        "AKSSORA_ROOTFS_PATH environment variable is not set. Please set it in .env or the environment",
                    )
                })?;
                let path = PathBuf::from(env_path);
                if !path.exists() {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        format!(
                            "Rootfs image not found at `{}`. Please ensure the file exists or check AKSSORA_ROOTFS_PATH in .env",
                            path.display()
                        ),
                    ));
                }
                path
            }
        };

        let kernel_path = {
            let env_path = env::var("AKSSORA_KERNEL_PATH").map_err(|_| {
                Error::new(
                    ErrorKind::NotFound,
                    "AKSSORA_KERNEL_PATH environment variable is not set. Please set it in .env or the environment",
                )
            })?;
            let path = PathBuf::from(env_path);
            if !path.exists() {
                return Err(Error::new(
                    ErrorKind::NotFound,
                    format!(
                        "Kernel image not found at `{}`. Please ensure the file exists or check AKSSORA_KERNEL_PATH in .env",
                        path.display()
                    ),
                ));
            }
            path
        };

        Ok(Self {
            rootfs_path,
            kernel_path,

            vcpu_count: 2,

            mem_size_mib: 1024,

            socket_path: base.join("firecracker.socket"),

            vsock_uds_path: base.join("vsock.socket"),
        })
    }
}
