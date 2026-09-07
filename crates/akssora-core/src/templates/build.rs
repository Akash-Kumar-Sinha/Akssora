use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;

use tokio::process::Command;
use uuid::Uuid;

use crate::error::{AkssoraCoreError, Result};

pub trait ImageBuilder: Send + Sync {
    fn build<'a>(
        &'a self,
        name: &'a str,
        dockerfile: &'a str,
        output_image_path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<u64>> + Send + 'a>>;
}

#[derive(Default, Clone)]
pub struct DockerImageBuilder;

impl ImageBuilder for DockerImageBuilder {
    fn build<'a>(
        &'a self,
        name: &'a str,
        dockerfile: &'a str,
        output_image_path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<u64>> + Send + 'a>> {
        Box::pin(TemplateBuilder::build(name, dockerfile, output_image_path))
    }
}

pub struct TemplateBuilder;

impl TemplateBuilder {
    pub async fn build(name: &str, dockerfile: &str, output_image_path: &Path) -> Result<u64> {
        let build_id = Uuid::new_v4().to_string();
        let tag = format!("akssora-template-{build_id}");
        let container_name = format!("akssora-container-{build_id}");
        let temp_dir = std::env::temp_dir().join(format!("akssora-build-{build_id}"));

        std::fs::create_dir_all(&temp_dir)?;

        let dockerfile_path = temp_dir.join("Dockerfile");
        tokio::fs::write(&dockerfile_path, dockerfile).await?;

        // CRITICAL: execute docker build isolated in temp build directory
        let build_status = Command::new("docker")
            .arg("build")
            .arg("-t")
            .arg(&tag)
            .arg("-f")
            .arg(&dockerfile_path)
            .arg(&temp_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .status()
            .await
            .map_err(|e| {
                AkssoraCoreError::TemplateBuild(format!("failed to run docker build: {e}"))
            })?;

        if !build_status.success() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return Err(AkssoraCoreError::TemplateBuild(format!(
                "docker build failed for template `{name}`"
            )));
        }

        // CRITICAL: create container from built image to export root filesystem
        let create_status = Command::new("docker")
            .arg("create")
            .arg("--name")
            .arg(&container_name)
            .arg(&tag)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status()
            .await
            .map_err(|e| {
                AkssoraCoreError::TemplateBuild(format!("failed to create container: {e}"))
            })?;

        if !create_status.success() {
            let _ = Command::new("docker").arg("rmi").arg(&tag).status().await;
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return Err(AkssoraCoreError::TemplateBuild(
                "failed to create container from template image".into(),
            ));
        }

        if let Some(parent) = output_image_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // INFO: allocate raw ext4 disk image file
        let dd_status = Command::new("dd")
            .arg("if=/dev/zero")
            .arg(format!("of={}", output_image_path.to_string_lossy()))
            .arg("bs=1M")
            .arg("count=100")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|e| {
                AkssoraCoreError::TemplateBuild(format!("failed to allocate disk: {e}"))
            })?;

        if !dd_status.success() {
            Self::cleanup_docker(&container_name, &tag, &temp_dir).await;
            return Err(AkssoraCoreError::TemplateBuild(
                "failed to allocate raw disk image".into(),
            ));
        }

        let mkfs_status = Command::new("mkfs.ext4")
            .arg("-F")
            .arg(output_image_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|e| AkssoraCoreError::TemplateBuild(format!("failed to format ext4: {e}")))?;

        if !mkfs_status.success() {
            Self::cleanup_docker(&container_name, &tag, &temp_dir).await;
            return Err(AkssoraCoreError::TemplateBuild(
                "failed to format ext4 rootfs".into(),
            ));
        }

        let mount_dir = std::env::temp_dir().join(format!("akssora-mount-{build_id}"));
        std::fs::create_dir_all(&mount_dir)?;

        let mount_status = Command::new("sudo")
            .arg("mount")
            .arg(output_image_path)
            .arg(&mount_dir)
            .status()
            .await
            .map_err(|e| AkssoraCoreError::TemplateBuild(format!("failed to mount ext4: {e}")))?;

        if !mount_status.success() {
            Self::cleanup_docker(&container_name, &tag, &temp_dir).await;
            let _ = tokio::fs::remove_dir_all(&mount_dir).await;
            return Err(AkssoraCoreError::TemplateBuild(
                "failed to mount ext4 image for rootfs export".into(),
            ));
        }

        // CRITICAL: export container rootfs directly into mounted ext4 filesystem
        let export_cmd = format!(
            "docker export {} | sudo tar -C {} -xf -",
            container_name,
            mount_dir.to_string_lossy()
        );
        let export_status = Command::new("sh")
            .arg("-c")
            .arg(&export_cmd)
            .status()
            .await
            .map_err(|e| {
                AkssoraCoreError::TemplateBuild(format!("failed to export rootfs: {e}"))
            })?;

        // CRITICAL: install guest agent binary as /akssora-guest-agent in rootfs
        let guest_agent_src = Self::find_guest_agent_binary();
        if let Some(agent_path) = guest_agent_src {
            let dest = mount_dir.join("akssora-guest-agent");
            let _ = Command::new("sudo")
                .arg("cp")
                .arg(&agent_path)
                .arg(&dest)
                .status()
                .await;
            let _ = Command::new("sudo")
                .arg("chmod")
                .arg("+x")
                .arg(&dest)
                .status()
                .await;
        }

        // INFO: create essential mountpoint directories
        let _ = Command::new("sudo")
            .arg("mkdir")
            .arg("-p")
            .arg(mount_dir.join("dev"))
            .arg(mount_dir.join("proc"))
            .arg(mount_dir.join("sys"))
            .arg(mount_dir.join("tmp"))
            .status()
            .await;

        let _ = Command::new("sudo")
            .arg("umount")
            .arg(&mount_dir)
            .status()
            .await;

        let _ = tokio::fs::remove_dir_all(&mount_dir).await;
        Self::cleanup_docker(&container_name, &tag, &temp_dir).await;

        if !export_status.success() {
            return Err(AkssoraCoreError::TemplateBuild(
                "failed to unpack exported container rootfs".into(),
            ));
        }

        let metadata = tokio::fs::metadata(output_image_path).await?;
        Ok(metadata.len())
    }

    async fn cleanup_docker(container_name: &str, tag: &str, temp_dir: &Path) {
        let _ = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .arg(container_name)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        let _ = Command::new("docker")
            .arg("rmi")
            .arg(tag)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        let _ = tokio::fs::remove_dir_all(temp_dir).await;
    }

    fn find_guest_agent_binary() -> Option<PathBuf> {
        let _ = dotenvy::dotenv();
        if let Ok(env_path) = std::env::var("AKSSORA_GUEST_AGENT_PATH") {
            let p = PathBuf::from(env_path);
            if p.exists() {
                return Some(p);
            }
        }
        let candidate_paths = [
            PathBuf::from(
                "/home/aks/vs_stuff/Development/rust_devs/akssora/target/x86_64-unknown-linux-musl/release/akssora-guest-agent",
            ),
            PathBuf::from("target/x86_64-unknown-linux-musl/release/akssora-guest-agent"),
            PathBuf::from("/usr/local/bin/akssora-guest-agent"),
        ];

        candidate_paths.into_iter().find(|path| path.exists())
    }
}
