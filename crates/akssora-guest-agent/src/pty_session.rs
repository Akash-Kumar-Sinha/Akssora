use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::process::Stdio;

use akssora_core::protocol::{GuestRequest, GuestResponse, read_request, write_response};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::openpty;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::error::{AkssoraGuestAgentError, Result};

pub struct PtySession;

impl PtySession {
    pub async fn run<S: AsyncReadExt + AsyncWriteExt + Unpin>(stream: &mut S) -> Result<()> {
        let pty = openpty(None, None).map_err(AkssoraGuestAgentError::PtyOpen)?;

        let master = pty.master;

        let slave_file: std::fs::File = pty.slave.into();
        let stdin = slave_file
            .try_clone()
            .map_err(AkssoraGuestAgentError::PtyClone)?;
        let stdout = slave_file
            .try_clone()
            .map_err(AkssoraGuestAgentError::PtyClone)?;
        let stderr = slave_file;
        let slave_raw_fd = stdin.as_raw_fd();

        let shell = if Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "/bin/sh"
        };

        // INFO: ensure /root, /etc/profile.d, and history files exist
        let _ = std::fs::create_dir_all("/root");
        let _ = std::fs::create_dir_all("/etc/profile.d");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/root/.python_history");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/root/.bash_history");

        // INFO: persist prompt in rc files so login shells also pick it up immediately
        let prompt_rc = "export PS1='\\[\\033[1;36m\\]akssora>\\[\\033[0m\\] '\nexport PROMPT_COMMAND=''\n";
        let _ = std::fs::write("/root/.bashrc", prompt_rc);
        let _ = std::fs::write("/root/.profile", prompt_rc);
        let _ = std::fs::write("/etc/profile.d/akssora.sh", prompt_rc);
        let _ = std::fs::write("/etc/hostname", "akssora\n");

        unsafe {
            let name = c"akssora";
            nix::libc::sethostname(name.as_ptr(), 7);
        }

        let mut cmd = Command::new(shell);
        cmd.arg("-l")
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .env("TERM", "xterm-256color")
            .env("HOME", "/root")
            .env("USER", "root")
            .env("PS1", "\x1b[1;36makssora>\x1b[0m ")
            .env("PROMPT_COMMAND", "")
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            );

        // CRITICAL: configure controlling terminal and session leadership for the shell child process
        unsafe {
            cmd.pre_exec(move || {
                if nix::libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }

                if nix::libc::ioctl(slave_raw_fd, nix::libc::TIOCSCTTY, 0) < 0 {
                    let _ = nix::libc::ioctl(0, nix::libc::TIOCSCTTY, 0);
                }

                let pid = nix::libc::getpid();
                let _ = nix::libc::setpgid(pid, pid);
                let _ = nix::libc::tcsetpgrp(0, pid);

                Ok(())
            });
        }

        let mut child = cmd.spawn().map_err(AkssoraGuestAgentError::PtySpawn)?;

        let flags = fcntl(master.as_raw_fd(), FcntlArg::F_GETFL)
            .map_err(AkssoraGuestAgentError::PtyFcntl)?;
        let mut flags = OFlag::from_bits_truncate(flags);
        flags.insert(OFlag::O_NONBLOCK);
        fcntl(master.as_raw_fd(), FcntlArg::F_SETFL(flags))
            .map_err(AkssoraGuestAgentError::PtyFcntl)?;

        let async_master = AsyncFd::new(master).map_err(AkssoraGuestAgentError::AsyncFd)?;

        loop {
            tokio::select! {
                readable_guard = async_master.readable() => {
                    let mut guard = readable_guard.map_err(AkssoraGuestAgentError::AsyncFd)?;

                    match guard.try_io(|inner| {
                        let mut buf = [0u8; 4096];
                        let n = unsafe {
                            nix::libc::read(
                                inner.get_ref().as_raw_fd(),
                                buf.as_mut_ptr() as *mut _,
                                buf.len(),
                            )
                        };

                        if n < 0 {
                            let err = std::io::Error::last_os_error();
                            // CRITICAL: EIO on Linux master PTY indicates slave closed (EOF)
                            if err.raw_os_error() == Some(nix::libc::EIO) {
                                return Ok((buf, 0));
                            }
                            return Err(err);
                        }

                        Ok((buf, n as usize))
                    }) {
                        Ok(Ok((_buf, 0))) => {
                            // INFO: PTY slave was closed by child process
                            break;
                        }
                        Ok(Ok((buf, n))) => {
                            write_response(
                                stream,
                                &GuestResponse::Output(buf[..n].to_vec()),
                            )
                            .await
                            .map_err(AkssoraGuestAgentError::WriteResponse)?;
                        }
                        Ok(Err(error)) => {
                            return Err(AkssoraGuestAgentError::PtyRead(error));
                        }
                        Err(_would_block) => continue,
                    }
                }

                request = read_request(stream) => {
                    match request.map_err(AkssoraGuestAgentError::Protocol)? {
                        Some(GuestRequest::Input(bytes)) => {
                            Self::write_to_master(&async_master, &bytes).await?;
                        }
                        Some(GuestRequest::CloseShell) | None => {
                            break;
                        }
                        Some(GuestRequest::OpenShell) => {
                            // INFO: shell is already active on this connection
                        }
                        Some(GuestRequest::Exec { .. }) => {
                            // INFO: nested exec inside active shell is ignored
                        }
                        Some(GuestRequest::WriteFileChunk { .. })
                        | Some(GuestRequest::ReadFile { .. })
                        | Some(GuestRequest::SetEnv { .. })
                        | Some(GuestRequest::ExposePort { .. }) => {}
                    }
                }

                status = child.wait() => {
                    let _ = status.map_err(AkssoraGuestAgentError::PtyWait)?;
                    break;
                }
            }
        }

        // CRITICAL: ensure child process and shell are cleaned up on teardown
        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = write_response(stream, &GuestResponse::ShellClosed).await;

        Ok(())
    }

    async fn write_to_master(async_master: &AsyncFd<OwnedFd>, data: &[u8]) -> Result<()> {
        let mut written = 0;

        while written < data.len() {
            let mut guard = async_master
                .writable()
                .await
                .map_err(AkssoraGuestAgentError::AsyncFd)?;

            match guard.try_io(|inner| {
                let n = unsafe {
                    nix::libc::write(
                        inner.get_ref().as_raw_fd(),
                        data[written..].as_ptr() as *const _,
                        data.len() - written,
                    )
                };

                if n < 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(err);
                }

                Ok(n as usize)
            }) {
                Ok(Ok(n)) => {
                    written += n;
                }
                Ok(Err(error)) => return Err(AkssoraGuestAgentError::PtyWrite(error)),
                Err(_would_block) => continue,
            }
        }

        Ok(())
    }
}
