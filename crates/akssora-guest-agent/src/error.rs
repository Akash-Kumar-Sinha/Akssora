use akssora_core::AkssoraCoreError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AkssoraGuestAgentError {
    #[error("vsock bind failed: {0}")]
    VsockBind(String),

    #[error("protocol error: {0}")]
    Protocol(#[from] AkssoraCoreError),

    #[error("failed to spawn command `{cmd}`: {source}")]
    SpawnFailed {
        cmd: String,
        #[source]
        source: std::io::Error,
    },

    #[error("child process stdout was not piped")]
    StdoutNotPiped,

    #[error("child process stderr was not piped")]
    StderrNotPiped,

    #[error("child process I/O error: {0}")]
    ChildIo(#[from] std::io::Error),

    #[error("failed to write response: {0}")]
    WriteResponse(AkssoraCoreError),

    #[error("failed to open PTY")]
    PtyOpen(#[source] nix::errno::Errno),

    #[error("failed to clone PTY slave file descriptor")]
    PtyClone(#[source] std::io::Error),

    #[error("failed to spawn PTY child shell")]
    PtySpawn(#[source] std::io::Error),

    #[error("failed to set non-blocking flag on PTY master")]
    PtyFcntl(#[source] nix::errno::Errno),

    #[error("failed to read from PTY master")]
    PtyRead(#[source] std::io::Error),

    #[error("failed to write to PTY master")]
    PtyWrite(#[source] std::io::Error),

    #[error("failed to wait for PTY child process")]
    PtyWait(#[source] std::io::Error),

    #[error("async fd error: {0}")]
    AsyncFd(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, AkssoraGuestAgentError>;
