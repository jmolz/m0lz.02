//! TOML configuration parsing for `.pice/config.toml`.
//!
//! Moved from `pice-cli/src/config/mod.rs` in T3 of the Phase 0 refactor.
//! Both `pice-cli` (config preview + validation) and `pice-daemon` (config
//! loading at daemon startup) depend on this module.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
        }
    }
}

impl PiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config from {}", path.display()))?;
        let config: PiceConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config from {}", path.display()))?;
        Ok(config)
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
}
