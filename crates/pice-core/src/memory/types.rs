use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConsumer {
    Prime,
    Plan,
    Execute,
    Review,
    Evaluate,
    AdversarialEvaluate,
    Commit,
    Handoff,
}

impl MemoryConsumer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prime => "prime",
            Self::Plan => "plan",
            Self::Execute => "execute",
            Self::Review => "review",
            Self::Evaluate => "evaluate",
            Self::AdversarialEvaluate => "adversarial_evaluate",
            Self::Commit => "commit",
            Self::Handoff => "handoff",
        }
    }

    pub fn is_hard_denied_reader(&self) -> bool {
        matches!(
            self,
            Self::Review | Self::Evaluate | Self::AdversarialEvaluate | Self::Commit
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriter {
    ExecuteSummary,
    HandoffSummary,
    OperatorNote,
}

impl MemoryWriter {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ExecuteSummary => "execute_summary",
            Self::HandoffSummary => "handoff_summary",
            Self::OperatorNote => "operator_note",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStore {
    #[default]
    ProjectLearnings,
    PrivateState,
    Both,
}

impl MemoryStore {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ProjectLearnings => "project_learnings",
            Self::PrivateState => "private_state",
            Self::Both => "both",
        }
    }

    pub fn includes_project_learnings(&self) -> bool {
        matches!(self, Self::ProjectLearnings | Self::Both)
    }

    pub fn includes_private_state(&self) -> bool {
        matches!(self, Self::PrivateState | Self::Both)
    }

    pub fn concrete_stores(&self) -> Vec<Self> {
        match self {
            Self::ProjectLearnings => vec![Self::ProjectLearnings],
            Self::PrivateState => vec![Self::PrivateState],
            Self::Both => vec![Self::ProjectLearnings, Self::PrivateState],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionStatus {
    Clean,
    Rejected,
}

impl RedactionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecord {
    pub id: String,
    pub created_at: String,
    pub source: MemoryWriter,
    pub store: MemoryStore,
    pub project_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    pub redaction_status: RedactionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction_reason: Option<String>,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBrief {
    pub source: String,
    pub records: Vec<MemoryBriefRecord>,
    pub estimated_tokens: usize,
}

impl MemoryBrief {
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBriefRecord {
    pub id: String,
    pub source: String,
    pub created_at: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl From<&MemoryRecord> for MemoryBriefRecord {
    fn from(record: &MemoryRecord) -> Self {
        Self {
            id: record.id.clone(),
            source: record.source.as_str().to_string(),
            created_at: record.created_at.clone(),
            title: record.title.clone(),
            body: record.body.clone(),
            tags: record.tags.clone(),
        }
    }
}

/// Conservative deterministic token estimate used for v1 budgeting.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

pub fn stable_record_id(seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("mem_{}", &hash[..24])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_record_ids_have_mem_prefix() {
        let id = stable_record_id("project/run/title/body");
        assert!(id.starts_with("mem_"));
        assert_eq!(id.len(), 28);
        assert_eq!(id, stable_record_id("project/run/title/body"));
    }

    #[test]
    fn token_estimate_uses_ceil_four_chars() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}
