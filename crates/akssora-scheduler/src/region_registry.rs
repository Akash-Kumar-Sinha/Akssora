use std::net::SocketAddr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::host_registry::{HostRegistry, HostSnapshot, ResourceRequest};
use crate::placement::{PlacementDecision, best_fit_place, first_fit_place, least_loaded_place};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub id: String,
    pub display_name: String,
    pub location: String,
    pub enabled: bool,
    pub registered_at: DateTime<Utc>,
    #[serde(skip)]
    hosts: Arc<DashMap<Uuid, HostRegionEntry>>,
}

#[derive(Debug, Clone)]
struct HostRegionEntry {
    pub region_id: String,
    pub address: SocketAddr,
    pub registered_at: DateTime<Utc>,
}

pub struct RegionRegistry {
    regions: DashMap<String, Region>,
    host_regions: DashMap<Uuid, String>,
    host_registry: Arc<HostRegistry>,
}

impl RegionRegistry {
    pub fn new(host_registry: Arc<HostRegistry>) -> Self {
        Self {
            regions: DashMap::new(),
            host_regions: DashMap::new(),
            host_registry,
        }
    }

    pub fn register_region(&self, id: &str, display_name: &str, location: &str) {
        let region = Region {
            id: id.to_string(),
            display_name: display_name.to_string(),
            location: location.to_string(),
            enabled: true,
            registered_at: Utc::now(),
            hosts: Arc::new(DashMap::new()),
        };

        self.regions.insert(id.to_string(), region);
        tracing::info!(region_id = id, display_name, location, "region registered");
    }

    pub fn register_host(
        &self,
        host_id: Uuid,
        region_id: &str,
        address: SocketAddr,
        total_vcpus: u32,
        total_mem_mib: u32,
    ) -> bool {
        if !self.regions.contains_key(region_id) {
            tracing::warn!(
                host_id = %host_id,
                region_id,
                "cannot register host: region does not exist"
            );
            return false;
        }

        if let Some(region) = self.regions.get_mut(region_id) {
            region.hosts.insert(
                host_id,
                HostRegionEntry {
                    region_id: region_id.to_string(),
                    address,
                    registered_at: Utc::now(),
                },
            );
        }

        self.host_regions.insert(host_id, region_id.to_string());

        self.host_registry
            .upsert_host(host_id, address, total_vcpus, total_mem_mib);

        tracing::info!(
            host_id = %host_id,
            region_id,
            address = %address,
            "host registered in region"
        );
        true
    }

    pub fn remove_host(&self, host_id: &Uuid) -> bool {
        if let Some((_, region_id)) = self.host_regions.remove(host_id) {
            if let Some(region) = self.regions.get_mut(&region_id) {
                region.hosts.remove(host_id);
            }
            self.host_registry.remove_host(host_id);
            tracing::info!(host_id = %host_id, "host removed from region");
            return true;
        }
        false
    }

    pub fn hosts_in_region(&self, region_id: &str) -> Vec<HostSnapshot> {
        let region = match self.regions.get(region_id) {
            Some(r) => r,
            None => return Vec::new(),
        };

        region
            .hosts
            .iter()
            .filter_map(|entry| self.host_registry.get_host(entry.key()))
            .collect()
    }

    pub fn host_region(&self, host_id: &Uuid) -> Option<String> {
        self.host_regions.get(host_id).map(|r| r.value().clone())
    }

    pub fn list_regions(&self) -> Vec<Region> {
        self.regions.iter().map(|r| r.value().clone()).collect()
    }

    pub fn list_enabled_regions(&self) -> Vec<Region> {
        self.regions
            .iter()
            .filter(|r| r.value().enabled)
            .map(|r| r.value().clone())
            .collect()
    }

    pub fn is_region_available(&self, region_id: &str) -> bool {
        self.regions
            .get(region_id)
            .map(|r| r.value().enabled)
            .unwrap_or(false)
    }

    pub fn disable_region(&self, region_id: &str) -> bool {
        if let Some(mut region) = self.regions.get_mut(region_id) {
            region.enabled = false;
            tracing::info!(region_id, "region disabled");
            return true;
        }
        false
    }

    pub fn enable_region(&self, region_id: &str) -> bool {
        if let Some(mut region) = self.regions.get_mut(region_id) {
            region.enabled = true;
            tracing::info!(region_id, "region enabled");
            return true;
        }
        false
    }

    pub fn place_sandbox(
        &self,
        request: &ResourceRequest,
        region_id: Option<&str>,
    ) -> PlacementDecision {
        let hosts = match region_id {
            Some(rid) => {
                if !self.is_region_available(rid) {
                    return PlacementDecision::Unplaced {
                        reason: crate::placement::UnplacedReason::NoHosts,
                    };
                }
                self.hosts_in_region(rid)
            }
            None => self.host_registry.available_hosts(),
        };

        best_fit_place(&hosts, request)
    }

    pub fn place_sandbox_with_strategy(
        &self,
        request: &ResourceRequest,
        region_id: Option<&str>,
        strategy: PlacementStrategy,
    ) -> PlacementDecision {
        let hosts = match region_id {
            Some(rid) => {
                if !self.is_region_available(rid) {
                    return PlacementDecision::Unplaced {
                        reason: crate::placement::UnplacedReason::NoHosts,
                    };
                }
                self.hosts_in_region(rid)
            }
            None => self.host_registry.available_hosts(),
        };

        match strategy {
            PlacementStrategy::BestFit => best_fit_place(&hosts, request),
            PlacementStrategy::FirstFit => first_fit_place(&hosts, request),
            PlacementStrategy::LeastLoaded => least_loaded_place(&hosts, request),
        }
    }

    pub fn region_capacity(&self, region_id: &str) -> Option<(u32, u32, u32)> {
        let hosts = self.hosts_in_region(region_id);
        let total_vcpus: u32 = hosts.iter().map(|h| h.total_vcpus).sum();
        let total_mem: u32 = hosts.iter().map(|h| h.total_mem_mib).sum();
        let host_count: u32 = hosts.len() as u32;
        Some((total_vcpus, total_mem, host_count))
    }

    pub fn region_count(&self) -> usize {
        self.regions.len()
    }

    pub fn total_hosts(&self) -> usize {
        self.host_regions.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementStrategy {
    BestFit,
    FirstFit,
    LeastLoaded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionInfo {
    pub id: String,
    pub display_name: String,
    pub location: String,
    pub enabled: bool,
    pub host_count: u32,
    pub total_vcpus: u32,
    pub total_mem_mib: u32,
    pub free_vcpus: u32,
    pub free_mem_mib: u32,
}

impl RegionRegistry {
    pub fn get_region_info(&self, region_id: &str) -> Option<RegionInfo> {
        let region = self.regions.get(region_id)?;
        let hosts = self.hosts_in_region(region_id);

        let total_vcpus: u32 = hosts.iter().map(|h| h.total_vcpus).sum();
        let total_mem: u32 = hosts.iter().map(|h| h.total_mem_mib).sum();
        let free_vcpus: u32 = hosts.iter().map(|h| h.free_vcpus()).sum();
        let free_mem: u32 = hosts.iter().map(|h| h.free_mem_mib()).sum();

        Some(RegionInfo {
            id: region.id.clone(),
            display_name: region.display_name.clone(),
            location: region.location.clone(),
            enabled: region.enabled,
            host_count: hosts.len() as u32,
            total_vcpus,
            total_mem_mib: total_mem,
            free_vcpus,
            free_mem_mib: free_mem,
        })
    }

    pub fn list_region_info(&self) -> Vec<RegionInfo> {
        self.regions
            .iter()
            .filter_map(|entry| self.get_region_info(entry.key()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn setup() -> (RegionRegistry, Arc<HostRegistry>) {
        let host_registry = Arc::new(HostRegistry::default());
        let registry = RegionRegistry::new(host_registry.clone());
        registry.register_region("us-east-1", "US East (Virginia)", "Virginia, USA");
        registry.register_region("eu-west-1", "EU West (Ireland)", "Dublin, Ireland");
        (registry, host_registry)
    }

    #[test]
    fn register_region() {
        let (registry, _) = setup();
        assert_eq!(registry.region_count(), 2);
        assert!(registry.is_region_available("us-east-1"));
    }

    #[test]
    fn register_host_in_region() {
        let (registry, _) = setup();
        let host_id = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);

        assert!(registry.register_host(host_id, "us-east-1", addr, 8, 4096));
        assert_eq!(registry.host_region(&host_id).unwrap(), "us-east-1");
        assert_eq!(registry.total_hosts(), 1);
    }

    #[test]
    fn register_host_unknown_region() {
        let (registry, _) = setup();
        let host_id = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);

        assert!(!registry.register_host(host_id, "unknown-region", addr, 8, 4096));
        assert_eq!(registry.total_hosts(), 0);
    }

    #[test]
    fn hosts_in_region() {
        let (registry, _) = setup();
        let h1 = Uuid::new_v4();
        let h2 = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);

        registry.register_host(h1, "us-east-1", addr, 8, 4096);
        registry.register_host(h2, "eu-west-1", addr, 8, 4096);

        let us_hosts = registry.hosts_in_region("us-east-1");
        assert_eq!(us_hosts.len(), 1);
        assert_eq!(us_hosts[0].host_id, h1);
    }

    #[test]
    fn disable_region() {
        let (registry, _) = setup();
        assert!(registry.disable_region("us-east-1"));
        assert!(!registry.is_region_available("us-east-1"));
        assert!(registry.enable_region("us-east-1"));
        assert!(registry.is_region_available("us-east-1"));
    }

    #[test]
    fn place_in_region() {
        let (registry, _) = setup();
        let h1 = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);
        registry.register_host(h1, "us-east-1", addr, 8, 4096);

        let req = ResourceRequest {
            vcpus: 2,
            mem_mib: 1024,
        };

        let decision = registry.place_sandbox(&req, Some("us-east-1"));
        assert!(matches!(decision, PlacementDecision::Placed { .. }));

        let decision = registry.place_sandbox(&req, Some("eu-west-1"));
        assert!(matches!(decision, PlacementDecision::Unplaced { .. }));
    }

    #[test]
    fn place_without_region() {
        let (registry, _) = setup();
        let h1 = Uuid::new_v4();
        let h2 = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);
        registry.register_host(h1, "us-east-1", addr, 8, 4096);
        registry.register_host(h2, "eu-west-1", addr, 8, 4096);

        let req = ResourceRequest {
            vcpus: 2,
            mem_mib: 1024,
        };

        let decision = registry.place_sandbox(&req, None);
        assert!(matches!(decision, PlacementDecision::Placed { .. }));
    }

    #[test]
    fn region_capacity() {
        let (registry, _) = setup();
        let h1 = Uuid::new_v4();
        let h2 = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);
        registry.register_host(h1, "us-east-1", addr, 8, 4096);
        registry.register_host(h2, "us-east-1", addr, 4, 2048);

        let (vcpus, mem, hosts) = registry.region_capacity("us-east-1").unwrap();
        assert_eq!(vcpus, 12);
        assert_eq!(mem, 6144);
        assert_eq!(hosts, 2);
    }

    #[test]
    fn remove_host() {
        let (registry, _) = setup();
        let h1 = Uuid::new_v4();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)), 9000);
        registry.register_host(h1, "us-east-1", addr, 8, 4096);

        assert!(registry.remove_host(&h1));
        assert_eq!(registry.total_hosts(), 0);
        assert!(registry.hosts_in_region("us-east-1").is_empty());
    }
}
