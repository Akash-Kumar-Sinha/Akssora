use crate::host_registry::{HostSnapshot, ResourceRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementDecision {
    Placed { host: HostSnapshot },
    Unplaced { reason: UnplacedReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnplacedReason {
    NoHosts,
    InsufficientResources,
}

pub fn best_fit_place(hosts: &[HostSnapshot], request: &ResourceRequest) -> PlacementDecision {
    let mut best: Option<&HostSnapshot> = None;
    let mut best_free_vcpus = u32::MAX;

    for host in hosts {
        if !host.can_schedule(request) {
            continue;
        }

        let free_vcpus = host.free_vcpus();

        if free_vcpus < best_free_vcpus {
            best = Some(host);
            best_free_vcpus = free_vcpus;
        }
    }

    match best {
        Some(host) => PlacementDecision::Placed { host: host.clone() },
        None if hosts.is_empty() => PlacementDecision::Unplaced {
            reason: UnplacedReason::NoHosts,
        },
        None => PlacementDecision::Unplaced {
            reason: UnplacedReason::InsufficientResources,
        },
    }
}

pub fn first_fit_place(hosts: &[HostSnapshot], request: &ResourceRequest) -> PlacementDecision {
    for host in hosts {
        if host.can_schedule(request) {
            return PlacementDecision::Placed { host: host.clone() };
        }
    }

    if hosts.is_empty() {
        PlacementDecision::Unplaced {
            reason: UnplacedReason::NoHosts,
        }
    } else {
        PlacementDecision::Unplaced {
            reason: UnplacedReason::InsufficientResources,
        }
    }
}

pub fn least_loaded_place(hosts: &[HostSnapshot], request: &ResourceRequest) -> PlacementDecision {
    let mut best: Option<&HostSnapshot> = None;
    let mut best_score: u64 = 0;

    for host in hosts {
        if !host.can_schedule(request) {
            continue;
        }

        let score = host.free_vcpus() as u64 + host.free_mem_mib() as u64;
        if score > best_score {
            best = Some(host);
            best_score = score;
        }
    }

    match best {
        Some(host) => PlacementDecision::Placed { host: host.clone() },
        None if hosts.is_empty() => PlacementDecision::Unplaced {
            reason: UnplacedReason::NoHosts,
        },
        None => PlacementDecision::Unplaced {
            reason: UnplacedReason::InsufficientResources,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::CircuitState;
    use chrono::Utc;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn fake_host(
        vcpus: u32,
        mem: u32,
        used_vcpus: u32,
        used_mem: u32,
        circuit: CircuitState,
    ) -> HostSnapshot {
        HostSnapshot {
            host_id: uuid::Uuid::new_v4(),
            address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9000),
            total_vcpus: vcpus,
            used_vcpus,
            total_mem_mib: mem,
            used_mem_mib: used_mem,
            sandbox_count: 0,
            circuit_state: circuit,
            last_heartbeat: Utc::now(),
        }
    }

    #[test]
    fn best_fit_picks_tightest_fit() {
        let h1 = fake_host(8, 4096, 0, 0, CircuitState::Closed);
        let h2 = fake_host(8, 4096, 6, 3500, CircuitState::Closed);
        let h3 = fake_host(8, 4096, 2, 1000, CircuitState::Closed);
        let req = ResourceRequest {
            vcpus: 1,
            mem_mib: 512,
        };

        let decision = best_fit_place(&[h1, h2, h3], &req);
        assert!(
            matches!(decision, PlacementDecision::Placed { ref host } if host.free_vcpus() == 2)
        );
    }

    #[test]
    fn best_fit_skips_open_circuit() {
        let h1 = fake_host(8, 4096, 0, 0, CircuitState::Open);
        let h2 = fake_host(8, 4096, 0, 0, CircuitState::Closed);
        let h2_id = h2.host_id;
        let req = ResourceRequest {
            vcpus: 2,
            mem_mib: 1024,
        };

        let decision = best_fit_place(&[h1, h2], &req);
        assert!(
            matches!(decision, PlacementDecision::Placed { ref host } if host.host_id == h2_id)
        );
    }

    #[test]
    fn best_fit_unplaced_when_insufficient() {
        let h1 = fake_host(2, 512, 2, 400, CircuitState::Closed);
        let req = ResourceRequest {
            vcpus: 4,
            mem_mib: 2048,
        };

        let decision = best_fit_place(&[h1], &req);
        assert_eq!(
            decision,
            PlacementDecision::Unplaced {
                reason: UnplacedReason::InsufficientResources
            }
        );
    }

    #[test]
    fn best_fit_no_hosts() {
        let req = ResourceRequest {
            vcpus: 1,
            mem_mib: 256,
        };
        let decision = best_fit_place(&[], &req);
        assert_eq!(
            decision,
            PlacementDecision::Unplaced {
                reason: UnplacedReason::NoHosts
            }
        );
    }

    #[test]
    fn first_fit_picks_first_available() {
        let h1 = fake_host(8, 4096, 7, 3800, CircuitState::Closed);
        let h2 = fake_host(8, 4096, 0, 0, CircuitState::Closed);
        let h2_id = h2.host_id;
        let req = ResourceRequest {
            vcpus: 2,
            mem_mib: 512,
        };

        let decision = first_fit_place(&[h1, h2], &req);
        assert!(
            matches!(decision, PlacementDecision::Placed { ref host } if host.host_id == h2_id)
        );
    }

    #[test]
    fn least_loaded_picks_most_free() {
        let h1 = fake_host(16, 8192, 10, 6000, CircuitState::Closed);
        let h2 = fake_host(16, 8192, 2, 1000, CircuitState::Closed);
        let h2_id = h2.host_id;
        let req = ResourceRequest {
            vcpus: 1,
            mem_mib: 512,
        };

        let decision = least_loaded_place(&[h1, h2], &req);
        assert!(
            matches!(decision, PlacementDecision::Placed { ref host } if host.host_id == h2_id)
        );
    }
}
