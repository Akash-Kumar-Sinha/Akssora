use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AkssoraCoreError, Result};
use crate::session::{Session, SessionConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkResult {
    pub new_id: Uuid,
    pub source_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("source sandbox not found: {0}")]
    SourceNotFound(Uuid),

    #[error("cannot fork a stopped sandbox")]
    SourceStopped,

    #[error("snapshot creation failed: {0}")]
    SnapshotFailed(String),

    #[error("boot from snapshot failed: {0}")]
    BootFailed(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<ForkError> for AkssoraCoreError {
    fn from(e: ForkError) -> Self {
        AkssoraCoreError::Pool(e.to_string())
    }
}

pub async fn fork_session(
    source: &mut Session,
    source_id: Uuid,
) -> Result<ForkResult> {
    let new_id = Uuid::new_v4();
    let created_at = Utc::now();

    let new_base = std::env::temp_dir().join(format!("fork-{}", new_id));
    std::fs::create_dir_all(&new_base)?;

    let snapshot_path = new_base.join("snapshot.json");
    let mem_file_path = new_base.join("vm.mem");

    source
        .pause()
        .await
        .map_err(|e| ForkError::SnapshotFailed(format!("pause failed: {e}")))?;

    source
        .create_snapshot(snapshot_path.clone(), mem_file_path.clone())
        .await
        .map_err(|e| ForkError::SnapshotFailed(e.to_string()))?;

    source
        .resume()
        .await
        .map_err(|e| ForkError::SnapshotFailed(format!("resume failed: {e}")))?;

    let new_config = SessionConfig::new(new_id)
        .map_err(|e| ForkError::BootFailed(e.to_string()))?;

    let new_vsock_path = new_config.vsock_uds_path.clone();

    let new_session =
        Session::from_snapshot(new_id, snapshot_path, mem_file_path, new_vsock_path)
            .await
            .map_err(|e| ForkError::BootFailed(e.to_string()))?;

    FORKED_SESSION.with(|cell| {
        *cell.borrow_mut() = Some(new_session);
    });

    Ok(ForkResult {
        new_id,
        source_id,
        created_at,
    })
}

thread_local! {
    static FORKED_SESSION: std::cell::RefCell<Option<Session>> = const { std::cell::RefCell::new(None) };
}

pub fn take_forked_session() -> Option<Session> {
    FORKED_SESSION.with(|cell| cell.borrow_mut().take())
}

pub async fn fork_sandbox(
    source: &mut Session,
    source_id: Uuid,
) -> Result<(ForkResult, Session)> {
    let result = fork_session(source, source_id).await?;
    let session = take_forked_session()
        .ok_or_else(|| AkssoraCoreError::Pool("forked session not found".into()))?;
    Ok((result, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_result_serializes() {
        let result = ForkResult {
            new_id: Uuid::new_v4(),
            source_id: Uuid::new_v4(),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: ForkResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.new_id, result.new_id);
        assert_eq!(parsed.source_id, result.source_id);
    }

    #[test]
    fn fork_error_display() {
        let err = ForkError::SourceNotFound(Uuid::nil());
        assert!(err.to_string().contains("not found"));
    }
}
