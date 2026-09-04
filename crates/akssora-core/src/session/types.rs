use serde::Serialize;

pub type SessionId = uuid::Uuid;

#[derive(Serialize)]
pub struct BootSource {
    pub kernel_image_path: String,
    pub boot_args: String,
}

#[derive(Serialize)]
pub struct Drive {
    pub drive_id: String,
    pub path_on_host: String,
    pub is_root_device: bool,
    pub is_read_only: bool,
}

#[derive(Serialize)]
pub struct MachineConfig {
    pub vcpu_count: u8,
    pub mem_size_mib: u32,
}

#[derive(Serialize)]
pub struct Action {
    pub action_type: String,
}

#[derive(Serialize)]
pub struct VsockConfig {
    pub guest_cid: u32,
    pub uds_path: String,
}

#[derive(Debug)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub output: Vec<u8>,
}

#[derive(Serialize)]
pub struct EntropyDevice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limiter: Option<serde_json::Value>,
}
