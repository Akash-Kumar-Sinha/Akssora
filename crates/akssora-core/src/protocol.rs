use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{AkssoraCoreError, Result};

pub const MAX_FRAME_SIZE: usize = 10 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GuestRequest {
    Exec {
        cmd: String,
    },
    OpenShell,
    Input(Vec<u8>),
    CloseShell,
    WriteFileChunk {
        path: String,
        offset: u64,
        data: Vec<u8>,
        is_last: bool,
    },
    ReadFile {
        path: String,
        chunk_size: Option<u32>,
    },
    SetEnv {
        vars: std::collections::HashMap<String, String>,
    },
    ExposePort {
        guest_port: u16,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum GuestResponse {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
    Output(Vec<u8>),
    ShellClosed,
    FileWriteAck {
        path: String,
        bytes_written: u64,
    },
    ReadFileChunk {
        path: String,
        offset: u64,
        data: Vec<u8>,
        is_last: bool,
    },
    FileError {
        path: String,
        message: String,
    },
    EnvSet {
        count: usize,
    },
    PortExposed {
        guest_port: u16,
        relay_port: u16,
    },
}

pub async fn read_request<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Option<GuestRequest>> {
    let mut len_buf = [0u8; 4];

    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(error) => {
            return Err(AkssoraCoreError::ReadLengthPrefix(error));
        }
    }

    let len = u32::from_be_bytes(len_buf) as usize;

    if len > MAX_FRAME_SIZE {
        return Err(AkssoraCoreError::BodyTooLarge(len));
    }

    let mut body = vec![0u8; len];

    reader
        .read_exact(&mut body)
        .await
        .map_err(|source| AkssoraCoreError::ReadBody {
            expected: len,
            source,
        })?;

    let request = serde_json::from_slice::<GuestRequest>(&body)?;

    Ok(Some(request))
}

pub async fn write_request<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    request: &GuestRequest,
) -> Result<()> {
    let json = serde_json::to_vec(request)?;
    let len = u32::try_from(json.len()).map_err(|_| AkssoraCoreError::ResponseTooLarge)?;

    writer
        .write_all(&len.to_be_bytes())
        .await
        .map_err(AkssoraCoreError::WriteRequest)?;

    writer
        .write_all(&json)
        .await
        .map_err(AkssoraCoreError::WriteRequest)?;

    writer
        .flush()
        .await
        .map_err(AkssoraCoreError::WriteRequest)?;

    Ok(())
}

pub async fn read_response<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Option<GuestResponse>> {
    let mut len_buf = [0u8; 4];

    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Ok(None);
        }
        Err(error) => {
            return Err(AkssoraCoreError::ReadLengthPrefix(error));
        }
    }

    let len = u32::from_be_bytes(len_buf) as usize;

    if len > MAX_FRAME_SIZE {
        return Err(AkssoraCoreError::BodyTooLarge(len));
    }

    let mut body = vec![0u8; len];

    reader
        .read_exact(&mut body)
        .await
        .map_err(|source| AkssoraCoreError::ReadBody {
            expected: len,
            source,
        })?;

    let response = serde_json::from_slice::<GuestResponse>(&body)?;

    Ok(Some(response))
}

pub async fn write_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &GuestResponse,
) -> Result<()> {
    let json = serde_json::to_vec(response)?;
    let len = u32::try_from(json.len()).map_err(|_| AkssoraCoreError::ResponseTooLarge)?;

    writer
        .write_all(&len.to_be_bytes())
        .await
        .map_err(AkssoraCoreError::WriteResponse)?;

    writer
        .write_all(&json)
        .await
        .map_err(AkssoraCoreError::WriteResponse)?;

    writer
        .flush()
        .await
        .map_err(AkssoraCoreError::WriteResponse)?;

    Ok(())
}
