use thiserror::Error;

#[derive(Error, Debug)]
pub enum AkssoraGuestAgentError {
    #[error("vsock bind failed: {0}")]
    VsockBind(String),

    #[error("failed to read request body ({expected} bytes expected): {source}")]
    ReadBody {
        expected: usize,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to spawn child process")]
    SpawnFailed {
        cmd: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to read request length prefix: {0}")]
    ReadLengthPrefix(#[source] std::io::Error),

    #[error("failed to write response: {0}")]
    WriteResponse(#[source] std::io::Error),

    #[error("declared body length {0} exceeds maximum allowed size")]
    BodyTooLarge(usize),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("process error: {0}")]
    Process(String),

    #[error("child process io error: {0}")]
    ChildIo(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AkssoraGuestAgentError>;
