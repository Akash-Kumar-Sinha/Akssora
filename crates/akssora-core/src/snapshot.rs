use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AkssoraCoreError, Result};
use crate::firecracker::FirecrackerClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SnapshotType {
    #[default]
    #[serde(rename = "Full")]
    Full,
    #[serde(rename = "Diff")]
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VmStateValue {
    #[serde(rename = "Paused")]
    Paused,
    #[serde(rename = "Resumed")]
    Resumed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmState {
    pub state: VmStateValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSnapshotParams {
    pub snapshot_type: SnapshotType,
    pub snapshot_path: PathBuf,
    pub mem_file_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSnapshotParams {
    pub snapshot_path: PathBuf,
    pub mem_file_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_diff_snapshots: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_vm: Option<bool>,
}

pub async fn pause_vm(client: &FirecrackerClient) -> Result<()> {
    client
        .patch(
            "/vm",
            &VmState {
                state: VmStateValue::Paused,
            },
        )
        .await
        .map_err(|e| AkssoraCoreError::PauseMicroVM(e.to_string()))
}

pub async fn resume_vm(client: &FirecrackerClient) -> Result<()> {
    client
        .patch(
            "/vm",
            &VmState {
                state: VmStateValue::Resumed,
            },
        )
        .await
        .map_err(|e| AkssoraCoreError::ResumeMicroVM(e.to_string()))
}

pub async fn create_snapshot(
    client: &FirecrackerClient,
    params: &CreateSnapshotParams,
) -> Result<()> {
    client
        .put("/snapshot/create", params)
        .await
        .map_err(|e| AkssoraCoreError::SnapshotCreate(e.to_string()))
}

pub async fn load_snapshot(client: &FirecrackerClient, params: &LoadSnapshotParams) -> Result<()> {
    client
        .put("/snapshot/load", params)
        .await
        .map_err(|e| AkssoraCoreError::SnapshotLoad(e.to_string()))
}
