use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{AkssoraCoreError, Result};
use crate::protocol::{GuestRequest, GuestResponse, read_response, write_request};

pub const DEFAULT_FILE_CHUNK_SIZE: usize = 64 * 1024;

pub async fn write_file_chunks<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
    path: &str,
    data: &[u8],
) -> Result<u64> {
    if data.is_empty() {
        let request = GuestRequest::WriteFileChunk {
            path: path.to_owned(),
            offset: 0,
            data: Vec::new(),
            is_last: true,
        };
        write_request(stream, &request).await?;

        match read_response(stream).await? {
            Some(GuestResponse::FileWriteAck { bytes_written, .. }) => Ok(bytes_written),
            Some(GuestResponse::FileError { message, .. }) => Err(AkssoraCoreError::FileError {
                path: path.to_owned(),
                message,
            }),
            Some(other) => Err(AkssoraCoreError::GuestAgent(format!(
                "unexpected response to WriteFileChunk: {other:?}"
            ))),
            None => Err(AkssoraCoreError::GuestAgent(
                "connection closed during file write".into(),
            )),
        }
    } else {
        let mut offset = 0;
        let mut total_written = 0;

        for chunk in data.chunks(DEFAULT_FILE_CHUNK_SIZE) {
            let is_last = offset + chunk.len() >= data.len();
            let request = GuestRequest::WriteFileChunk {
                path: path.to_owned(),
                offset: offset as u64,
                data: chunk.to_vec(),
                is_last,
            };

            write_request(stream, &request).await?;
            offset += chunk.len();

            if is_last {
                match read_response(stream).await? {
                    Some(GuestResponse::FileWriteAck { bytes_written, .. }) => {
                        total_written = bytes_written;
                    }
                    Some(GuestResponse::FileError { message, .. }) => {
                        return Err(AkssoraCoreError::FileError {
                            path: path.to_owned(),
                            message,
                        });
                    }
                    Some(other) => {
                        return Err(AkssoraCoreError::GuestAgent(format!(
                            "unexpected response to WriteFileChunk: {other:?}"
                        )));
                    }
                    None => {
                        return Err(AkssoraCoreError::GuestAgent(
                            "connection closed during file write".into(),
                        ));
                    }
                }
            }
        }

        Ok(total_written)
    }
}

pub async fn read_file_chunks<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
    path: &str,
) -> Result<Vec<u8>> {
    let request = GuestRequest::ReadFile {
        path: path.to_owned(),
        chunk_size: Some(DEFAULT_FILE_CHUNK_SIZE as u32),
    };

    write_request(stream, &request).await?;

    let mut file_data = Vec::new();

    while let Some(response) = read_response(stream).await? {
        match response {
            GuestResponse::ReadFileChunk { data, is_last, .. } => {
                file_data.extend(data);
                if is_last {
                    break;
                }
            }
            GuestResponse::FileError { message, .. } => {
                return Err(AkssoraCoreError::FileError {
                    path: path.to_owned(),
                    message,
                });
            }
            other => {
                return Err(AkssoraCoreError::GuestAgent(format!(
                    "unexpected response to ReadFile: {other:?}"
                )));
            }
        }
    }

    Ok(file_data)
}
