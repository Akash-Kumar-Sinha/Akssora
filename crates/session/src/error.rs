use thiserror::Error;

#[derive(Error, Debug)]
pub enum SessionError {
    #[error("Failed to boot VM: {0}")]
    BootError(String),

    #[error("Failed to close VM: {0}")]
    CloseError(String),
    // #[error("Failed to read output: {0}")]
    // OutputReadError(String),

    // #[error("Failed to connect to Firecracker API: {0}")]
    // ApiConnectionError(String),

    // #[error("Failed to send request to Firecracker API: {0}")]
    // ApiRequestError(String),

    // #[error("Failed to receive response from Firecracker API: {0}")]
    // ApiResponseError(String),
}

pub type Result<T> = std::result::Result<T, SessionError>;
