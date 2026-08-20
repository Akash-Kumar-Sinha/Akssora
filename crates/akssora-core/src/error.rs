use thiserror::Error;

#[derive(Error, Debug)]
pub enum AkssoraCoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("invalid utf-8 in guest output: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("vsock connect failed: {0}")]
    VsockConnect(String),

    #[error("firecracker api error: {0}")]
    FirecrackerApi(String),

    #[error("failed to spawn firecracker process: {0}")]
    ProcessSpawn(String),

    #[error("session closed unexpectedly")]
    SessionClosed,

    #[error("received unexpected guest response")]
    UnexpectedResponse,
}

pub type Result<T> = std::result::Result<T, AkssoraCoreError>;
