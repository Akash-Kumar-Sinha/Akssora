use thiserror::Error;

use crate::session::SessionId;

#[derive(Debug, Error)]
pub enum AkssoraCoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to read request length prefix")]
    ReadLengthPrefix(#[source] std::io::Error),

    #[error("failed to read request body ({expected} bytes expected)")]
    ReadBody {
        expected: usize,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write response")]
    WriteResponse(#[source] std::io::Error),

    #[error("failed to write request")]
    WriteRequest(#[source] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("failed to spawn process `{command}`")]
    ProcessSpawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to spawn child process `{cmd}`")]
    SpawnFailed {
        cmd: String,
        #[source]
        source: std::io::Error,
    },

    #[error("child process stdout was not piped")]
    StdoutNotPiped,

    #[error("child process stderr was not piped")]
    StderrNotPiped,

    #[error("failed to configure boot: {0}")]
    BootConfig(String),

    #[error("failed to configure root filesystem: {0}")]
    RootfsConfig(String),

    #[error("failed to configure machine: {0}")]
    MachineConfig(String),

    #[error("failed to configure vsock: {0}")]
    VsockConfig(String),

    #[error("failed to start microVM: {0}")]
    StartMicroVM(String),

    #[error("failed to boot microVM: {0}")]
    Boot(String),

    #[error("failed to close microVM: {0}")]
    Close(String),

    #[error("failed to kill microVM: {0}")]
    Kill(String),

    #[error("failed to bind vsock")]
    VsockBind(#[source] std::io::Error),

    #[error("failed to connect to vsock")]
    VsockConnect(#[source] std::io::Error),

    #[error("failed to spawn PTY process")]
    PtySpawn(#[source] std::io::Error),

    #[error("failed to open PTY master")]
    PtyOpen(#[source] nix::errno::Errno),

    #[error("failed to clone PTY slave")]
    PtyClone(#[source] std::io::Error),

    #[error("failed to read from PTY master")]
    PtyRead(#[source] std::io::Error),

    #[error("failed to write to PTY master")]
    PtyWrite(#[source] std::io::Error),

    #[error("PTY blocking task failed")]
    PtyTask(#[source] tokio::task::JoinError),

    #[error("partial write to PTY master: wrote {written} of {expected} bytes")]
    PtyPartialWrite { written: usize, expected: usize },

    #[error("failed to wait for PTY child process")]
    PtyWait(#[source] std::io::Error),

    #[error("failed to communicate with Akssora Guest Agent: {0}")]
    GuestAgent(String),

    #[error("failed to kill microVM")]
    KillMicroVM(#[source] std::io::Error),

    #[error("failed to wait for microVM process")]
    WaitMicroVM(#[source] std::io::Error),

    #[error("request body length {0} exceeds maximum allowed size")]
    BodyTooLarge(usize),

    #[error("failed to connect to Firecracker API")]
    FirecrackerConnect(std::io::Error),

    #[error("failed to perform Firecracker handshake")]
    FirecrackerHandshake(hyper::Error),

    #[error("failed to build request")]
    RequestBuild(hyper::http::Error),

    #[error("failed to send request to Firecracker API")]
    FirecrackerRequest(hyper::Error),

    #[error("Firecracker API returned an error: {0}")]
    FirecrackerApi(u16),

    #[error("response body length exceeds maximum allowed size")]
    ResponseTooLarge,

    #[error("session not found")]
    SessionNotFound(SessionId),

    #[error("failed to lock process: {0}")]
    LockProcess(String),

    #[error("shell session is not open")]
    ShellNotOpen,

    #[error("failed to send input to shell session: {0}")]
    ShellSend(String),

    #[error("guest file operation failed for `{path}`: {message}")]
    FileError { path: String, message: String },

    #[error("path traversal detected: `{0}`")]
    PathTraversal(String),

    #[error("template build failed: {0}")]
    TemplateBuild(String),

    #[error("template `{0}` not found")]
    TemplateNotFound(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("failed to pause microVM: {0}")]
    PauseMicroVM(String),

    #[error("failed to resume microVM: {0}")]
    ResumeMicroVM(String),

    #[error("failed to create snapshot: {0}")]
    SnapshotCreate(String),

    #[error("failed to load snapshot: {0}")]
    SnapshotLoad(String),

    #[error("warm pool error: {0}")]
    Pool(String),
}

pub type Result<T> = std::result::Result<T, AkssoraCoreError>;
