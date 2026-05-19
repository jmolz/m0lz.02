//! TOML configuration parsing for `.pice/config.toml`.
//!
//! Moved from `pice-cli/src/config/mod.rs` in T3 of the Phase 0 refactor.
//! Both `pice-cli` (config preview + validation) and `pice-daemon` (config
//! loading at daemon startup) depend on this module.

use crate::memory::{MemoryConsumer, MemoryPolicy, MemoryStore, MemoryWriter};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// Top-level PICE configuration (`.pice/config.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiceConfig {
    pub provider: ProviderConfig,
    pub evaluation: EvaluationConfig,
    pub telemetry: TelemetryConfig,
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub init: InitConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationConfig {
    pub primary: EvalProviderConfig,
    pub adversarial: AdversarialConfig,
    pub tiers: TiersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalProviderConfig {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdversarialConfig {
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TiersConfig {
    pub tier1_models: Vec<String>,
    pub tier2_models: Vec<String>,
    pub tier3_models: Vec<String>,
    pub tier3_agent_team: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    pub enabled: bool,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub db_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitConfig {
    #[serde(default = "default_project_type")]
    pub project_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub store: MemoryStore,
    #[serde(default = "default_max_recalled_items")]
    pub max_recalled_items: usize,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_write_after")]
    pub write_after: Vec<MemoryConsumer>,
    #[serde(default = "default_read_for")]
    pub read_for: Vec<MemoryConsumer>,
}

fn default_max_recalled_items() -> usize {
    6
}

fn default_max_tokens() -> usize {
    1200
}

fn default_retention_days() -> u32 {
    90
}

fn default_write_after() -> Vec<MemoryConsumer> {
    vec![MemoryConsumer::Execute, MemoryConsumer::Handoff]
}

fn default_read_for() -> Vec<MemoryConsumer> {
    vec![
        MemoryConsumer::Prime,
        MemoryConsumer::Plan,
        MemoryConsumer::Execute,
    ]
}

fn default_project_type() -> String {
    "auto".to_string()
}

impl Default for InitConfig {
    fn default() -> Self {
        Self {
            project_type: default_project_type(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            store: MemoryStore::ProjectLearnings,
            max_recalled_items: default_max_recalled_items(),
            max_tokens: default_max_tokens(),
            retention_days: default_retention_days(),
            write_after: default_write_after(),
            read_for: default_read_for(),
        }
    }
}

impl MemoryConfig {
    pub fn validate(&self) -> Result<()> {
        if self.max_recalled_items > 20 {
            anyhow::bail!("memory.max_recalled_items must be between 0 and 20");
        }
        if self.max_tokens > 4000 {
            anyhow::bail!("memory.max_tokens must be between 0 and 4000");
        }
        if self.retention_days > 3650 {
            anyhow::bail!("memory.retention_days must be 0 or between 1 and 3650");
        }
        for consumer in &self.read_for {
            if !matches!(
                consumer,
                MemoryConsumer::Prime | MemoryConsumer::Plan | MemoryConsumer::Execute
            ) {
                anyhow::bail!(
                    "memory.read_for may include only prime, plan, and execute (got {})",
                    consumer.as_str()
                );
            }
        }
        for consumer in &self.write_after {
            if !matches!(consumer, MemoryConsumer::Execute | MemoryConsumer::Handoff) {
                anyhow::bail!(
                    "memory.write_after may include only execute and handoff (got {})",
                    consumer.as_str()
                );
            }
        }
        Ok(())
    }

    pub fn policy(&self) -> MemoryPolicy {
        let allowed_readers: BTreeSet<_> = self.read_for.iter().copied().collect();
        let allowed_writers: BTreeSet<_> = self
            .write_after
            .iter()
            .filter_map(|consumer| match consumer {
                MemoryConsumer::Execute => Some(MemoryWriter::ExecuteSummary),
                MemoryConsumer::Handoff => Some(MemoryWriter::HandoffSummary),
                _ => None,
            })
            .collect();
        MemoryPolicy {
            enabled: self.enabled,
            allowed_readers,
            allowed_writers,
            max_recalled_items: self.max_recalled_items,
            max_tokens: self.max_tokens,
        }
    }
}

impl Default for PiceConfig {
    fn default() -> Self {
        Self {
            provider: ProviderConfig {
                name: "claude-code".to_string(),
            },
            evaluation: EvaluationConfig {
                primary: EvalProviderConfig {
                    provider: "claude-code".to_string(),
                    model: "claude-opus-4-6".to_string(),
                },
                adversarial: AdversarialConfig {
                    provider: "codex".to_string(),
                    model: "gpt-5.5".to_string(),
                    effort: "high".to_string(),
                    enabled: true,
                },
                tiers: TiersConfig {
                    tier1_models: vec!["claude-opus-4-6".to_string()],
                    tier2_models: vec!["claude-opus-4-6".to_string(), "gpt-5.5".to_string()],
                    tier3_models: vec!["claude-opus-4-6".to_string(), "gpt-5.5".to_string()],
                    tier3_agent_team: true,
                },
            },
            telemetry: TelemetryConfig {
                enabled: false,
                endpoint: "https://telemetry.pice.dev/v1/events".to_string(),
            },
            metrics: MetricsConfig {
                db_path: ".pice/metrics.db".to_string(),
            },
            init: InitConfig::default(),
            memory: MemoryConfig::default(),
        }
    }
}

impl PiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;
        let config: PiceConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        self.memory.validate()
    }

    /// Save is used by future config-editing commands (Phase 3+).
    #[allow(dead_code)]
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self).context("failed to serialize config")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }
        std::fs::write(path, &content)
            .with_context(|| format!("failed to write config to {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn default_config_has_correct_values() {
        let config = PiceConfig::default();
        assert_eq!(config.provider.name, "claude-code");
        assert_eq!(config.evaluation.primary.model, "claude-opus-4-6");
        assert_eq!(config.evaluation.adversarial.model, "gpt-5.5");
        assert!(config.evaluation.adversarial.enabled);
        assert!(!config.telemetry.enabled);
        assert_eq!(config.init.project_type, "auto");
        assert!(!config.memory.enabled);
        assert_eq!(config.memory.max_recalled_items, 6);
    }

    #[test]
    fn config_roundtrip_via_toml() {
        let config = PiceConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: PiceConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.provider.name, config.provider.name);
        assert_eq!(
            parsed.evaluation.primary.model,
            config.evaluation.primary.model
        );
        assert_eq!(parsed.evaluation.tiers.tier2_models.len(), 2);
        assert_eq!(parsed.memory.store, MemoryStore::ProjectLearnings);
    }

    #[test]
    fn config_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = PiceConfig::default();
        config.save(&path).unwrap();

        let loaded = PiceConfig::load(&path).unwrap();
        assert_eq!(loaded.provider.name, "claude-code");
        assert_eq!(loaded.evaluation.adversarial.effort, "high");
        assert!(!loaded.memory.enabled);
    }

    #[test]
    fn config_load_nonexistent_returns_error() {
        let result = PiceConfig::load(&PathBuf::from("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn config_load_invalid_toml_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();
        let result = PiceConfig::load(&path);
        assert!(result.is_err());
    }

    #[test]
    fn old_config_without_memory_section_parses() {
        let content = r#"
[provider]
name = "claude-code"
[evaluation.primary]
provider = "claude-code"
model = "claude-opus-4-6"
[evaluation.adversarial]
provider = "codex"
model = "gpt-5.5"
effort = "high"
enabled = true
[evaluation.tiers]
tier1_models = ["claude-opus-4-6"]
tier2_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_agent_team = true
[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"
[metrics]
db_path = ".pice/metrics.db"
"#;
        let parsed: PiceConfig = toml::from_str(content).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.memory.store, MemoryStore::ProjectLearnings);
    }

    #[test]
    fn memory_config_rejects_unknown_keys() {
        let content = r#"
[provider]
name = "claude-code"
[evaluation.primary]
provider = "claude-code"
model = "claude-opus-4-6"
[evaluation.adversarial]
provider = "codex"
model = "gpt-5.5"
effort = "high"
enabled = true
[evaluation.tiers]
tier1_models = ["claude-opus-4-6"]
tier2_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_models = ["claude-opus-4-6", "gpt-5.5"]
tier3_agent_team = true
[telemetry]
enabled = false
endpoint = "https://telemetry.pice.dev/v1/events"
[metrics]
db_path = ".pice/metrics.db"
[memory]
enabled = true
private_state = true
"#;
        let err = toml::from_str::<PiceConfig>(content).unwrap_err();
        assert!(err.to_string().contains("private_state"));
    }

    #[test]
    fn memory_policy_hard_denies_misconfigured_readers() {
        let cfg = MemoryConfig {
            enabled: true,
            read_for: vec![
                MemoryConsumer::Prime,
                MemoryConsumer::Evaluate,
                MemoryConsumer::Commit,
            ],
            ..MemoryConfig::default()
        };
        let policy = cfg.policy();
        assert!(policy.can_read(MemoryConsumer::Prime));
        assert!(!policy.can_read(MemoryConsumer::Evaluate));
        assert!(!policy.can_read(MemoryConsumer::Commit));
    }

    #[test]
    fn memory_config_bounds_are_validated() {
        let mut cfg = MemoryConfig {
            max_tokens: 4001,
            ..MemoryConfig::default()
        };
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("max_tokens"));
        cfg.max_tokens = 1200;
        cfg.read_for = vec![MemoryConsumer::Review];
        assert!(cfg.validate().unwrap_err().to_string().contains("read_for"));
    }

    #[test]
    fn readme_configuration_examples_are_parseable() {
        let readme_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../README.md");
        let readme = std::fs::read_to_string(readme_path).unwrap();
        let mut in_toml = false;
        let mut block = String::new();
        let mut parsed_blocks = 0;

        for line in readme.lines() {
            if line.trim() == "```toml" {
                in_toml = true;
                block.clear();
                continue;
            }
            if in_toml && line.trim() == "```" {
                let _: PiceConfig = toml::from_str(&block).unwrap();
                parsed_blocks += 1;
                in_toml = false;
                continue;
            }
            if in_toml {
                block.push_str(line);
                block.push('\n');
            }
        }

        assert_eq!(parsed_blocks, 3);
    }
}
