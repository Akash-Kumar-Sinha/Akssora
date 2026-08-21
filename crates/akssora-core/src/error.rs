use thiserror::Error;

#[derive(Error, Debug)]
pub enum AkssoraCoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("failed to spawn firecracker process: {0}")]
    ProcessSpawn(String),

    #[error("failed to configure boot: {0}")]
    BootConfig(String),

    #[error("failed to configure rootfs: {0}")]
    RootfsConfig(String),
    
    #[error("machine config error: {0}")]
    MachineConfig(String), 
    
    #[error("vsock config error: {0}")]
    VsockConfig(String),

    #[error("failed to start microVM: {0}")]
    StartMicroVM(String),

    #[error("failed to kill microVM: {0}")]
    KillMicroVM(String),
    
    #[error("vsock connect failed: {0}")]
    VsockConnect(String),
    
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

}

pub type Result<T> = std::result::Result<T, AkssoraCoreError>;
