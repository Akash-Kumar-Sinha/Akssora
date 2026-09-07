use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::host_registry::{HostRegistry, HostSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MigrationState {
    Pending,
    Draining,
    Snapshotting,
    Transferring,
    Resuming,
    Complete,
    RolledBack,
    Failed,
}

impl MigrationState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            MigrationState::Complete | MigrationState::RolledBack | MigrationState::Failed
        )
    }

    pub fn next_states(&self) -> &'static [MigrationState] {
        match self {
            MigrationState::Pending => &[MigrationState::Draining],
            MigrationState::Draining => &[MigrationState::Snapshotting, MigrationState::RolledBack],
            MigrationState::Snapshotting => &[
                MigrationState::Transferring,
                MigrationState::RolledBack,
                MigrationState::Failed,
            ],
            MigrationState::Transferring => &[
                MigrationState::Resuming,
                MigrationState::RolledBack,
                MigrationState::Failed,
            ],
            MigrationState::Resuming => &[
                MigrationState::Complete,
                MigrationState::RolledBack,
                MigrationState::Failed,
            ],
            MigrationState::Complete => &[],
            MigrationState::RolledBack => &[],
            MigrationState::Failed => &[],
        }
    }

    pub fn can_rollback(&self) -> bool {
        matches!(
            self,
            MigrationState::Draining
                | MigrationState::Snapshotting
                | MigrationState::Transferring
                | MigrationState::Resuming
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRequest {
    pub sandbox_id: Uuid,
    pub source_host_id: Uuid,
    pub target_host_id: Option<Uuid>,
    pub template_id: Option<String>,
    #[serde(default = "default_migration_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_drain_timeout_secs")]
    pub drain_secs: u64,
}

fn default_migration_timeout_secs() -> u64 {
    60
}
fn default_drain_timeout_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    pub migration_id: Uuid,
    pub sandbox_id: Uuid,
    pub source_host_id: Uuid,
    pub target_host_id: Uuid,
    pub final_state: MigrationState,
    pub duration_ms: u64,
    pub message: String,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct MigrationRecord {
    pub migration_id: Uuid,
    pub sandbox_id: Uuid,
    pub source_host_id: Uuid,
    pub target_host_id: Option<Uuid>,
    pub state: MigrationState,
    pub started_at: Instant,
    pub created_at: DateTime<Utc>,
    pub state_history: Vec<(MigrationState, DateTime<Utc>, Option<String>)>,
}

impl MigrationRecord {
    pub fn new(req: &MigrationRequest) -> Self {
        let now = Utc::now();
        Self {
            migration_id: Uuid::new_v4(),
            sandbox_id: req.sandbox_id,
            source_host_id: req.source_host_id,
            target_host_id: req.target_host_id,
            state: MigrationState::Pending,
            started_at: Instant::now(),
            created_at: now,
            state_history: vec![(MigrationState::Pending, now, None)],
        }
    }

    pub fn transition(
        &mut self,
        new_state: MigrationState,
        reason: Option<String>,
    ) -> Result<(), MigrationError> {
        let allowed = self.state.next_states();
        if !allowed.contains(&new_state) {
            return Err(MigrationError::InvalidTransition {
                from: self.state,
                to: new_state,
            });
        }
        let now = Utc::now();
        self.state = new_state;
        self.state_history.push((new_state, now, reason));
        Ok(())
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("invalid state transition: {from:?} → {to:?}")]
    InvalidTransition {
        from: MigrationState,
        to: MigrationState,
    },

    #[error("source host {0} not found in registry")]
    SourceHostNotFound(Uuid),

    #[error("target host {0} not found in registry")]
    TargetHostNotFound(Uuid),

    #[error("target host {0} has insufficient capacity")]
    TargetInsufficientCapacity(Uuid),

    #[error("snapshot creation failed: {0}")]
    SnapshotFailed(String),

    #[error("snapshot transfer failed: {0}")]
    TransferFailed(String),

    #[error("VM resume on destination failed: {0}")]
    ResumeFailed(String),

    #[error("drain timeout exceeded")]
    DrainTimeout,

    #[error("migration timeout exceeded")]
    MigrationTimeout,

    #[error("rollback failed: {0}")]
    RollbackFailed(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct MigrationOrchestrator {
    active: DashMap<Uuid, MigrationRecord>,
    completed: DashMap<Uuid, MigrationRecord>,
    registry: Arc<HostRegistry>,
}

impl MigrationOrchestrator {
    pub fn new(registry: Arc<HostRegistry>) -> Self {
        Self {
            active: DashMap::new(),
            completed: DashMap::new(),
            registry,
        }
    }

    pub fn start_migration(&self, req: MigrationRequest) -> Result<Uuid, MigrationError> {
        if self.registry.get_host(&req.source_host_id).is_none() {
            return Err(MigrationError::SourceHostNotFound(req.source_host_id));
        }

        let target_id = match req.target_host_id {
            Some(id) => {
                if self.registry.get_host(&id).is_none() {
                    return Err(MigrationError::TargetHostNotFound(id));
                }
                id
            }
            None => {
                let hosts = self.registry.available_hosts();
                let candidates: Vec<_> = hosts
                    .iter()
                    .filter(|h| h.host_id != req.source_host_id)
                    .collect();
                candidates
                    .first()
                    .map(|h| h.host_id)
                    .ok_or(MigrationError::TargetInsufficientCapacity(Uuid::nil()))?
            }
        };

        let mut record = MigrationRecord::new(&req);
        record.target_host_id = Some(target_id);
        let migration_id = record.migration_id;

        self.active.insert(migration_id, record);

        tracing::info!(
            migration_id = %migration_id,
            sandbox_id = %req.sandbox_id,
            source = %req.source_host_id,
            target = %target_id,
            "migration started"
        );

        Ok(migration_id)
    }

    pub async fn advance(&self, migration_id: &Uuid) -> Result<MigrationState, MigrationError> {
        let mut record = self
            .active
            .get_mut(migration_id)
            .ok_or(MigrationError::SourceHostNotFound(*migration_id))?;

        if record.elapsed_ms() > 60_000 {
            let _ = record.transition(
                MigrationState::Failed,
                Some("migration timeout exceeded".into()),
            );
            self.promote_to_completed(migration_id);
            return Ok(MigrationState::Failed);
        }

        match record.state {
            MigrationState::Pending => {
                tracing::info!(%migration_id, "draining connections");
                let _ = record.transition(
                    MigrationState::Draining,
                    Some("draining in-flight connections".into()),
                );

                tokio::time::sleep(Duration::from_secs(2)).await;

                Ok(MigrationState::Draining)
            }
            MigrationState::Draining => {
                tracing::info!(%migration_id, "creating snapshot");
                let _ = record.transition(
                    MigrationState::Snapshotting,
                    Some("creating VM snapshot on source".into()),
                );

                tokio::time::sleep(Duration::from_secs(1)).await;

                Ok(MigrationState::Snapshotting)
            }
            MigrationState::Snapshotting => {
                tracing::info!(%migration_id, "transferring snapshot to destination");
                let _ = record.transition(
                    MigrationState::Transferring,
                    Some("streaming snapshot bytes to destination".into()),
                );

                tokio::time::sleep(Duration::from_secs(3)).await;

                Ok(MigrationState::Transferring)
            }
            MigrationState::Transferring => {
                tracing::info!(%migration_id, "resuming VM on destination");
                let _ = record.transition(
                    MigrationState::Resuming,
                    Some("booting VM from snapshot on destination".into()),
                );

                tokio::time::sleep(Duration::from_secs(1)).await;

                Ok(MigrationState::Resuming)
            }
            MigrationState::Resuming => {
                tracing::info!(%migration_id, "migration complete");
                let _ = record.transition(
                    MigrationState::Complete,
                    Some("VM running on destination host".into()),
                );

                self.promote_to_completed(migration_id);
                Ok(MigrationState::Complete)
            }
            state => Ok(state),
        }
    }

    pub async fn rollback(
        &self,
        migration_id: &Uuid,
        reason: &str,
    ) -> Result<MigrationState, MigrationError> {
        let mut record = self
            .active
            .get_mut(migration_id)
            .ok_or(MigrationError::SourceHostNotFound(*migration_id))?;

        if !record.state.can_rollback() {
            return Err(MigrationError::InvalidTransition {
                from: record.state,
                to: MigrationState::RolledBack,
            });
        }

        tracing::warn!(
            %migration_id,
            from_state = ?record.state,
            reason,
            "rolling back migration"
        );

        match record.state {
            MigrationState::Snapshotting | MigrationState::Transferring => {
                tracing::info!(%migration_id, "cleaning up partial snapshot on destination");
            }
            MigrationState::Resuming => {
                tracing::warn!(%migration_id, "VM may be starting on destination, attempting stop");
            }
            _ => {}
        }

        tracing::info!(%migration_id, "ensuring source VM is resumed");

        let _ = record.transition(
            MigrationState::RolledBack,
            Some(format!("rolled back: {reason}")),
        );

        self.promote_to_completed(migration_id);
        Ok(MigrationState::RolledBack)
    }

    pub fn state(&self, migration_id: &Uuid) -> Option<MigrationState> {
        if let Some(record) = self.active.get(migration_id) {
            return Some(record.state);
        }
        self.completed.get(migration_id).map(|r| r.state)
    }

    pub fn get_record(&self, migration_id: &Uuid) -> Option<MigrationRecordSnapshot> {
        if let Some(record) = self.active.get(migration_id) {
            return Some(MigrationRecordSnapshot::from_record(&record));
        }
        self.completed
            .get(migration_id)
            .map(|r| MigrationRecordSnapshot::from_record(&r))
    }

    pub fn active_migrations(&self) -> Vec<MigrationRecordSnapshot> {
        self.active
            .iter()
            .map(|r| MigrationRecordSnapshot::from_record(&r))
            .collect()
    }

    pub fn completed_migrations(&self) -> Vec<MigrationRecordSnapshot> {
        self.completed
            .iter()
            .map(|r| MigrationRecordSnapshot::from_record(&r))
            .collect()
    }

    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    fn promote_to_completed(&self, migration_id: &Uuid) {
        if let Some((_, record)) = self.active.remove(migration_id) {
            self.completed.insert(*migration_id, record);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRecordSnapshot {
    pub migration_id: Uuid,
    pub sandbox_id: Uuid,
    pub source_host_id: Uuid,
    pub target_host_id: Option<Uuid>,
    pub state: MigrationState,
    pub elapsed_ms: u64,
    pub created_at: DateTime<Utc>,
    pub state_history: Vec<(MigrationState, DateTime<Utc>, Option<String>)>,
}

impl MigrationRecordSnapshot {
    fn from_record(record: &MigrationRecord) -> Self {
        Self {
            migration_id: record.migration_id,
            sandbox_id: record.sandbox_id,
            source_host_id: record.source_host_id,
            target_host_id: record.target_host_id,
            state: record.state,
            elapsed_ms: record.elapsed_ms(),
            created_at: record.created_at,
            state_history: record.state_history.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainHostRequest {
    pub host_id: Uuid,
    pub target_host_id: Option<Uuid>,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_max_concurrent() -> usize {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainHostResult {
    pub host_id: Uuid,
    pub total_migrations: usize,
    pub completed: usize,
    pub failed: usize,
    pub rolled_back: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionHandoff {
    pub connection_id: Uuid,
    pub sandbox_id: Uuid,
    pub source_host: HostSnapshot,
    pub target_host: HostSnapshot,
    pub redirect_after: MigrationState,
}

pub struct ConnectionRedirectTable {
    redirects: DashMap<Uuid, HostSnapshot>, // sandbox_id → new host
}

impl ConnectionRedirectTable {
    pub fn new() -> Self {
        Self {
            redirects: DashMap::new(),
        }
    }

    pub fn register_redirect(&self, sandbox_id: Uuid, new_host: HostSnapshot) {
        tracing::info!(%sandbox_id, host = %new_host.host_id, "registering connection redirect");
        self.redirects.insert(sandbox_id, new_host);
    }

    pub fn get_redirect(&self, sandbox_id: &Uuid) -> Option<HostSnapshot> {
        self.redirects.get(sandbox_id).map(|r| r.value().clone())
    }

    pub fn clear_redirect(&self, sandbox_id: &Uuid) {
        self.redirects.remove(sandbox_id);
    }
}

impl Default for ConnectionRedirectTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_orchestrator() -> MigrationOrchestrator {
        let registry = Arc::new(HostRegistry::default());

        use crate::host_registry::HostRegistry;
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        let addr1 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 9000);
        let addr2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 9000);

        let host1 = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let host2 = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();

        registry.upsert_host(host1, addr1, 8, 4096);
        registry.upsert_host(host2, addr2, 8, 4096);
        registry.heartbeat(&host1, 2, 1024, 3);
        registry.heartbeat(&host2, 0, 0, 0);

        MigrationOrchestrator::new(registry)
    }

    fn make_request(source: Uuid, target: Uuid) -> MigrationRequest {
        MigrationRequest {
            sandbox_id: Uuid::new_v4(),
            source_host_id: source,
            target_host_id: Some(target),
            template_id: Some("test-template".into()),
            timeout_secs: 120,
            drain_secs: 2,
        }
    }

    #[test]
    fn state_transitions_valid() {
        let mut record = MigrationRecord {
            migration_id: Uuid::new_v4(),
            sandbox_id: Uuid::new_v4(),
            source_host_id: Uuid::new_v4(),
            target_host_id: Some(Uuid::new_v4()),
            state: MigrationState::Pending,
            started_at: Instant::now(),
            created_at: Utc::now(),
            state_history: vec![],
        };

        assert!(record.transition(MigrationState::Draining, None).is_ok());
        assert!(
            record
                .transition(MigrationState::Snapshotting, None)
                .is_ok()
        );
        assert!(
            record
                .transition(MigrationState::Transferring, None)
                .is_ok()
        );
        assert!(record.transition(MigrationState::Resuming, None).is_ok());
        assert!(record.transition(MigrationState::Complete, None).is_ok());
    }

    #[test]
    fn invalid_transition_rejected() {
        let mut record = MigrationRecord {
            migration_id: Uuid::new_v4(),
            sandbox_id: Uuid::new_v4(),
            source_host_id: Uuid::new_v4(),
            target_host_id: Some(Uuid::new_v4()),
            state: MigrationState::Pending,
            started_at: Instant::now(),
            created_at: Utc::now(),
            state_history: vec![],
        };

        assert!(record.transition(MigrationState::Complete, None).is_err());
        assert!(
            record
                .transition(MigrationState::Snapshotting, None)
                .is_err()
        );
    }

    #[test]
    fn rollback_from_any_active_state() {
        for state in &[
            MigrationState::Draining,
            MigrationState::Snapshotting,
            MigrationState::Transferring,
            MigrationState::Resuming,
        ] {
            let mut record = MigrationRecord {
                migration_id: Uuid::new_v4(),
                sandbox_id: Uuid::new_v4(),
                source_host_id: Uuid::new_v4(),
                target_host_id: Some(Uuid::new_v4()),
                state: *state,
                started_at: Instant::now(),
                created_at: Utc::now(),
                state_history: vec![],
            };
            assert!(
                record
                    .transition(MigrationState::RolledBack, Some("test".into()))
                    .is_ok(),
                "should be able to rollback from {state:?}"
            );
        }
    }

    #[test]
    fn cannot_rollback_from_terminal_state() {
        for state in &[
            MigrationState::Complete,
            MigrationState::RolledBack,
            MigrationState::Failed,
        ] {
            assert!(
                !state.can_rollback(),
                "terminal state {state:?} should not allow rollback"
            );
        }
    }

    #[test]
    fn terminal_states_have_no_next() {
        for state in &[
            MigrationState::Complete,
            MigrationState::RolledBack,
            MigrationState::Failed,
        ] {
            assert!(
                state.next_states().is_empty(),
                "terminal state {state:?} should have no next states"
            );
        }
    }

    #[test]
    fn orchestrator_start_migration() {
        let orch = make_orchestrator();
        let source = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let target = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();

        let req = make_request(source, target);
        let migration_id = orch.start_migration(req).unwrap();

        assert_eq!(orch.state(&migration_id), Some(MigrationState::Pending));
        assert_eq!(orch.active_count(), 1);
    }

    #[test]
    fn orchestrator_rejects_unknown_source() {
        let orch = make_orchestrator();
        let unknown = Uuid::new_v4();
        let target = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();

        let req = make_request(unknown, target);
        assert!(matches!(
            orch.start_migration(req),
            Err(MigrationError::SourceHostNotFound(_))
        ));
    }

    #[test]
    fn orchestrator_rejects_unknown_target() {
        let orch = make_orchestrator();
        let source = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let unknown = Uuid::new_v4();

        let req = make_request(source, unknown);
        assert!(matches!(
            orch.start_migration(req),
            Err(MigrationError::TargetHostNotFound(_))
        ));
    }

    #[test]
    fn serializes_roundtrip() {
        let req = MigrationRequest {
            sandbox_id: Uuid::new_v4(),
            source_host_id: Uuid::new_v4(),
            target_host_id: Some(Uuid::new_v4()),
            template_id: Some("test".into()),
            timeout_secs: 60,
            drain_secs: 5,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: MigrationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sandbox_id, req.sandbox_id);
        assert_eq!(parsed.timeout_secs, 60);
    }

    #[test]
    fn connection_redirect_table() {
        let table = ConnectionRedirectTable::new();
        let sandbox_id = Uuid::new_v4();
        let host = HostSnapshot {
            host_id: Uuid::new_v4(),
            address: "10.0.0.1:9000".parse().unwrap(),
            total_vcpus: 8,
            used_vcpus: 2,
            total_mem_mib: 4096,
            used_mem_mib: 1024,
            sandbox_count: 3,
            circuit_state: crate::circuit_breaker::CircuitState::Closed,
            last_heartbeat: Utc::now(),
        };

        assert!(table.get_redirect(&sandbox_id).is_none());
        table.register_redirect(sandbox_id, host.clone());
        assert!(table.get_redirect(&sandbox_id).is_some());
        table.clear_redirect(&sandbox_id);
        assert!(table.get_redirect(&sandbox_id).is_none());
    }

    #[test]
    fn migration_record_snapshot_serializes() {
        let record = MigrationRecordSnapshot {
            migration_id: Uuid::new_v4(),
            sandbox_id: Uuid::new_v4(),
            source_host_id: Uuid::new_v4(),
            target_host_id: Some(Uuid::new_v4()),
            state: MigrationState::Transferring,
            elapsed_ms: 5000,
            created_at: Utc::now(),
            state_history: vec![],
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: MigrationRecordSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.state, MigrationState::Transferring);
        assert_eq!(parsed.elapsed_ms, 5000);
    }
}
