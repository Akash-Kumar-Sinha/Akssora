use std::net::SocketAddr;

use tokio::io;
use tokio::net::TcpListener;
use tokio_vsock::VsockStream;

use crate::error::{AkssoraGuestAgentError, Result};

#[allow(dead_code)]
pub async fn start_guest_port_proxy(guest_port: u16, host_stream: VsockStream) -> Result<u16> {
    let addr: SocketAddr =
        format!("0.0.0.0:{}", guest_port)
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                AkssoraGuestAgentError::Protocol(akssora_core::AkssoraCoreError::Io(
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()),
                ))
            })?;

    let listener = TcpListener::bind(addr)
        .await
        .map_err(AkssoraGuestAgentError::ChildIo)?;

    let actual_port = listener
        .local_addr()
        .map_err(AkssoraGuestAgentError::ChildIo)?
        .port();

    tracing::info!(guest_port = actual_port, "guest port proxy listening");

    let (tcp_stream, _peer) = listener
        .accept()
        .await
        .map_err(AkssoraGuestAgentError::ChildIo)?;

    tracing::debug!(guest_port = actual_port, "accepted connection for relay");

    let (mut tcp_read, mut tcp_write) = tcp_stream.into_split();
    let (mut vsock_read, mut vsock_write) = host_stream.into_split();

    let tcp_to_vsock = io::copy(&mut tcp_read, &mut vsock_write);
    let vsock_to_tcp = io::copy(&mut vsock_read, &mut tcp_write);

    tokio::select! {
        result = tcp_to_vsock => {
            if let Err(e) = result {
                tracing::trace!(error = %e, "guest: tcp→vsock ended");
            }
        }
        result = vsock_to_tcp => {
            if let Err(e) = result {
                tracing::trace!(error = %e, "guest: vsock→tcp ended");
            }
        }
    }

    Ok(actual_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_proxy_compiles() {
        assert!(
            std::mem::size_of::<
                fn(
                    u16,
                    VsockStream,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u16>> + Send>>,
            >() > 0
        );
    }
}
