
use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AkssoraCoreError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialTemplateSpec {
    pub template: TemplateMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub agent: String,
    pub agent_version: String,
    pub category: String,
    pub maintainer: String,
    pub license: String,
    pub image: ImageSpec,
    pub dockerfile: DockerfileSpec,
    #[serde(default)]
    pub resources: ResourceSpec,
    #[serde(default)]
    pub permissions: PermissionSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSpec {
    pub base: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerfileSpec {
    pub inline: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceSpec {
    #[serde(default)]
    pub min_memory_mib: u32,
    #[serde(default)]
    pub min_vcpus: u32,
    #[serde(default)]
    pub recommended_memory_mib: u32,
    #[serde(default)]
    pub recommended_vcpus: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionSpec {
    #[serde(default)]
    pub network_egress: bool,
    #[serde(default)]
    pub max_connections: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialTemplate {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub agent: String,
    pub agent_version: String,
    pub category: String,
    pub maintainer: String,
    pub license: String,
    pub base_image: String,
    pub image_format: String,
    pub min_memory_mib: u32,
    pub min_vcpus: u32,
    pub recommended_memory_mib: u32,
    pub recommended_vcpus: u32,
    pub network_egress: bool,
    pub loaded_at: DateTime<Utc>,
}

impl OfficialTemplate {
    fn from_spec(spec: &OfficialTemplateSpec) -> Self {
        Self {
            id: spec.template.id.clone(),
            name: spec.template.name.clone(),
            version: spec.template.version.clone(),
            description: spec.template.description.clone(),
            agent: spec.template.agent.clone(),
            agent_version: spec.template.agent_version.clone(),
            category: spec.template.category.clone(),
            maintainer: spec.template.maintainer.clone(),
            license: spec.template.license.clone(),
            base_image: spec.template.image.base.clone(),
            image_format: spec.template.image.format.clone(),
            min_memory_mib: spec.template.resources.min_memory_mib,
            min_vcpus: spec.template.resources.min_vcpus,
            recommended_memory_mib: spec.template.resources.recommended_memory_mib,
            recommended_vcpus: spec.template.resources.recommended_vcpus,
            network_egress: spec.template.permissions.network_egress,
            loaded_at: Utc::now(),
        }
    }
}

pub struct OfficialTemplateRegistry {
    templates: HashMap<String, OfficialTemplate>,
    versioned: HashMap<String, OfficialTemplate>,
}

impl OfficialTemplateRegistry {
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
            versioned: HashMap::new(),
        }
    }

    pub fn load_from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        let mut registry = Self::new();

        if !dir.exists() {
            tracing::warn!(
                dir = %dir.display(),
                "official templates directory not found, registry empty"
            );
            return Ok(registry);
        }

        let entries = std::fs::read_dir(dir)
            .map_err(|e| AkssoraCoreError::TemplateBuild(format!(
                "failed to read official templates dir {}: {}", dir.display(), e
            )))?;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let toml_path = path.join("template.toml");
            if !toml_path.exists() {
                continue;
            }

            match Self::load_single(&toml_path) {
                Ok(template) => {
                    tracing::info!(
                        id = %template.id,
                        version = %template.version,
                        agent = %template.agent,
                        "loaded official template"
                    );

                    registry.templates.insert(template.id.clone(), template.clone());

                    let versioned_key = format!("{}@{}", template.id, template.version);
                    registry.versioned.insert(versioned_key, template.clone());

                    let latest_key = format!("{}@latest", template.id);
                    registry.versioned.insert(latest_key, template);
                }
                Err(e) => {
                    tracing::error!(
                        path = %toml_path.display(),
                        error = %e,
                        "failed to load official template"
                    );
                }
            }
        }

        tracing::info!(
            count = registry.templates.len(),
            "official template registry loaded"
        );

        Ok(registry)
    }

    fn load_single(path: &Path) -> Result<OfficialTemplate> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| AkssoraCoreError::TemplateBuild(format!(
                "failed to read {}: {}", path.display(), e
            )))?;

        let spec: OfficialTemplateSpec = toml::from_str(&content)
            .map_err(|e| AkssoraCoreError::TemplateBuild(format!(
                "failed to parse {}: {}", path.display(), e
            )))?;

        Ok(OfficialTemplate::from_spec(&spec))
    }

    pub fn get(&self, id: &str) -> Option<&OfficialTemplate> {
        self.templates.get(id)
    }

    pub fn get_versioned(&self, id: &str, version: &str) -> Option<&OfficialTemplate> {
        let key = format!("{}@{}", id, version);
        self.versioned.get(&key)
    }

    pub fn list(&self) -> Vec<&OfficialTemplate> {
        self.templates.values().collect()
    }

    pub fn list_by_agent(&self, agent: &str) -> Vec<&OfficialTemplate> {
        self.templates
            .values()
            .filter(|t| t.agent == agent)
            .collect()
    }

    pub fn count(&self) -> usize {
        self.templates.len()
    }
}

impl Default for OfficialTemplateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_template_toml() {
        let toml = r#"
[template]
id = "official/claude-code"
name = "Claude Code"
version = "1.0.0"
description = "Claude Code CLI"
agent = "claude-code"
agent_version = "latest"
category = "official"
maintainer = "Akssora Team"
license = "MIT"

[template.image]
base = "ubuntu:22.04"
format = "ext4"

[template.dockerfile]
inline = "FROM ubuntu:22.04"

[template.resources]
min_memory_mib = 512
min_vcpus = 2
recommended_memory_mib = 1024
recommended_vcpus = 4

[template.permissions]
network_egress = true
max_connections = 100
"#;

        let spec: OfficialTemplateSpec = toml::from_str(toml).unwrap();
        assert_eq!(spec.template.id, "official/claude-code");
        assert_eq!(spec.template.version, "1.0.0");
        assert_eq!(spec.template.resources.min_memory_mib, 512);
        assert!(spec.template.permissions.network_egress);
    }

    #[test]
    fn from_spec_conversion() {
        let spec = OfficialTemplateSpec {
            template: TemplateMetadata {
                id: "official/test".to_string(),
                name: "Test".to_string(),
                version: "1.0.0".to_string(),
                description: "Test template".to_string(),
                agent: "test-agent".to_string(),
                agent_version: "1.0.0".to_string(),
                category: "official".to_string(),
                maintainer: "Test".to_string(),
                license: "MIT".to_string(),
                image: ImageSpec {
                    base: "ubuntu:22.04".to_string(),
                    format: "ext4".to_string(),
                },
                dockerfile: DockerfileSpec {
                    inline: "FROM ubuntu:22.04".to_string(),
                },
                resources: ResourceSpec::default(),
                permissions: PermissionSpec::default(),
            },
        };

        let template = OfficialTemplate::from_spec(&spec);
        assert_eq!(template.id, "official/test");
    }

    #[test]
    fn registry_crud() {
        let mut registry = OfficialTemplateRegistry::new();
        assert_eq!(registry.count(), 0);

        let template = OfficialTemplate {
            id: "official/test".to_string(),
            name: "Test".to_string(),
            version: "1.0.0".to_string(),
            description: "Test".to_string(),
            agent: "test-agent".to_string(),
            agent_version: "1.0.0".to_string(),
            category: "official".to_string(),
            maintainer: "Test".to_string(),
            license: "MIT".to_string(),
            base_image: "ubuntu:22.04".to_string(),
            image_format: "ext4".to_string(),
            min_memory_mib: 256,
            min_vcpus: 1,
            recommended_memory_mib: 512,
            recommended_vcpus: 2,
            network_egress: true,
            loaded_at: Utc::now(),
        };

        registry.templates.insert(template.id.clone(), template.clone());
        registry.versioned.insert(
            format!("{}@{}", template.id, template.version),
            template.clone(),
        );
        registry.versioned.insert(
            format!("{}@latest", template.id),
            template.clone(),
        );

        assert_eq!(registry.count(), 1);
        assert!(registry.get("official/test").is_some());
        assert!(registry.get_versioned("official/test", "1.0.0").is_some());
        assert!(registry.get_versioned("official/test", "2.0.0").is_none());
        assert!(registry.get("official/other").is_none());
    }
}
