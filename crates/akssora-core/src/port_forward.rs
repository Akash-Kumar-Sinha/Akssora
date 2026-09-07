use std::net::SocketAddr;

use tokio::io;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

use crate::error::{AkssoraCoreError, Result};

pub struct PortForwardHandle {
    pub sandbox_id: Uuid,
    pub guest_port: u16,
    pub host_port: u16,
    pub listen_addr: SocketAddr,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl PortForwardHandle {
    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.host_port)
    }

    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

#[tracing::instrument(skip(vsock_uds_path), fields(sandbox_id = %sandbox_id, guest_port))]
pub async fn start_port_forward(
    sandbox_id: Uuid,
    guest_port: u16,
    host_port: u16,
    vsock_uds_path: std::path::PathBuf,
) -> Result<PortForwardHandle> {
    let listener = TcpListener::bind(("0.0.0.0", host_port))
        .await
        .map_err(AkssoraCoreError::Io)?;

    let listen_addr = listener.local_addr().map_err(AkssoraCoreError::Io)?;
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let sid = sandbox_id;
    tokio::spawn(async move {
        tracing::info!(guest_port, host_port, "port forward relay started");

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((tcp_stream, peer_addr)) => {
                            tracing::debug!(peer = %peer_addr, "accepted connection for relay");
                            let vsock_path = vsock_uds_path.clone();
                            tokio::spawn(async move {
                                if let Err(e) = relay_connection(tcp_stream, guest_port, vsock_path).await {
                                    tracing::warn!(error = %e, "relay connection failed");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "failed to accept connection");
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    tracing::info!("port forward relay shutting down");
                    break;
                }
            }
        }
    });

    Ok(PortForwardHandle {
        sandbox_id: sid,
        guest_port,
        host_port,
        listen_addr,
        shutdown_tx,
    })
}

async fn relay_connection(
    tcp_stream: TcpStream,
    guest_port: u16,
    vsock_uds_path: std::path::PathBuf,
) -> Result<()> {
    let vsock_stream = connect_to_guest_vsock(&vsock_uds_path, guest_port).await?;

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();
    let (mut vsock_read, mut vsock_write) = tokio::io::split(vsock_stream);

    let tcp_to_vsock = io::copy(&mut tcp_read, &mut vsock_write);
    let vsock_to_tcp = io::copy(&mut vsock_read, &mut tcp_write);

    tokio::select! {
        result = tcp_to_vsock => {
            if let Err(e) = result {
                tracing::trace!(error = %e, "tcp→vsock copy ended");
            }
        }
        result = vsock_to_tcp => {
            if let Err(e) = result {
                tracing::trace!(error = %e, "vsock→tcp copy ended");
            }
        }
    }

    Ok(())
}

async fn connect_to_guest_vsock(
    vsock_uds_path: &std::path::Path,
    port: u16,
) -> Result<VsockStreamWrapper> {
    use tokio::io::AsyncWriteExt;

    let mut stream = tokio::net::UnixStream::connect(vsock_uds_path)
        .await
        .map_err(AkssoraCoreError::VsockConnect)?;

    let connect_msg = format!("CONNECT {}\n", port);
    stream
        .write_all(connect_msg.as_bytes())
        .await
        .map_err(AkssoraCoreError::WriteRequest)?;

    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream
            .read_exact(&mut byte)
            .await
            .map_err(AkssoraCoreError::ReadLengthPrefix)?;
        response.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }

    if !response.starts_with(b"OK") {
        return Err(AkssoraCoreError::VsockConnect(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("vsock CONNECT rejected: {:?}", String::from_utf8_lossy(&response)),
        )));
    }

    Ok(VsockStreamWrapper(stream))
}

mod vsock_wrapper {
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::UnixStream;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub struct VsockStream(pub UnixStream);

    impl AsyncRead for VsockStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for VsockStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }
}

use vsock_wrapper::VsockStream as VsockStreamWrapper;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_local_url_format() {
        let (tx, _rx) = tokio::sync::watch::channel(false);
        let handle = PortForwardHandle {
            sandbox_id: Uuid::new_v4(),
            guest_port: 8080,
            host_port: 9090,
            listen_addr: "127.0.0.1:9090".parse().unwrap(),
            shutdown_tx: tx,
        };
        assert_eq!(handle.local_url(), "http://127.0.0.1:9090");
    }
}
