mod env;
mod error;
mod files;
mod port_forward;
mod pty_session;
mod seccomp;

use std::process::Stdio;

use akssora_core::protocol::{GuestRequest, GuestResponse, read_request, write_response};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener, VsockStream};

use crate::env::apply_to_tokio_command;
use crate::error::{AkssoraGuestAgentError, Result};
use crate::pty_session::PtySession;

fn mount_essential_filesystems() {
    let _ = std::fs::create_dir_all("/proc");
    let _ = std::fs::create_dir_all("/sys");
    let _ = std::fs::create_dir_all("/dev");
    let _ = std::fs::create_dir_all("/dev/pts");
    let _ = std::fs::create_dir_all("/dev/shm");
    let _ = std::fs::create_dir_all("/tmp");
    let _ = std::fs::create_dir_all("/root");
    let _ = std::fs::create_dir_all("/home");

    // INFO: pre-create shell and python history files in home directory
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/root/.python_history");
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/root/.bash_history");

    // CRITICAL: mount essential filesystems if running as init (PID 1)
    unsafe {
        nix::libc::mount(
            c"proc".as_ptr(),
            c"/proc".as_ptr(),
            c"proc".as_ptr(),
            0,
            std::ptr::null(),
        );
        nix::libc::mount(
            c"sysfs".as_ptr(),
            c"/sys".as_ptr(),
            c"sysfs".as_ptr(),
            0,
            std::ptr::null(),
        );
        nix::libc::mount(
            c"devtmpfs".as_ptr(),
            c"/dev".as_ptr(),
            c"devtmpfs".as_ptr(),
            0,
            std::ptr::null(),
        );
        let _ = std::fs::create_dir_all("/dev/pts");
        nix::libc::mount(
            c"devpts".as_ptr(),
            c"/dev/pts".as_ptr(),
            c"devpts".as_ptr(),
            0,
            std::ptr::null(),
        );
    }

    // INFO: set guest hostname so prompt displays akssora instead of (none)
    unsafe {
        let name = c"akssora";
        nix::libc::sethostname(name.as_ptr(), 7);
    }
    let _ = std::fs::write("/etc/hostname", "akssora\n");
}

#[tokio::main]
async fn main() -> Result<()> {
    mount_essential_filesystems();

    if let Err(e) = seccomp::apply_seccomp_filter() {
        tracing::warn!(error = %e, "seccomp filter could not be applied (non-fatal)");
    }

    let addr = VsockAddr::new(VMADDR_CID_ANY, 1024);
    let listener = VsockListener::bind(addr)
        .map_err(|error| AkssoraGuestAgentError::VsockBind(error.to_string()))?;

    loop {
        let (mut stream, _peer_addr) = match listener.accept().await {
            Ok(connection) => connection,
            Err(_) => {
                continue;
            }
        };

        let _ = handle_connection(&mut stream).await;
    }
}

async fn handle_connection(stream: &mut VsockStream) -> Result<()> {
    loop {
        let request = match read_request(stream).await {
            Ok(Some(request)) => request,
            Ok(None) => return Ok(()),
            Err(error) => return Err(AkssoraGuestAgentError::Protocol(error)),
        };

        match request {
            GuestRequest::Exec { cmd } => {
                handle_exec(stream, cmd).await?;
            }
            GuestRequest::OpenShell => {
                PtySession::run(stream).await?;
            }
            GuestRequest::WriteFileChunk {
                path,
                offset,
                data,
                is_last,
            } => {
                files::handle_write_file_chunk(stream, &path, offset, &data, is_last).await?;
            }
            GuestRequest::ReadFile { path, chunk_size } => {
                files::handle_read_file(stream, &path, chunk_size).await?;
            }
            GuestRequest::SetEnv { vars } => {
                let count = env::set_env_vars(&vars);
                write_response(stream, &GuestResponse::EnvSet { count })
                    .await
                    .map_err(AkssoraGuestAgentError::WriteResponse)?;
            }
            GuestRequest::ExposePort { guest_port } => {
                handle_expose_port(stream, guest_port).await?;
            }
            GuestRequest::Input(_) | GuestRequest::CloseShell => {}
        }
    }
}

async fn handle_exec(stream: &mut VsockStream, cmd: String) -> Result<()> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    apply_to_tokio_command(&mut command);

    let mut child = command
        .spawn()
        .map_err(|source| AkssoraGuestAgentError::SpawnFailed {
            cmd: cmd.clone(),
            source,
        })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or(AkssoraGuestAgentError::StdoutNotPiped)?;

    let mut stderr = child
        .stderr
        .take()
        .ok_or(AkssoraGuestAgentError::StderrNotPiped)?;

    let mut stdout_buf = [0u8; 4096];
    let mut stderr_buf = [0u8; 4096];

    let mut stdout_open = true;
    let mut stderr_open = true;

    loop {
        tokio::select! {
            result = stdout.read(&mut stdout_buf), if stdout_open => {
                let n = result.map_err(AkssoraGuestAgentError::ChildIo)?;
                if n == 0 {
                    stdout_open = false;
                    continue;
                }
                write_response(
                    stream,
                    &GuestResponse::Stdout(stdout_buf[..n].to_vec()),
                )
                .await
                .map_err(AkssoraGuestAgentError::WriteResponse)?;
            }

            result = stderr.read(&mut stderr_buf), if stderr_open => {
                let n = result.map_err(AkssoraGuestAgentError::ChildIo)?;
                if n == 0 {
                    stderr_open = false;
                    continue;
                }
                write_response(
                    stream,
                    &GuestResponse::Stderr(stderr_buf[..n].to_vec()),
                )
                .await
                .map_err(AkssoraGuestAgentError::WriteResponse)?;
            }

            status = child.wait() => {
                let status = status.map_err(AkssoraGuestAgentError::ChildIo)?;
                let code = status.code().unwrap_or(-1);
                write_response(stream, &GuestResponse::Exit(code))
                    .await
                    .map_err(AkssoraGuestAgentError::WriteResponse)?;
                break;
            }
        }

        if !stdout_open && !stderr_open {
            let status = child
                .wait()
                .await
                .map_err(AkssoraGuestAgentError::ChildIo)?;
            let code = status.code().unwrap_or(-1);
            write_response(stream, &GuestResponse::Exit(code))
                .await
                .map_err(AkssoraGuestAgentError::WriteResponse)?;
            break;
        }
    }

    Ok(())
}

async fn handle_expose_port(stream: &mut VsockStream, guest_port: u16) -> Result<()> {
    let addr: std::net::SocketAddr =
        format!("0.0.0.0:{}", guest_port)
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                AkssoraGuestAgentError::Protocol(akssora_core::AkssoraCoreError::Io(
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()),
                ))
            })?;

    let _listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(AkssoraGuestAgentError::ChildIo)?;

    let actual_port = _listener
        .local_addr()
        .map_err(AkssoraGuestAgentError::ChildIo)?
        .port();

    tracing::info!(guest_port = actual_port, "port exposed in guest");

    write_response(
        stream,
        &GuestResponse::PortExposed {
            guest_port,
            relay_port: actual_port,
        },
    )
    .await
    .map_err(AkssoraGuestAgentError::WriteResponse)?;

    Ok(())
}
