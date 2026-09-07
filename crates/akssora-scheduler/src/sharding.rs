#![allow(dead_code)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShardKey {
    Region(String),
    Tenant(Uuid),
    Hybrid { region: String, tenant_id: Uuid },
}

impl ShardKey {
    pub fn region(region: &str) -> Self {
        ShardKey::Region(region.to_string())
    }

    pub fn tenant(tenant_id: Uuid) -> Self {
        ShardKey::Tenant(tenant_id)
    }

    pub fn hybrid(region: &str, tenant_id: Uuid) -> Self {
        ShardKey::Hybrid {
            region: region.to_string(),
            tenant_id,
        }
    }

    pub fn shard_id(&self) -> String {
        match self {
            ShardKey::Region(r) => format!("region:{}", r),
            ShardKey::Tenant(t) => format!("tenant:{}", t),
            ShardKey::Hybrid { region, tenant_id } => {
                format!("region:{}:tenant:{}", region, tenant_id)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardInfo {
    pub shard_id: String,
    pub key: ShardKey,
    pub scheduler_address: String,
    pub host_count: u32,
    pub sandbox_count: u32,
    pub healthy: bool,
}

pub struct ShardRegistry {
    shards: dashmap::DashMap<String, ShardInfo>,
    default_shard: Option<String>,
}

impl ShardRegistry {
    pub fn new() -> Self {
        Self {
            shards: dashmap::DashMap::new(),
            default_shard: None,
        }
    }

    pub fn register(&self, info: ShardInfo) {
        tracing::info!(
            shard_id = %info.shard_id,
            hosts = info.host_count,
            sandboxes = info.sandbox_count,
            "shard registered"
        );
        self.shards.insert(info.shard_id.clone(), info);
    }

    pub fn unregister(&self, shard_id: &str) -> bool {
        self.shards.remove(shard_id).is_some()
    }

    pub fn resolve(&self, key: &ShardKey) -> Option<ShardInfo> {
        let shard_id = key.shard_id();

        if let Some(info) = self.shards.get(&shard_id) {
            return Some(info.value().clone());
        }

        if let ShardKey::Hybrid { region, .. } = key {
            let region_id = format!("region:{}", region);
            if let Some(info) = self.shards.get(&region_id) {
                return Some(info.value().clone());
            }
        }

        if let ShardKey::Hybrid { tenant_id, .. } = key {
            let tenant_id_str = format!("tenant:{}", tenant_id);
            if let Some(info) = self.shards.get(&tenant_id_str) {
                return Some(info.value().clone());
            }
        }

        if let Some(default_id) = &self.default_shard {
            return self.shards.get(default_id).map(|r| r.value().clone());
        }

        None
    }

    pub fn set_default_shard(&mut self, shard_id: &str) {
        self.default_shard = Some(shard_id.to_string());
    }

    pub fn list_shards(&self) -> Vec<ShardInfo> {
        self.shards.iter().map(|r| r.value().clone()).collect()
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn total_sandboxes(&self) -> u64 {
        self.shards
            .iter()
            .map(|r| r.value().sandbox_count as u64)
            .sum()
    }

    pub fn total_hosts(&self) -> u64 {
        self.shards
            .iter()
            .map(|r| r.value().host_count as u64)
            .sum()
    }

    pub fn least_loaded_shard(&self) -> Option<ShardInfo> {
        self.shards
            .iter()
            .filter(|r| r.value().healthy)
            .min_by_key(|r| r.value().sandbox_count)
            .map(|r| r.value().clone())
    }
}

impl Default for ShardRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ConsistentHashRing {
    ring: Vec<(u64, String)>, // (hash, shard_id)
    virtual_nodes: usize,
}

impl ConsistentHashRing {
    pub fn new(virtual_nodes: usize) -> Self {
        Self {
            ring: Vec::new(),
            virtual_nodes,
        }
    }

    pub fn add_shard(&mut self, shard_id: &str) {
        for i in 0..self.virtual_nodes {
            let key = format!("{}:{}", shard_id, i);
            let hash = self.hash(&key);
            self.ring.push((hash, shard_id.to_string()));
        }
        self.ring.sort_by_key(|(h, _)| *h);
    }

    pub fn remove_shard(&mut self, shard_id: &str) {
        self.ring.retain(|(_, id)| id != shard_id);
    }

    pub fn get_shard(&self, key: &str) -> Option<&str> {
        if self.ring.is_empty() {
            return None;
        }

        let hash = self.hash(key);

        match self.ring.binary_search_by_key(&hash, |(h, _)| *h) {
            Ok(idx) => Some(&self.ring[idx].1),
            Err(idx) => {
                let idx = if idx >= self.ring.len() { 0 } else { idx };
                Some(&self.ring[idx].1)
            }
        }
    }

    pub fn shard_count(&self) -> usize {
        let mut seen = std::collections::HashSet::new();
        for (_, id) in &self.ring {
            seen.insert(id.clone());
        }
        seen.len()
    }

    fn hash(&self, key: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }
}

pub struct GlobalScheduler {
    registry: Arc<ShardRegistry>,
    hash_ring: std::sync::Mutex<ConsistentHashRing>,
}

impl GlobalScheduler {
    pub fn new(registry: Arc<ShardRegistry>) -> Self {
        Self {
            registry,
            hash_ring: std::sync::Mutex::new(ConsistentHashRing::new(150)),
        }
    }

    pub fn route(&self, key: &ShardKey, sandbox_id: &Uuid) -> Option<ShardInfo> {
        if let Some(info) = self.registry.resolve(key) {
            return Some(info);
        }

        let ring = self.hash_ring.lock().unwrap();
        let shard_id = ring.get_shard(&sandbox_id.to_string())?;
        self.registry
            .list_shards()
            .into_iter()
            .find(|s| s.shard_id == shard_id)
    }

    pub fn register_shard(&self, info: ShardInfo) {
        let shard_id = info.shard_id.clone();
        self.registry.register(info);
        self.hash_ring.lock().unwrap().add_shard(&shard_id);
    }

    pub fn fleet_metrics(&self) -> FleetMetrics {
        let shards = self.registry.list_shards();
        let total_hosts: u64 = shards.iter().map(|s| s.host_count as u64).sum();
        let total_sandboxes: u64 = shards.iter().map(|s| s.sandbox_count as u64).sum();
        let healthy_shards = shards.iter().filter(|s| s.healthy).count();
        let unhealthy_shards = shards.len() - healthy_shards;

        FleetMetrics {
            total_shards: shards.len(),
            healthy_shards,
            unhealthy_shards,
            total_hosts,
            total_sandboxes,
            shards,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FleetMetrics {
    pub total_shards: usize,
    pub healthy_shards: usize,
    pub unhealthy_shards: usize,
    pub total_hosts: u64,
    pub total_sandboxes: u64,
    pub shards: Vec<ShardInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_shard(region: &str, sandbox_count: u32) -> ShardInfo {
        let key = ShardKey::region(region);
        ShardInfo {
            shard_id: key.shard_id(),
            key,
            scheduler_address: "127.0.0.1:4000".to_string(),
            host_count: 10,
            sandbox_count,
            healthy: true,
        }
    }

    #[test]
    fn shard_key_serialization() {
        let key = ShardKey::region("us-east-1");
        let json = serde_json::to_string(&key).unwrap();
        let parsed: ShardKey = serde_json::from_str(&json).unwrap();
        assert_eq!(key, parsed);
    }

    #[test]
    fn shard_key_hybrid() {
        let key = ShardKey::hybrid("us-east-1", Uuid::new_v4());
        let id = key.shard_id();
        assert!(id.contains("region:us-east-1"));
        assert!(id.contains("tenant:"));
    }

    #[test]
    fn registry_basic() {
        let registry = ShardRegistry::new();
        let shard = make_shard("us-east", 100);
        registry.register(shard.clone());

        let key = ShardKey::region("us-east");
        let resolved = registry.resolve(&key);
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().shard_id, "region:us-east");
    }

    #[test]
    fn registry_fallback_to_default() {
        let mut registry = ShardRegistry::new();
        registry.register(make_shard("default", 0));
        registry.set_default_shard("region:default");

        let key = ShardKey::region("nonexistent");
        let resolved = registry.resolve(&key);
        assert!(resolved.is_some());
    }

    #[test]
    fn registry_hybrid_fallback() {
        let registry = ShardRegistry::new();
        let shard = make_shard("us-east", 50);
        registry.register(shard);

        let key = ShardKey::hybrid("us-east", Uuid::new_v4());
        let resolved = registry.resolve(&key);
        assert!(resolved.is_some());
    }

    #[test]
    fn registry_fleet_metrics() {
        let registry = ShardRegistry::new();
        registry.register(make_shard("us-east", 100));
        registry.register(make_shard("eu-west", 200));
        assert_eq!(registry.total_sandboxes(), 300);
        assert_eq!(registry.total_hosts(), 20);
    }

    #[test]
    fn registry_least_loaded() {
        let registry = ShardRegistry::new();
        registry.register(make_shard("us-east", 200));
        registry.register(make_shard("eu-west", 50));
        registry.register(make_shard("ap-south", 300));

        let least = registry.least_loaded_shard().unwrap();
        assert_eq!(least.shard_id, "region:eu-west");
    }

    #[test]
    fn consistent_hash_ring_distribution() {
        let mut ring = ConsistentHashRing::new(150);
        ring.add_shard("shard-1");
        ring.add_shard("shard-2");
        ring.add_shard("shard-3");

        let mut counts: HashMap<String, usize> = HashMap::new();
        for i in 0..1000 {
            let key = format!("sandbox-{}", i);
            let shard = ring.get_shard(&key).unwrap().to_string();
            *counts.entry(shard).or_insert(0) += 1;
        }

        assert_eq!(counts.len(), 3);
        for count in counts.values() {
            assert!(*count > 200, "shard got only {} assignments", count);
        }
    }

    #[test]
    fn consistent_hash_ring_remove() {
        let mut ring = ConsistentHashRing::new(10);
        ring.add_shard("shard-1");
        ring.add_shard("shard-2");
        assert_eq!(ring.shard_count(), 2);

        ring.remove_shard("shard-1");
        assert_eq!(ring.shard_count(), 1);

        let shard = ring.get_shard("test-key").unwrap();
        assert_eq!(shard, "shard-2");
    }

    #[test]
    fn global_scheduler_route() {
        let registry = Arc::new(ShardRegistry::new());
        let scheduler = GlobalScheduler::new(registry.clone());

        let shard = make_shard("us-east", 0);
        scheduler.register_shard(shard);

        let key = ShardKey::region("us-east");
        let result = scheduler.route(&key, &Uuid::new_v4());
        assert!(result.is_some());
    }

    #[test]
    fn global_scheduler_fleet_metrics() {
        let registry = Arc::new(ShardRegistry::new());
        let scheduler = GlobalScheduler::new(registry);

        scheduler.register_shard(make_shard("us-east", 100));
        scheduler.register_shard(make_shard("eu-west", 200));

        let metrics = scheduler.fleet_metrics();
        assert_eq!(metrics.total_shards, 2);
        assert_eq!(metrics.total_sandboxes, 300);
        assert_eq!(metrics.healthy_shards, 2);
    }
}
