use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt, split};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::{AkssoraCoreError, Result};
use crate::firecracker::FirecrackerClient;
use crate::protocol::{GuestRequest, GuestResponse, read_response, write_request};
use crate::session::{
    Action, BootSource, Drive, EntropyDevice, ExecOutput, MachineConfig, SessionConfig, SessionId,
    VsockConfig,
};
use crate::traits::ManagedResource;

#[derive(Debug)]
pub struct Session {
    pub session_id: SessionId,
    pub process: tokio::process::Child,
    pub client: FirecrackerClient,
    pub vsock_uds_path: PathBuf,
    shell_input_tx: Option<mpsc::Sender<Vec<u8>>>,
    shell_tasks: Option<(JoinHandle<()>, JoinHandle<()>)>,
}

impl Session {
    #[tracing::instrument]
    pub async fn new() -> Result<Self> {
        Self::with_rootfs(None).await
    }

    #[tracing::instrument(skip(custom_rootfs))]
    pub async fn with_rootfs(custom_rootfs: Option<PathBuf>) -> Result<Self> {
        let session_id = Uuid::new_v4();

        let config = SessionConfig::with_rootfs(session_id, custom_rootfs)?;

        // CRITICAL: Remove any existing socket files before starting Firecracker to avoid conflicts.
        let _ = fs::remove_file(&config.socket_path);
        let _ = fs::remove_file(&config.vsock_uds_path);

        let _ = dotenvy::dotenv();
        let firecracker_bin = env::var("FIRECRACKER_BIN").map_err(|_| {
            AkssoraCoreError::StartMicroVM(
                "FIRECRACKER_BIN environment variable is not set. Please set it in .env or the environment".into(),
            )
        })?;
        if !Path::new(&firecracker_bin).exists() {
            return Err(AkssoraCoreError::StartMicroVM(format!(
                "Firecracker binary not found at `{firecracker_bin}`. Please check FIRECRACKER_BIN in .env"
            )));
        }

        let mut process = Command::new(&firecracker_bin)
            .arg("--api-sock")
            .arg(&config.socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| AkssoraCoreError::ProcessSpawn {
                command: firecracker_bin.clone(),
                source,
            })?;

        Self::wait_for_socket_ready(&mut process, &config.socket_path).await?;

        let client = FirecrackerClient::new(config.socket_path.clone());

        client
            .put(
                "/boot-source",
                &BootSource {
                    kernel_image_path: config.kernel_path.to_string_lossy().into_owned(),
                    boot_args: "console=ttyS0 reboot=k panic=1 pci=off random.trust_cpu=on systemd.hostname=akssora root=/dev/vda rw init=/akssora-guest-agent".into(),
                },
            )
            .await
            .map_err(|e| AkssoraCoreError::BootConfig(e.to_string()))?;

        client
            .put(
                "/drives/rootfs",
                &Drive {
                    drive_id: "rootfs".into(),
                    path_on_host: config.rootfs_path.to_string_lossy().into_owned(),
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

        // INFO: Attach virtio-rng device as fallback entropy source
        let _ = client
            .put("/entropy", &EntropyDevice { rate_limiter: None })
            .await;

        client
            .put(
                "/vsock",
                &VsockConfig {
                    guest_cid: 3,
                    uds_path: config.vsock_uds_path.to_string_lossy().into_owned(),
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

        Ok(Self {
            session_id,
            process,
            client,
            vsock_uds_path: config.vsock_uds_path,
            shell_input_tx: None,
            shell_tasks: None,
        })
    }

    #[tracing::instrument]
    pub async fn from_snapshot(
        session_id: SessionId,
        snapshot_path: PathBuf,
        mem_file_path: PathBuf,
        vsock_uds_path: PathBuf,
    ) -> Result<Self> {
        let base = env::temp_dir().join(session_id.to_string());
        fs::create_dir_all(&base)?;
        let socket_path = base.join("firecracker.socket");
        let _ = fs::remove_file(&socket_path);

        let _ = dotenvy::dotenv();
        let firecracker_bin = env::var("FIRECRACKER_BIN").map_err(|_| {
            AkssoraCoreError::StartMicroVM(
                "FIRECRACKER_BIN environment variable is not set. Please set it in .env or the environment".into(),
            )
        })?;
        if !Path::new(&firecracker_bin).exists() {
            return Err(AkssoraCoreError::StartMicroVM(format!(
                "Firecracker binary not found at `{firecracker_bin}`. Please check FIRECRACKER_BIN in .env"
            )));
        }

        let mut process = Command::new(&firecracker_bin)
            .arg("--api-sock")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| AkssoraCoreError::ProcessSpawn {
                command: firecracker_bin.clone(),
                source,
            })?;

        Self::wait_for_socket_ready(&mut process, &socket_path).await?;

        let client = FirecrackerClient::new(socket_path);

        crate::snapshot::load_snapshot(
            &client,
            &crate::snapshot::LoadSnapshotParams {
                snapshot_path,
                mem_file_path,
                enable_diff_snapshots: None,
                resume_vm: Some(true),
            },
        )
        .await?;

        Ok(Self {
            session_id,
            process,
            client,
            vsock_uds_path,
            shell_input_tx: None,
            shell_tasks: None,
        })
    }

    #[tracing::instrument(skip(self))]
    pub async fn pause(&mut self) -> Result<()> {
        crate::snapshot::pause_vm(&self.client).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn resume(&mut self) -> Result<()> {
        crate::snapshot::resume_vm(&self.client).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn create_snapshot(
        &self,
        snapshot_path: PathBuf,
        mem_file_path: PathBuf,
    ) -> Result<()> {
        crate::snapshot::create_snapshot(
            &self.client,
            &crate::snapshot::CreateSnapshotParams {
                snapshot_type: crate::snapshot::SnapshotType::Full,
                snapshot_path,
                mem_file_path,
            },
        )
        .await
    }

    #[tracing::instrument(skip(self))]
    pub async fn end(mut self) -> Result<()> {
        let _ = self.close_shell().await;

        self.process
            .kill()
            .await
            .map_err(AkssoraCoreError::KillMicroVM)?;

        self.process
            .wait()
            .await
            .map_err(AkssoraCoreError::KillMicroVM)?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub async fn exec_stream(&self, cmd: &str) -> Result<mpsc::Receiver<GuestResponse>> {
        let mut stream = self.connect_to_guest().await?;

        let request = GuestRequest::Exec {
            cmd: cmd.to_owned(),
        };

        write_request(&mut stream, &request).await?;

        let (tx, rx) = mpsc::channel(64);

        tokio::spawn(async move {
            while let Ok(Some(response)) = read_response(&mut stream).await {
                let is_terminal = matches!(
                    response,
                    GuestResponse::Exit(_) | GuestResponse::ShellClosed
                );
                if tx.send(response).await.is_err() {
                    break;
                }
                if is_terminal {
                    break;
                }
            }
        });

        Ok(rx)
    }

    #[tracing::instrument(skip(self))]
    pub async fn exec(&self, cmd: &str) -> Result<ExecOutput> {
        let mut rx = self.exec_stream(cmd).await?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut output = Vec::new();
        let mut exit_code = -1;

        while let Some(response) = rx.recv().await {
            match response {
                GuestResponse::Stdout(bytes) => {
                    stdout.extend(bytes);
                }
                GuestResponse::Stderr(bytes) => {
                    stderr.extend(bytes);
                }
                GuestResponse::Output(bytes) => {
                    output.extend(bytes);
                }
                GuestResponse::ShellClosed => {
                    exit_code = 0;
                    break;
                }
                GuestResponse::Exit(code) => {
                    exit_code = code;
                    break;
                }
                GuestResponse::FileWriteAck { .. }
                | GuestResponse::ReadFileChunk { .. }
                | GuestResponse::FileError { .. }
                | GuestResponse::EnvSet { .. }
                | GuestResponse::PortExposed { .. } => {}
            }
        }

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
            output,
        })
    }

    #[tracing::instrument(skip(self, data))]
    pub async fn write_file(&self, path: &str, data: &[u8]) -> Result<u64> {
        let mut stream = self.connect_to_guest().await?;
        crate::files::write_file_chunks(&mut stream, path, data).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let mut stream = self.connect_to_guest().await?;
        crate::files::read_file_chunks(&mut stream, path).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn open_shell(&mut self) -> Result<mpsc::Receiver<Vec<u8>>> {
        let _ = self.close_shell().await;

        let (input_tx, output_rx, write_task, read_task) = self.create_shell_connection().await?;

        self.shell_input_tx = Some(input_tx);
        self.shell_tasks = Some((write_task, read_task));

        Ok(output_rx)
    }

    pub async fn open_shell_channels(
        &self,
    ) -> Result<(mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>)> {
        let (input_tx, output_rx, _write_task, _read_task) = self.create_shell_connection().await?;

        Ok((input_tx, output_rx))
    }

    async fn create_shell_connection(
        &self,
    ) -> Result<(
        mpsc::Sender<Vec<u8>>,
        mpsc::Receiver<Vec<u8>>,
        JoinHandle<()>,
        JoinHandle<()>,
    )> {
        let mut stream = self.connect_to_guest().await?;

        write_request(&mut stream, &GuestRequest::OpenShell).await?;

        let (read_half, mut write_half) = split(stream);

        let (input_tx, mut input_rx) = mpsc::channel::<Vec<u8>>(128);
        let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>(128);

        let write_task = tokio::spawn(async move {
            while let Some(bytes) = input_rx.recv().await {
                if write_request(&mut write_half, &GuestRequest::Input(bytes))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // INFO: input channel closed, send CloseShell request to guest
            let _ = write_request(&mut write_half, &GuestRequest::CloseShell).await;
        });

        let read_task = tokio::spawn(async move {
            let mut read_half = read_half;
            while let Ok(Some(response)) = read_response(&mut read_half).await {
                match response {
                    GuestResponse::Output(data) => {
                        if output_tx.send(data).await.is_err() {
                            break;
                        }
                    }
                    GuestResponse::Stdout(data) | GuestResponse::Stderr(data) => {
                        if output_tx.send(data).await.is_err() {
                            break;
                        }
                    }
                    GuestResponse::ShellClosed | GuestResponse::Exit(_) => {
                        break;
                    }
                    GuestResponse::FileWriteAck { .. }
                    | GuestResponse::ReadFileChunk { .. }
                    | GuestResponse::FileError { .. }
                    | GuestResponse::EnvSet { .. }
                    | GuestResponse::PortExposed { .. } => {}
                }
            }
        });

        Ok((input_tx, output_rx, write_task, read_task))
    }

    pub async fn send_input(&self, data: &[u8]) -> Result<()> {
        let sender = self
            .shell_input_tx
            .as_ref()
            .ok_or(AkssoraCoreError::ShellNotOpen)?;

        sender
            .send(data.to_vec())
            .await
            .map_err(|error| AkssoraCoreError::ShellSend(error.to_string()))?;

        Ok(())
    }

    pub async fn close_shell(&mut self) -> Result<()> {
        self.shell_input_tx.take();

        if let Some((write_task, read_task)) = self.shell_tasks.take() {
            write_task.abort();
            read_task.abort();
        }

        Ok(())
    }

    pub fn is_shell_open(&self) -> bool {
        self.shell_input_tx.is_some()
    }

    async fn connect_to_guest(&self) -> Result<UnixStream> {
        let timeout_duration = Duration::from_secs(10);
        let start = Instant::now();

        loop {
            if let Ok(mut stream) = UnixStream::connect(&self.vsock_uds_path).await
                && stream.write_all(b"CONNECT 1024\n").await.is_ok()
            {
                let mut response_bytes = Vec::new();
                let mut byte = [0u8; 1];

                while stream.read_exact(&mut byte).await.is_ok() {
                    response_bytes.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }

                if response_bytes.starts_with(b"OK") {
                    return Ok(stream);
                }
            }

            if start.elapsed() > timeout_duration {
                return Err(AkssoraCoreError::VsockConnect(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Timed out waiting for guest agent to become ready on vsock port 1024",
                )));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_for_socket_ready(
        process: &mut tokio::process::Child,
        socket_path: &Path,
    ) -> Result<()> {
        let start = Instant::now();
        let timeout = Duration::from_secs(2);
        let poll_interval = Duration::from_millis(5);

        loop {
            // CRITICAL: Fail fast if the Firecracker process exited prematurely
            if let Some(status) = process.try_wait().map_err(AkssoraCoreError::Io)? {
                return Err(AkssoraCoreError::StartMicroVM(format!(
                    "Firecracker process exited prematurely with status: {status}"
                )));
            }

            // INFO: Check whether Unix domain socket is actively accepting connections
            if UnixStream::connect(socket_path).await.is_ok() {
                break;
            }

            if start.elapsed() >= timeout {
                let _ = process.kill().await;
                return Err(AkssoraCoreError::StartMicroVM(
                    "Timed out waiting for Firecracker API socket to be ready".into(),
                ));
            }

            tokio::time::sleep(poll_interval).await;
        }

        Ok(())
    }
}

impl ManagedResource for Session {
    type Id = SessionId;

    fn id(&self) -> Self::Id {
        self.session_id
    }

    fn is_alive(&self) -> bool {
        self.process.id().is_some()
    }
}
