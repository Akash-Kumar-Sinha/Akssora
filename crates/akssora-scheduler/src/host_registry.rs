use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::circuit_breaker::{CircuitBreaker, CircuitState};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceRequest {
    pub vcpus: u32,
    pub mem_mib: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub host_id: Uuid,
    pub address: SocketAddr,
    pub total_vcpus: u32,
    pub used_vcpus: u32,
    pub total_mem_mib: u32,
    pub used_mem_mib: u32,
    pub sandbox_count: u32,
    pub circuit_state: CircuitState,
    pub last_heartbeat: DateTime<Utc>,
}

impl HostSnapshot {
    pub fn free_vcpus(&self) -> u32 {
        self.total_vcpus.saturating_sub(self.used_vcpus)
    }

    pub fn free_mem_mib(&self) -> u32 {
        self.total_mem_mib.saturating_sub(self.used_mem_mib)
    }

    pub fn can_schedule(&self, req: &ResourceRequest) -> bool {
        self.circuit_state != CircuitState::Open
            && self.free_vcpus() >= req.vcpus
            && self.free_mem_mib() >= req.mem_mib
    }
}

pub struct HostRegistry {
    hosts: Arc<DashMap<Uuid, HostEntry>>,
    heartbeat_timeout: Duration,
}

#[derive(Debug)]
struct HostEntry {
    address: SocketAddr,
    total_vcpus: u32,
    used_vcpus: u32,
    total_mem_mib: u32,
    used_mem_mib: u32,
    sandbox_count: u32,
    last_heartbeat: DateTime<Utc>,
    breaker: CircuitBreaker,
}

impl HostRegistry {
    pub fn new(heartbeat_timeout: Duration) -> Self {
        Self {
            hosts: Arc::new(DashMap::new()),
            heartbeat_timeout,
        }
    }

    #[tracing::instrument(skip(self), fields(host_id = %host_id))]
    pub fn upsert_host(
        &self,
        host_id: Uuid,
        address: SocketAddr,
        total_vcpus: u32,
        total_mem_mib: u32,
    ) {
        self.hosts
            .entry(host_id)
            .and_modify(|entry| {
                entry.address = address;
                entry.total_vcpus = total_vcpus;
                entry.total_mem_mib = total_mem_mib;
                entry.last_heartbeat = Utc::now();
            })
            .or_insert_with(|| HostEntry {
                address,
                total_vcpus,
                used_vcpus: 0,
                total_mem_mib,
                used_mem_mib: 0,
                sandbox_count: 0,
                last_heartbeat: Utc::now(),
                breaker: CircuitBreaker::new(),
            });
        tracing::info!("host upserted");
    }

    #[tracing::instrument(skip(self), fields(host_id = %host_id))]
    pub fn heartbeat(
        &self,
        host_id: &Uuid,
        used_vcpus: u32,
        used_mem_mib: u32,
        sandbox_count: u32,
    ) -> bool {
        if let Some(mut entry) = self.hosts.get_mut(host_id) {
            entry.used_vcpus = used_vcpus;
            entry.used_mem_mib = used_mem_mib;
            entry.sandbox_count = sandbox_count;
            entry.last_heartbeat = Utc::now();
            entry.breaker.record_success();
            tracing::info!(
                free_vcpus = entry.total_vcpus - entry.used_vcpus,
                free_mem = entry.total_mem_mib - entry.used_mem_mib,
                "heartbeat recorded"
            );
            true
        } else {
            tracing::warn!("heartbeat from unknown host");
            false
        }
    }

    pub fn record_failure(&self, host_id: &Uuid) {
        if let Some(entry) = self.hosts.get(host_id) {
            entry.value().breaker.record_failure();
            tracing::warn!("failure recorded on host");
        }
    }

    pub fn remove_host(&self, host_id: &Uuid) -> bool {
        let removed = self.hosts.remove(host_id).is_some();
        if removed {
            tracing::info!("host removed from registry");
        }
        removed
    }

    pub fn available_hosts(&self) -> Vec<HostSnapshot> {
        let now = Utc::now();
        self.hosts
            .iter()
            .filter_map(|entry| {
                let e = entry.value();
                let elapsed = now.signed_duration_since(e.last_heartbeat);
                if elapsed.num_seconds() as u64 > self.heartbeat_timeout.as_secs() {
                    return None;
                }
                Some(HostSnapshot {
                    host_id: *entry.key(),
                    address: e.address,
                    total_vcpus: e.total_vcpus,
                    used_vcpus: e.used_vcpus,
                    total_mem_mib: e.total_mem_mib,
                    used_mem_mib: e.used_mem_mib,
                    sandbox_count: e.sandbox_count,
                    circuit_state: e.breaker.state(),
                    last_heartbeat: e.last_heartbeat,
                })
            })
            .collect()
    }

    pub fn get_host(&self, host_id: &Uuid) -> Option<HostSnapshot> {
        let entry = self.hosts.get(host_id)?;
        let e = entry.value();
        Some(HostSnapshot {
            host_id: *entry.key(),
            address: e.address,
            total_vcpus: e.total_vcpus,
            used_vcpus: e.used_vcpus,
            total_mem_mib: e.total_mem_mib,
            used_mem_mib: e.used_mem_mib,
            sandbox_count: e.sandbox_count,
            circuit_state: e.breaker.state(),
            last_heartbeat: e.last_heartbeat,
        })
    }

    pub fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub fn total_sandboxes(&self) -> u32 {
        self.hosts.iter().map(|e| e.value().sandbox_count).sum()
    }
}

impl Default for HostRegistry {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}
