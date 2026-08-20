use bytes::Bytes;
use http_body_util::Full;
use std::path::PathBuf;
use tokio::{self, net::UnixStream};

use hyper::{
    Result,
    http::{Method, Request},
};
use hyper_util::rt::TokioIo;
use serde::Serialize;

type RequestBody = Full<Bytes>;

pub struct FirecrackerClient {
    socket_path: PathBuf,
}

impl FirecrackerClient {
    pub fn new(socket_path: PathBuf) -> Self {
        FirecrackerClient { socket_path }
    }

    pub async fn put(&self, path: &str, body: &impl Serialize) -> Result<()> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .expect("Failed to connect to Firecracker socket");

        let io = TokioIo::new(stream);

        let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
            .await
            .expect("Failed to perform HTTP/2 handshake");

        tokio::spawn(async move {
            if let Err(e) = conn.await {
                eprintln!("Connection error: {:?}", e);
            }
        });

        let body_json = serde_json::to_vec(body).expect("Failed to serialize body");

        let req: Request<RequestBody> = Request::builder()
            .method(Method::PUT)
            .uri(path)
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body_json)))
            .expect("Failed to build request");

        let response = sender
            .send_request(req)
            .await
            .expect("Failed to send request");

        println!("Response: {}", response.status());

        Ok(())
    }
}
