use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum GuestRequest {
    Exec { cmd: String },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum GuestResponse {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
}
