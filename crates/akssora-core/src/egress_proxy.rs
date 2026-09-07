use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EgressTarget {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<Protocol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
    Http,
    Https,
}

impl EgressTarget {
    pub fn domain_port(domain: &str, port: u16) -> Self {
        Self {
            host: domain.to_string(),
            port: Some(port),
            protocol: None,
        }
    }

    pub fn https(domain: &str) -> Self {
        Self {
            host: domain.to_string(),
            port: Some(443),
            protocol: Some(Protocol::Https),
        }
    }

    pub fn http(domain: &str) -> Self {
        Self {
            host: domain.to_string(),
            port: Some(80),
            protocol: Some(Protocol::Http),
        }
    }

    pub fn ip(ip: Ipv4Addr) -> Self {
        Self {
            host: ip.to_string(),
            port: None,
            protocol: None,
        }
    }

    pub fn cidr(range: &str) -> Self {
        Self {
            host: range.to_string(),
            port: None,
            protocol: None,
        }
    }

    pub fn all() -> Self {
        Self {
            host: "*".to_string(),
            port: None,
            protocol: None,
        }
    }
}

impl std::fmt::Display for EgressTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.port {
            Some(port) => write!(f, "{}:{}", self.host, port),
            None => write!(f, "{}", self.host),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressAllowlist {
    pub targets: Vec<EgressTarget>,
    pub dns_allowed: bool,
    pub max_connections: u32,
    pub max_bandwidth_bps: u64,
    pub dns_servers: Vec<SocketAddr>,
}

impl EgressAllowlist {
    pub fn deny_all() -> Self {
        Self {
            targets: Vec::new(),
            dns_allowed: false,
            max_connections: 0,
            max_bandwidth_bps: 0,
            dns_servers: Vec::new(),
        }
    }

    pub fn dns_only(dns_servers: Vec<SocketAddr>) -> Self {
        Self {
            targets: Vec::new(),
            dns_allowed: true,
            max_connections: 10,
            max_bandwidth_bps: 0,
            dns_servers,
        }
    }

    pub fn with_target(mut self, target: EgressTarget) -> Self {
        self.targets.push(target);
        self
    }

    pub fn with_targets(mut self, targets: Vec<EgressTarget>) -> Self {
        self.targets.extend(targets);
        self
    }

    pub fn with_dns(mut self, servers: Vec<SocketAddr>) -> Self {
        self.dns_allowed = true;
        self.dns_servers = servers;
        self
    }

    pub fn with_max_connections(mut self, max: u32) -> Self {
        self.max_connections = max;
        self
    }

    pub fn with_max_bandwidth(mut self, bps: u64) -> Self {
        self.max_bandwidth_bps = bps;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    pub fn target_count(&self) -> usize {
        self.targets.len()
    }
}

impl Default for EgressAllowlist {
    fn default() -> Self {
        Self::deny_all()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EgressDecision {
    Allowed,
    DeniedNoMatch,
    DeniedWrongPort,
    DeniedProtocol,
    DeniedConnectionLimit,
    DeniedDns,
}

impl EgressDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, EgressDecision::Allowed)
    }
}

pub fn check_egress(
    allowlist: &EgressAllowlist,
    target_host: &str,
    target_port: u16,
    is_dns: bool,
    active_connections: u32,
) -> EgressDecision {
    if is_dns {
        if !allowlist.dns_allowed {
            return EgressDecision::DeniedDns;
        }
        let dns_allowed = allowlist.dns_servers.iter().any(|addr| {
            let ip_str = addr.ip().to_string();
            matches_host_or_cidr(target_host, &ip_str) && target_port == addr.port()
        });
        if !dns_allowed && !allowlist.dns_servers.is_empty() {
            return EgressDecision::DeniedDns;
        }
        return EgressDecision::Allowed;
    }

    if allowlist.max_connections > 0 && active_connections >= allowlist.max_connections {
        return EgressDecision::DeniedConnectionLimit;
    }

    if allowlist.targets.is_empty() {
        return EgressDecision::DeniedNoMatch;
    }

    let mut host_matched = false;
    for target in &allowlist.targets {
        if target.host == "*" {
            if target.port.is_none_or(|p| p == target_port) {
                return EgressDecision::Allowed;
            }
            continue;
        }

        if matches_host_or_cidr(target_host, &target.host) {
            host_matched = true;

            if let Some(allowed_port) = target.port {
                if allowed_port == target_port {
                    return EgressDecision::Allowed;
                }
            } else {
                return EgressDecision::Allowed;
            }
        }
    }

    if host_matched {
        EgressDecision::DeniedWrongPort
    } else {
        EgressDecision::DeniedNoMatch
    }
}

fn matches_host_or_cidr(target_host: &str, pattern: &str) -> bool {
    if target_host == pattern {
        return true;
    }

    if let Some(domain) = pattern.strip_prefix("*.") {
        if target_host == domain {
            return true; // exact match on the base domain
        }
        if target_host.ends_with(&format!(".{}", domain)) || target_host.ends_with(domain) {
            return true;
        }
    }

    if pattern.contains('/')
        && let (Ok(target_ip), Ok((network, prefix))) =
            (target_host.parse::<Ipv4Addr>(), parse_cidr(pattern))
    {
        return cidr_contains(network, prefix, target_ip);
    }

    false
}

fn parse_cidr(cidr: &str) -> Result<(Ipv4Addr, u32), ()> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return Err(());
    }
    let network: Ipv4Addr = parts[0].parse().map_err(|_| ())?;
    let prefix: u32 = parts[1].parse().map_err(|_| ())?;
    Ok((network, prefix))
}

fn cidr_contains(network: Ipv4Addr, prefix: u32, ip: Ipv4Addr) -> bool {
    let network_u32 = u32::from(network);
    let ip_u32 = u32::from(ip);
    let mask = if prefix == 0 {
        0u32
    } else {
        u32::MAX << (32 - prefix)
    };
    (network_u32 & mask) == (ip_u32 & mask)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressProxyConfig {
    pub sandbox_id: Uuid,
    pub allowlist: EgressAllowlist,
    pub listen_addr: SocketAddr,
    pub audit_logging: bool,
    pub max_audit_body_size: usize,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
}

impl EgressProxyConfig {
    pub fn new(sandbox_id: Uuid, allowlist: EgressAllowlist) -> Self {
        let short_id = &sandbox_id.to_string()[..8];
        let port = 3128u16; // Standard proxy port
        let octets: Vec<u8> = short_id
            .chars()
            .collect::<Vec<_>>()
            .chunks(2)
            .filter_map(|c| u8::from_str_radix(&c.iter().collect::<String>(), 16).ok())
            .collect();

        let listen_addr = if octets.len() >= 2 {
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(172, 16, octets[0] % 16, octets[1] % 254 + 1)),
                port,
            )
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)), port)
        };

        Self {
            sandbox_id,
            allowlist,
            listen_addr,
            audit_logging: true,
            max_audit_body_size: 1024,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(300),
        }
    }

    pub fn with_audit_logging(mut self, enabled: bool) -> Self {
        self.audit_logging = enabled;
        self
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressAuditEntry {
    pub timestamp: DateTime<Utc>,
    pub sandbox_id: Uuid,
    pub target_host: String,
    pub target_port: u16,
    pub decision: EgressDecision,
    pub bytes_forwarded: u64,
    pub duration_ms: u64,
}

pub struct EgressProxyManager {
    configs: dashmap::DashMap<Uuid, EgressProxyConfig>,
}

impl EgressProxyManager {
    pub fn new() -> Self {
        Self {
            configs: dashmap::DashMap::new(),
        }
    }

    pub fn register(&self, config: EgressProxyConfig) {
        let sandbox_id = config.sandbox_id;
        tracing::info!(
            sandbox_id = %sandbox_id,
            targets = config.allowlist.target_count(),
            "egress proxy registered"
        );
        self.configs.insert(sandbox_id, config);
    }

    pub fn unregister(&self, sandbox_id: &Uuid) -> Option<EgressProxyConfig> {
        self.configs.remove(sandbox_id).map(|(_, c)| c)
    }

    pub fn update_allowlist(
        &self,
        sandbox_id: &Uuid,
        allowlist: EgressAllowlist,
    ) -> Result<(), EgressError> {
        if let Some(mut config) = self.configs.get_mut(sandbox_id) {
            tracing::info!(
                sandbox_id = %sandbox_id,
                new_targets = allowlist.target_count(),
                "egress allowlist updated"
            );
            config.allowlist = allowlist;
            Ok(())
        } else {
            Err(EgressError::SandboxNotFound(*sandbox_id))
        }
    }

    pub fn get_config(&self, sandbox_id: &Uuid) -> Option<EgressProxyConfig> {
        self.configs.get(sandbox_id).map(|c| c.value().clone())
    }

    pub fn check_egress(
        &self,
        sandbox_id: &Uuid,
        target_host: &str,
        target_port: u16,
        is_dns: bool,
        active_connections: u32,
    ) -> EgressDecision {
        match self.configs.get(sandbox_id) {
            Some(config) => check_egress(
                &config.allowlist,
                target_host,
                target_port,
                is_dns,
                active_connections,
            ),
            None => EgressDecision::DeniedNoMatch,
        }
    }

    pub fn list_sandboxes(&self) -> Vec<Uuid> {
        self.configs.iter().map(|r| *r.key()).collect()
    }

    pub fn count(&self) -> usize {
        self.configs.len()
    }
}

impl Default for EgressProxyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    #[error("sandbox not found: {0}")]
    SandboxNotFound(Uuid),

    #[error("policy not found for sandbox: {0}")]
    PolicyNotFound(Uuid),

    #[error("allowlist update failed: {0}")]
    UpdateFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPolicyMode {
    AllowAll,
    DenyAll,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressRule {
    pub host_pattern: String,
    pub port: Option<u16>,
    #[serde(default)]
    pub inject_headers: Vec<HeaderInjection>,
    pub forward_url: Option<String>,
    #[serde(default)]
    pub mirror: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderInjection {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub sandbox_id: Uuid,
    pub mode: NetworkPolicyMode,
    #[serde(default)]
    pub rules: Vec<EgressRule>,
    #[serde(default)]
    pub allowlist: EgressAllowlist,
    pub updated_at: DateTime<Utc>,
}

impl NetworkPolicy {
    pub fn deny_all(sandbox_id: Uuid) -> Self {
        Self {
            sandbox_id,
            mode: NetworkPolicyMode::DenyAll,
            rules: Vec::new(),
            allowlist: EgressAllowlist::deny_all(),
            updated_at: Utc::now(),
        }
    }

    pub fn allow_all(sandbox_id: Uuid) -> Self {
        Self {
            sandbox_id,
            mode: NetworkPolicyMode::AllowAll,
            rules: Vec::new(),
            allowlist: EgressAllowlist::deny_all().with_target(EgressTarget::all()),
            updated_at: Utc::now(),
        }
    }

    pub fn custom(sandbox_id: Uuid, rules: Vec<EgressRule>) -> Self {
        Self {
            sandbox_id,
            mode: NetworkPolicyMode::Custom,
            rules,
            allowlist: EgressAllowlist::deny_all(),
            updated_at: Utc::now(),
        }
    }

    pub fn check_target(&self, target_host: &str, target_port: u16) -> PolicyDecision {
        match self.mode {
            NetworkPolicyMode::AllowAll => PolicyDecision::Allowed { matched_rule: None },
            NetworkPolicyMode::DenyAll => PolicyDecision::Denied,
            NetworkPolicyMode::Custom => {
                for rule in &self.rules {
                    if matches_host_or_cidr(target_host, &rule.host_pattern)
                        && rule.port.is_none_or(|p| p == target_port)
                    {
                        return PolicyDecision::Allowed {
                            matched_rule: Some(rule.clone()),
                        };
                    }
                }
                let decision = check_egress(&self.allowlist, target_host, target_port, false, 0);
                if decision.is_allowed() {
                    PolicyDecision::Allowed { matched_rule: None }
                } else {
                    PolicyDecision::Denied
                }
            }
        }
    }
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self::deny_all(Uuid::nil())
    }
}

#[derive(Debug, Clone)]
pub enum PolicyDecision {
    Allowed { matched_rule: Option<EgressRule> },
    Denied,
}

pub struct SecretReference {
    pub name: String,
    pub account_id: Uuid,
    pub domain: String,
}

pub struct ResolvedSecret {
    pub name: String,
    pub value: String,
}

pub struct NetworkPolicyStore {
    policies: dashmap::DashMap<Uuid, NetworkPolicy>,
}

impl NetworkPolicyStore {
    pub fn new() -> Self {
        Self {
            policies: dashmap::DashMap::new(),
        }
    }

    pub fn get(&self, sandbox_id: &Uuid) -> Option<NetworkPolicy> {
        self.policies.get(sandbox_id).map(|p| p.value().clone())
    }

    pub fn update(&self, policy: NetworkPolicy) {
        let sandbox_id = policy.sandbox_id;
        tracing::info!(
            sandbox_id = %sandbox_id,
            mode = ?policy.mode,
            rules = policy.rules.len(),
            "network policy updated (hot-reload)"
        );
        self.policies.insert(sandbox_id, policy);
    }

    pub fn remove(&self, sandbox_id: &Uuid) -> Option<NetworkPolicy> {
        self.policies.remove(sandbox_id).map(|(_, p)| p)
    }

    pub fn list_sandboxes(&self) -> Vec<Uuid> {
        self.policies.iter().map(|r| *r.key()).collect()
    }

    pub fn count(&self) -> usize {
        self.policies.len()
    }
}

impl Default for NetworkPolicyStore {
    fn default() -> Self {
        Self::new()
    }
}

pub fn generate_proxy_egress_rules(
    sandbox_id: &Uuid,
    tap_device: &str,
    proxy_addr: &SocketAddr,
) -> String {
    let table_name = format!("akssora_egress_{}", &sandbox_id.to_string()[..8]);
    let proxy_ip = proxy_addr.ip();
    let proxy_port = proxy_addr.port();

    let rules = format!(
        r#"table inet {table_name} {{
    chain egress {{
        type filter hook output priority mangle; policy drop;

        # Allow loopback
        oifname "lo" accept

        # Allow traffic TO the proxy (for sandbox-internal proxy routing)
        ip daddr {proxy_ip} tcp dport {proxy_port} accept

        # Allow DNS if going through the proxy
        # (DNS queries to the proxy are allowed; direct DNS is blocked)

        # Allow established connections back from the proxy
        ct state established,related accept

        # Drop all other outbound traffic from the sandbox
        iifname "{tap_device}" drop
    }}
}}"#,
        table_name = table_name,
        proxy_ip = proxy_ip,
        proxy_port = proxy_port,
        tap_device = tap_device,
    );

    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_all_has_no_targets() {
        let allowlist = EgressAllowlist::deny_all();
        assert!(allowlist.is_empty());
        assert_eq!(allowlist.target_count(), 0);
        assert!(!allowlist.dns_allowed);
    }

    #[test]
    fn deny_all_blocks_everything() {
        let allowlist = EgressAllowlist::deny_all();
        let decision = check_egress(&allowlist, "example.com", 443, false, 0);
        assert_eq!(decision, EgressDecision::DeniedNoMatch);
    }

    #[test]
    fn wildcard_allows_all_ports() {
        let allowlist = EgressAllowlist::deny_all().with_target(EgressTarget::all());
        assert_eq!(
            check_egress(&allowlist, "example.com", 443, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "example.com", 80, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "other.com", 9999, false, 0),
            EgressDecision::Allowed
        );
    }

    #[test]
    fn exact_domain_match() {
        let allowlist =
            EgressAllowlist::deny_all().with_target(EgressTarget::https("api.example.com"));
        assert_eq!(
            check_egress(&allowlist, "api.example.com", 443, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "api.example.com", 80, false, 0),
            EgressDecision::DeniedWrongPort
        );
        assert_eq!(
            check_egress(&allowlist, "other.com", 443, false, 0),
            EgressDecision::DeniedNoMatch
        );
    }

    #[test]
    fn wildcard_subdomain_match() {
        let allowlist = EgressAllowlist::deny_all()
            .with_target(EgressTarget::domain_port("*.example.com", 443));
        assert_eq!(
            check_egress(&allowlist, "api.example.com", 443, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "foo.bar.example.com", 443, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "example.com", 443, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "api.example.com", 80, false, 0),
            EgressDecision::DeniedWrongPort
        );
    }

    #[test]
    fn cidr_match() {
        let allowlist = EgressAllowlist::deny_all().with_target(EgressTarget::cidr("10.0.0.0/8"));
        assert_eq!(
            check_egress(&allowlist, "10.1.2.3", 443, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "10.255.255.255", 80, false, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "11.0.0.1", 443, false, 0),
            EgressDecision::DeniedNoMatch
        );
    }

    #[test]
    fn connection_limit_enforced() {
        let allowlist = EgressAllowlist::deny_all()
            .with_target(EgressTarget::all())
            .with_max_connections(5);
        assert_eq!(
            check_egress(&allowlist, "example.com", 443, false, 4),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "example.com", 443, false, 5),
            EgressDecision::DeniedConnectionLimit
        );
    }

    #[test]
    fn dns_check() {
        let allowlist = EgressAllowlist::deny_all().with_dns(vec!["8.8.8.8:53".parse().unwrap()]);

        assert_eq!(
            check_egress(&allowlist, "8.8.8.8", 53, true, 0),
            EgressDecision::Allowed
        );
        assert_eq!(
            check_egress(&allowlist, "1.1.1.1", 53, true, 0),
            EgressDecision::DeniedDns
        );
        let no_dns = EgressAllowlist::deny_all();
        assert_eq!(
            check_egress(&no_dns, "8.8.8.8", 53, true, 0),
            EgressDecision::DeniedDns
        );
    }

    #[test]
    fn unknown_sandbox_denied() {
        let manager = EgressProxyManager::new();
        let decision = manager.check_egress(&Uuid::new_v4(), "example.com", 443, false, 0);
        assert_eq!(decision, EgressDecision::DeniedNoMatch);
    }

    #[test]
    fn proxy_manager_crud() {
        let manager = EgressProxyManager::new();
        let sandbox_id = Uuid::new_v4();
        let config = EgressProxyConfig::new(sandbox_id, EgressAllowlist::deny_all());

        manager.register(config);
        assert_eq!(manager.count(), 1);
        assert!(manager.get_config(&sandbox_id).is_some());

        manager.unregister(&sandbox_id);
        assert_eq!(manager.count(), 0);
        assert!(manager.get_config(&sandbox_id).is_none());
    }

    #[test]
    fn update_allowlist() {
        let manager = EgressProxyManager::new();
        let sandbox_id = Uuid::new_v4();
        let config = EgressProxyConfig::new(sandbox_id, EgressAllowlist::deny_all());
        manager.register(config);

        let new_allowlist =
            EgressAllowlist::deny_all().with_target(EgressTarget::https("api.example.com"));
        manager
            .update_allowlist(&sandbox_id, new_allowlist)
            .unwrap();

        let decision = manager.check_egress(&sandbox_id, "api.example.com", 443, false, 0);
        assert_eq!(decision, EgressDecision::Allowed);
    }

    #[test]
    fn egress_target_display() {
        let t1 = EgressTarget::domain_port("api.example.com", 443);
        assert_eq!(t1.to_string(), "api.example.com:443");

        let t2 = EgressTarget::ip(Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(t2.to_string(), "10.0.0.1");
    }

    #[test]
    fn egress_decision_is_allowed() {
        assert!(EgressDecision::Allowed.is_allowed());
        assert!(!EgressDecision::DeniedNoMatch.is_allowed());
        assert!(!EgressDecision::DeniedWrongPort.is_allowed());
        assert!(!EgressDecision::DeniedDns.is_allowed());
    }

    #[test]
    fn proxy_config_listen_addr() {
        let sandbox_id = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let config = EgressProxyConfig::new(sandbox_id, EgressAllowlist::deny_all());
        assert!(config.listen_addr.port() > 0);
    }

    #[test]
    fn nftables_rules_contain_proxy() {
        let sandbox_id = Uuid::new_v4();
        let proxy_addr: SocketAddr = "172.16.0.1:3128".parse().unwrap();
        let rules = generate_proxy_egress_rules(&sandbox_id, "tap-test", &proxy_addr);
        assert!(rules.contains("172.16.0.1"));
        assert!(rules.contains("3128"));
        assert!(rules.contains("tap-test"));
        assert!(rules.contains("policy drop"));
    }

    #[test]
    fn cidr_parse_and_check() {
        let (network, prefix) = parse_cidr("10.0.0.0/8").unwrap();
        assert_eq!(network, Ipv4Addr::new(10, 0, 0, 0));
        assert_eq!(prefix, 8);
        assert!(cidr_contains(network, prefix, Ipv4Addr::new(10, 1, 2, 3)));
        assert!(!cidr_contains(network, prefix, Ipv4Addr::new(11, 0, 0, 1)));
    }

    #[test]
    fn deny_all_serialization() {
        let allowlist = EgressAllowlist::deny_all();
        let json = serde_json::to_string(&allowlist).unwrap();
        let parsed: EgressAllowlist = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_empty());
        assert!(!parsed.dns_allowed);
    }

    #[test]
    fn egress_audit_entry_serializes() {
        let entry = EgressAuditEntry {
            timestamp: Utc::now(),
            sandbox_id: Uuid::new_v4(),
            target_host: "api.example.com".into(),
            target_port: 443,
            decision: EgressDecision::Allowed,
            bytes_forwarded: 1024,
            duration_ms: 50,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("api.example.com"));
    }
}
