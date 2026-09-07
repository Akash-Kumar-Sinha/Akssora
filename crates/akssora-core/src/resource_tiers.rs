
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceTier {
    Micro,
    Small,
    Medium,
    Large,
    Xlarge,
}

impl ResourceTier {
    pub const ALL: [ResourceTier; 5] = [
        ResourceTier::Micro,
        ResourceTier::Small,
        ResourceTier::Medium,
        ResourceTier::Large,
        ResourceTier::Xlarge,
    ];

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Micro => "micro",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Xlarge => "xlarge",
        }
    }

    pub const fn vcpus(&self) -> u32 {
        match self {
            Self::Micro => 1,
            Self::Small => 1,
            Self::Medium => 2,
            Self::Large => 4,
            Self::Xlarge => 8,
        }
    }

    pub const fn mem_mib(&self) -> u32 {
        match self {
            Self::Micro => 256,
            Self::Small => 512,
            Self::Medium => 1024,
            Self::Large => 2048,
            Self::Xlarge => 4096,
        }
    }

    pub const fn max_duration(&self) -> Duration {
        match self {
            Self::Micro => Duration::from_secs(3600), // 1 hour
            Self::Small => Duration::from_secs(7200), // 2 hours
            Self::Medium => Duration::from_secs(14400), // 4 hours
            Self::Large => Duration::from_secs(28800), // 8 hours
            Self::Xlarge => Duration::from_secs(43200), // 12 hours
        }
    }

    pub const fn description(&self) -> &'static str {
        match self {
            Self::Micro => "Quick scripts, CI tasks, lightweight one-shot commands",
            Self::Small => "Lightweight agents, simple builds, single-container workloads",
            Self::Medium => "General-purpose development, multi-step builds, testing",
            Self::Large => "Heavy builds, multiple containers, large codebases",
            Self::Xlarge => "ML workloads, large compilations, memory-intensive tasks",
        }
    }

    pub fn to_spec(&self) -> ResourceTierSpec {
        ResourceTierSpec {
            tier: Some(*self),
            name: self.as_str().to_string(),
            vcpus: self.vcpus(),
            mem_mib: self.mem_mib(),
            max_duration: self.max_duration(),
            description: self.description().to_string(),
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "micro" => Some(Self::Micro),
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large" => Some(Self::Large),
            "xlarge" => Some(Self::Xlarge),
            _ => None,
        }
    }
}

impl std::str::FromStr for ResourceTier {
    type Err = ();

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::from_name(s).ok_or(())
    }
}

impl std::fmt::Display for ResourceTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceTierSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<ResourceTier>,
    pub name: String,
    pub vcpus: u32,
    pub mem_mib: u32,
    pub max_duration: Duration,
    pub description: String,
}

impl From<ResourceTier> for ResourceTierSpec {
    fn from(tier: ResourceTier) -> Self {
        tier.to_spec()
    }
}

pub fn default_tiers() -> Vec<ResourceTier> {
    ResourceTier::ALL.to_vec()
}

pub fn default_tier_specs() -> Vec<ResourceTierSpec> {
    ResourceTier::ALL.iter().map(|t| t.to_spec()).collect()
}

pub struct TierRegistry {
    tiers: Vec<ResourceTier>,
}

impl TierRegistry {
    pub fn new() -> Self {
        Self {
            tiers: default_tiers(),
        }
    }

    pub fn with_tiers(tiers: Vec<ResourceTier>) -> Self {
        Self { tiers }
    }

    pub fn get(&self, name: &str) -> Option<ResourceTier> {
        let lower = name.trim().to_lowercase();
        self.tiers.iter().copied().find(|t| t.as_str() == lower)
    }

    pub fn list(&self) -> &[ResourceTier] {
        &self.tiers
    }

    pub fn resolve(&self, name: &str) -> Option<(u32, u32, Duration)> {
        self.get(name)
            .map(|t| (t.vcpus(), t.mem_mib(), t.max_duration()))
    }

    pub fn max_duration_for(&self, tier_name: &str) -> Option<Duration> {
        self.get(tier_name).map(|t| t.max_duration())
    }
}

impl Default for TierRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSize {
    #[serde(default)]
    pub tier: Option<String>,

    #[serde(default)]
    pub vcpus: Option<u32>,

    #[serde(default)]
    pub mem_mib: Option<u32>,
}

impl SandboxSize {
    pub fn resolve(
        &self,
        registry: &TierRegistry,
    ) -> (u32, u32, Duration) {
        if let Some(ref tier_name) = self.tier {
            if let Some((vcpus, mem_mib, max_duration)) = registry.resolve(tier_name) {
                return (vcpus, mem_mib, max_duration);
            }
            tracing::warn!(
                tier = tier_name.as_str(),
                "unknown resource tier, falling back to defaults"
            );
        }

        let vcpus = self.vcpus.unwrap_or(1);
        let mem_mib = self.mem_mib.unwrap_or(512);

        let max_duration = registry
            .get("small")
            .map(|t| t.max_duration())
            .unwrap_or(Duration::from_secs(7200));

        (vcpus, mem_mib, max_duration)
    }

    pub fn from_tier(tier: ResourceTier) -> Self {
        Self {
            tier: Some(tier.as_str().to_string()),
            vcpus: Some(tier.vcpus()),
            mem_mib: Some(tier.mem_mib()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_tiers_count() {
        let tiers = default_tiers();
        assert_eq!(tiers.len(), 5);
    }

    #[test]
    fn tier_lookup() {
        let registry = TierRegistry::new();

        let small = registry.get("small").unwrap();
        assert_eq!(small.vcpus(), 1);
        assert_eq!(small.mem_mib(), 512);
        assert_eq!(small.max_duration(), Duration::from_secs(7200));

        let xlarge = registry.get("xlarge").unwrap();
        assert_eq!(xlarge.vcpus(), 8);
        assert_eq!(xlarge.mem_mib(), 4096);
    }

    #[test]
    fn tier_lookup_case_insensitive() {
        let registry = TierRegistry::new();
        assert!(registry.get("Small").is_some());
        assert!(registry.get("MEDIUM").is_some());
        assert!(registry.get("XLARGE").is_some());
    }

    #[test]
    fn tier_lookup_unknown() {
        let registry = TierRegistry::new();
        assert!(registry.get("huge").is_none());
    }

    #[test]
    fn resolve_from_tier() {
        let registry = TierRegistry::new();
        let size = SandboxSize {
            tier: Some("large".to_string()),
            vcpus: None,
            mem_mib: None,
        };

        let (vcpus, mem_mib, max_dur) = size.resolve(&registry);
        assert_eq!(vcpus, 4);
        assert_eq!(mem_mib, 2048);
        assert_eq!(max_dur, Duration::from_secs(28800));
    }

    #[test]
    fn resolve_from_raw_values() {
        let registry = TierRegistry::new();
        let size = SandboxSize {
            tier: None,
            vcpus: Some(3),
            mem_mib: Some(1536),
        };

        let (vcpus, mem_mib, _) = size.resolve(&registry);
        assert_eq!(vcpus, 3);
        assert_eq!(mem_mib, 1536);
    }

    #[test]
    fn resolve_default_when_empty() {
        let registry = TierRegistry::new();
        let size = SandboxSize {
            tier: None,
            vcpus: None,
            mem_mib: None,
        };

        let (vcpus, mem_mib, _) = size.resolve(&registry);
        assert_eq!(vcpus, 1);
        assert_eq!(mem_mib, 512);
    }

    #[test]
    fn tier_tiers_are_increasing() {
        let tiers = default_tiers();
        for window in tiers.windows(2) {
            assert!(window[0].vcpus() <= window[1].vcpus());
            assert!(window[0].mem_mib() <= window[1].mem_mib());
            assert!(window[0].max_duration() <= window[1].max_duration());
        }
    }

    #[test]
    fn tier_serialization() {
        let tier = ResourceTier::Medium;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"medium\"");
        let parsed: ResourceTier = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ResourceTier::Medium);
    }

    #[test]
    fn tier_spec_conversion() {
        let spec = ResourceTier::Micro.to_spec();
        assert_eq!(spec.name, "micro");
        assert_eq!(spec.vcpus, 1);
        assert_eq!(spec.mem_mib, 256);
        assert_eq!(spec.tier, Some(ResourceTier::Micro));
    }
}
