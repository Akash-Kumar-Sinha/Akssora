use crate::error::{AkssoraCoreError, Result};
use crate::firecracker_client::FirecrackerClient;
use crate::protocol::{GuestRequest, GuestResponse};
use serde::Serialize;
use std::{
    path::PathBuf,
    process::{Child, Stdio},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct VmManagerConfig {
    pub rootfs_path: PathBuf,
    pub kernel_path: PathBuf,
    pub vcpu_count: u8,
    pub mem_size_mib: u32,
    pub socket_path: PathBuf,
    pub vsock_uds_path: PathBuf,
}

#[derive(Serialize)]
struct BootSource {
    kernel_image_path: String,
    boot_args: String,
}

#[derive(Serialize)]
struct Drive {
    drive_id: String,
    path_on_host: String,
    is_root_device: bool,
    is_read_only: bool,
}

#[derive(Serialize)]
struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u32,
}

#[derive(Serialize)]
struct Action {
    action_type: String,
}

#[derive(Serialize)]
struct VsockConfig {
    guest_cid: u32,
    uds_path: String,
}

pub struct VmManager {
    pub process: Child,
    pub client: FirecrackerClient,
    pub vsock_uds_path: PathBuf,
}

pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

impl VmManager {
    pub async fn boot(config: VmManagerConfig) -> Result<Self> {
        let _ = std::fs::remove_file(&config.socket_path);
        let _ = std::fs::remove_file(&config.vsock_uds_path);

        let process = std::process::Command::new("/home/aks/./firecracker")
            .arg("--api-sock")
            .arg(&config.socket_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AkssoraCoreError::ProcessSpawn(e.to_string()))?;

        std::thread::sleep(std::time::Duration::from_millis(100));

        let client = FirecrackerClient::new(config.socket_path);

        client
            .put(
                "/boot-source",
                &BootSource {
                    kernel_image_path: config.kernel_path.to_string_lossy().into(),
                    boot_args: "console=ttyS0 reboot=k panic=1 pci=off root=/dev/vda rw init=/akssora-guest-agent".to_string(),
                },
            )
            .await
            .map_err(|e| AkssoraCoreError::BootConfig(e.to_string()))?;

        client
            .put(
                "/drives/rootfs",
                &Drive {
                    drive_id: "rootfs".into(),
                    path_on_host: config.rootfs_path.to_string_lossy().into(),
                    is_root_device: true,
                    is_read_only: false,
                },
            )
            .await
            .map_err(|e| AkssoraCoreError::RootfsConfig(e.to_string()))?;

        client
            .put(
                "/machine-config",
                &MachineConfig {
                    vcpu_count: config.vcpu_count,
                    mem_size_mib: config.mem_size_mib,
                },
            )
            .await
            .map_err(|e| AkssoraCoreError::MachineConfig(e.to_string()))?;

        client
            .put(
                "/vsock",
                &VsockConfig {
                    guest_cid: 3,
                    uds_path: config.vsock_uds_path.to_string_lossy().into(),
                },
            )
            .await
            .map_err(|e| AkssoraCoreError::VsockConfig(e.to_string()))?;

        client
            .put(
                "/actions",
                &Action {
                    action_type: "InstanceStart".into(),
                },
            )
            .await
            .map_err(|e| AkssoraCoreError::StartMicroVM(e.to_string()))?;

        Ok(VmManager {
            vsock_uds_path: config.vsock_uds_path,
            process,
            client,
        })
    }

    pub async fn destroy(mut self) -> Result<()> {
        self.process
            .kill()
            .map_err(|e| AkssoraCoreError::KillMicroVM(e.to_string()))?;
        self.process
            .wait()
            .map_err(|e| AkssoraCoreError::KillMicroVM(e.to_string()))?;
        Ok(())
    }

    pub async fn exec(&self, cmd: &str) -> Result<ExecOutput> {
        let stream = UnixStream::connect(&self.vsock_uds_path).await.map_err(|e| AkssoraCoreError::VsockConnect(e.to_string()))?;
        let mut reader = BufReader::new(stream);

        reader.get_mut().write_all(b"CONNECT 1024\n").await?;
        let mut response_line = String::new();
        reader.read_line(&mut response_line).await?;
        if !response_line.starts_with("OK") {
            return Err(AkssoraCoreError::VsockConnect(response_line));
        }

        let req = GuestRequest::Exec {
            cmd: cmd.to_string(),
        };
        let json = serde_json::to_vec(&req)?;
        let len = (json.len() as u32).to_be_bytes();
        reader.get_mut().write_all(&len).await?;
        reader.get_mut().write_all(&json).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code;

        loop {
            let mut len_buf = [0u8; 4];
            reader.read_exact(&mut len_buf).await?;
            let len = u32::from_be_bytes(len_buf) as usize;

            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).await?;

            let resp: GuestResponse = serde_json::from_slice(&body)?;
            match resp {
                GuestResponse::Stdout(bytes) => stdout.extend(bytes),
                GuestResponse::Stderr(bytes) => stderr.extend(bytes),
                GuestResponse::Exit(code) => {
                    exit_code = code;
                    break;
                } // _ => return Err(AkssoraCoreError::UnexpectedResponse),
            }
        }

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }
}
