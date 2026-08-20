use crate::error::{AkssoraGuestAgentError, Result};
use akssora_core::protocol::{GuestRequest, GuestResponse};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener, VsockStream};

mod error;

const MAX_BODY_SIZE: usize = 10 * 1024 * 1024; // 10MB

#[tokio::main]
async fn main() -> Result<()> {
    let addr = VsockAddr::new(VMADDR_CID_ANY, 1024);
    let listener =
        VsockListener::bind(addr).map_err(|e| AkssoraGuestAgentError::VsockBind(e.to_string()))?;

    println!("akssora-guest-agent listening on vsock port 1024");

    loop {
        let (mut stream, peer_addr) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("failed to accept connection: {e}");
                continue;
            }
        };

        println!("connection from {:?}", peer_addr);

        if let Err(e) = handle_connection(&mut stream).await {
            eprintln!("connection error: {e}");
        }
    }
}

async fn handle_connection(stream: &mut VsockStream) -> Result<()> {
    loop {
        let req = match read_request(stream).await {
            Ok(Some(req)) => req,
            Ok(None) => return Ok(()), // clean EOF — normal disconnect, not an error
            Err(e) => return Err(e),
        };

        match req {
            GuestRequest::Exec { cmd } => {
                handle_exec(stream, cmd).await?;
            }
        }
    }
}

async fn read_request(stream: &mut VsockStream) -> Result<Option<GuestRequest>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(AkssoraGuestAgentError::ReadLengthPrefix(e)),
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_BODY_SIZE {
        return Err(AkssoraGuestAgentError::BodyTooLarge(len));
    }

    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .map_err(|source| AkssoraGuestAgentError::ReadBody {
            expected: len,
            source,
        })?;

    let req: GuestRequest = serde_json::from_slice(&body)?;
    Ok(Some(req))
}

async fn write_response(stream: &mut VsockStream, resp: &GuestResponse) -> Result<()> {
    let json = serde_json::to_vec(resp)?;
    let len = (json.len() as u32).to_be_bytes();
    stream
        .write_all(&len)
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
    stream
        .write_all(&json)
        .await
        .map_err(AkssoraGuestAgentError::WriteResponse)?;
    Ok(())
}

async fn handle_exec(stream: &mut VsockStream, cmd: String) -> Result<()> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AkssoraGuestAgentError::SpawnFailed {
            cmd: cmd.clone(),
            source: e,
        })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| AkssoraGuestAgentError::Process("child stdout was not piped".into()))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| AkssoraGuestAgentError::Process("child stderr was not piped".into()))?;

    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];

    loop {
        tokio::select! {
            n = stdout.read(&mut stdout_buf) => {
                let n = n?;
                if n == 0 { continue; }
                write_response(stream, &GuestResponse::Stdout(stdout_buf[..n].to_vec())).await?;
            }
            n = stderr.read(&mut stderr_buf) => {
                let n = n?;
                if n == 0 { continue; }
                write_response(stream, &GuestResponse::Stderr(stderr_buf[..n].to_vec())).await?;
            }
            status = child.wait() => {
                let status = status?;
                let code = status.code().unwrap_or(-1);
                write_response(stream, &GuestResponse::Exit(code)).await?;
                break;
            }
        }
    }

    Ok(())
}
