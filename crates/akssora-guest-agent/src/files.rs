use std::path::{Component, Path, PathBuf};

use akssora_core::protocol::{GuestResponse, write_response};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};

use crate::error::{AkssoraGuestAgentError, Result};

pub fn resolve_and_validate_path(
    raw_path: &str,
    base_dir: Option<&Path>,
) -> std::result::Result<PathBuf, String> {
    if raw_path.contains('\0') {
        return Err("path contains null byte".into());
    }

    let base = base_dir.unwrap_or_else(|| Path::new("/"));
    let raw = Path::new(raw_path);

    // CRITICAL: normalize path components to prevent directory traversal escapes
    let mut normalized_components = Vec::new();

    let combined = if raw.is_absolute() {
        if base != Path::new("/") && !raw.starts_with(base) {
            return Err(format!(
                "path traversal detected: `{raw_path}` is outside sandbox root `{}`",
                base.display()
            ));
        }
        raw.to_path_buf()
    } else {
        base.join(raw)
    };

    for component in combined.components() {
        match component {
            Component::Prefix(prefix) => {
                normalized_components.push(Component::Prefix(prefix));
            }
            Component::RootDir => {
                normalized_components.clear();
                normalized_components.push(Component::RootDir);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = normalized_components.last() {
                    if *last == Component::RootDir {
                        return Err(format!(
                            "path traversal detected: `{raw_path}` escapes sandbox root"
                        ));
                    }
                    normalized_components.pop();
                } else {
                    return Err(format!(
                        "path traversal detected: `{raw_path}` escapes sandbox root"
                    ));
                }
            }
            Component::Normal(c) => {
                normalized_components.push(Component::Normal(c));
            }
        }
    }

    let normalized: PathBuf = normalized_components.iter().collect();

    if !normalized.starts_with(base) {
        return Err(format!(
            "path traversal detected: `{}` is outside sandbox root `{}`",
            normalized.display(),
            base.display()
        ));
    }

    // CRITICAL: verify canonical prefix if base directory exists
    if let Ok(canonical_base) = std::fs::canonicalize(base) {
        if normalized.exists() {
            if let Ok(canonical_target) = std::fs::canonicalize(&normalized)
                && !canonical_target.starts_with(&canonical_base)
            {
                return Err(format!(
                    "path traversal detected: target `{}` escapes base `{}`",
                    canonical_target.display(),
                    canonical_base.display()
                ));
            }
        } else {
            // INFO: check closest existing ancestor within base
            let mut ancestor = normalized.parent();
            while let Some(dir) = ancestor {
                if dir.starts_with(base)
                    && dir.exists()
                    && let Ok(canonical_dir) = std::fs::canonicalize(dir)
                    && !canonical_dir.starts_with(&canonical_base)
                {
                    return Err(format!(
                        "path traversal detected: ancestor `{}` escapes base `{}`",
                        canonical_dir.display(),
                        canonical_base.display()
                    ));
                }
                ancestor = dir.parent();
            }
        }
    }

    Ok(normalized)
}

pub async fn handle_write_file_chunk<S: AsyncWriteExt + Unpin>(
    stream: &mut S,
    path: &str,
    offset: u64,
    data: &[u8],
    is_last: bool,
) -> Result<()> {
    let resolved_path = match resolve_and_validate_path(path, None) {
        Ok(p) => p,
        Err(err_msg) => {
            write_response(
                stream,
                &GuestResponse::FileError {
                    path: path.to_owned(),
                    message: err_msg,
                },
            )
            .await
            .map_err(AkssoraGuestAgentError::WriteResponse)?;
            return Ok(());
        }
    };

    if let Some(parent) = resolved_path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        write_response(
            stream,
            &GuestResponse::FileError {
                path: path.to_owned(),
                message: format!("failed to create parent directory: {e}"),
            },
        )
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
        return Ok(());
    }

    let file_result = tokio::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(offset == 0 && is_last && data.is_empty())
        .open(&resolved_path)
        .await;

    let mut file = match file_result {
        Ok(f) => f,
        Err(e) => {
            write_response(
                stream,
                &GuestResponse::FileError {
                    path: path.to_owned(),
                    message: format!("failed to open file for writing: {e}"),
                },
            )
            .await
            .map_err(AkssoraGuestAgentError::WriteResponse)?;
            return Ok(());
        }
    };

    if offset > 0
        && let Err(e) = file.seek(SeekFrom::Start(offset)).await
    {
        write_response(
            stream,
            &GuestResponse::FileError {
                path: path.to_owned(),
                message: format!("failed to seek in file: {e}"),
            },
        )
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
        return Ok(());
    }

    if !data.is_empty()
        && let Err(e) = file.write_all(data).await
    {
        write_response(
            stream,
            &GuestResponse::FileError {
                path: path.to_owned(),
                message: format!("failed to write chunk to file: {e}"),
            },
        )
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
        return Ok(());
    }

    if is_last {
        let _ = file.flush().await;
        let bytes_written = match file.metadata().await {
            Ok(meta) => meta.len(),
            Err(_) => offset + data.len() as u64,
        };

        write_response(
            stream,
            &GuestResponse::FileWriteAck {
                path: path.to_owned(),
                bytes_written,
            },
        )
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
    }

    Ok(())
}

pub async fn handle_read_file<S: AsyncWriteExt + Unpin>(
    stream: &mut S,
    path: &str,
    chunk_size: Option<u32>,
) -> Result<()> {
    let resolved_path = match resolve_and_validate_path(path, None) {
        Ok(p) => p,
        Err(err_msg) => {
            write_response(
                stream,
                &GuestResponse::FileError {
                    path: path.to_owned(),
                    message: err_msg,
                },
            )
            .await
            .map_err(AkssoraGuestAgentError::WriteResponse)?;
            return Ok(());
        }
    };

    let mut file = match tokio::fs::File::open(&resolved_path).await {
        Ok(f) => f,
        Err(e) => {
            write_response(
                stream,
                &GuestResponse::FileError {
                    path: path.to_owned(),
                    message: format!("failed to open file `{path}`: {e}"),
                },
            )
            .await
            .map_err(AkssoraGuestAgentError::WriteResponse)?;
            return Ok(());
        }
    };

    let buffer_size = chunk_size.unwrap_or(64 * 1024) as usize;
    let mut buffer = vec![0u8; buffer_size];
    let mut offset = 0u64;

    let meta = file.metadata().await;
    let total_len = meta.map(|m| m.len()).unwrap_or(0);

    if total_len == 0 {
        write_response(
            stream,
            &GuestResponse::ReadFileChunk {
                path: path.to_owned(),
                offset: 0,
                data: Vec::new(),
                is_last: true,
            },
        )
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
        return Ok(());
    }

    loop {
        let n = match file.read(&mut buffer).await {
            Ok(n) => n,
            Err(e) => {
                write_response(
                    stream,
                    &GuestResponse::FileError {
                        path: path.to_owned(),
                        message: format!("failed to read file `{path}`: {e}"),
                    },
                )
                .await
                .map_err(AkssoraGuestAgentError::WriteResponse)?;
                return Ok(());
            }
        };

        let is_last = n == 0 || (offset + n as u64 >= total_len);
        let chunk_data = buffer[..n].to_vec();

        write_response(
            stream,
            &GuestResponse::ReadFileChunk {
                path: path.to_owned(),
                offset,
                data: chunk_data,
                is_last,
            },
        )
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;

        offset += n as u64;

        if is_last || n == 0 {
            break;
        }
    }

    Ok(())
}
