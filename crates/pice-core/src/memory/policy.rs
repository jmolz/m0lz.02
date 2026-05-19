use std::collections::BTreeSet;

use super::types::{MemoryConsumer, MemoryWriter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPolicy {
    pub enabled: bool,
    pub allowed_readers: BTreeSet<MemoryConsumer>,
    pub allowed_writers: BTreeSet<MemoryWriter>,
    pub max_recalled_items: usize,
    pub max_tokens: usize,
}

impl MemoryPolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            allowed_readers: BTreeSet::new(),
            allowed_writers: BTreeSet::new(),
            max_recalled_items: 0,
            max_tokens: 0,
        }
    }

    pub fn can_read(&self, consumer: MemoryConsumer) -> bool {
        if consumer.is_hard_denied_reader() {
            return false;
        }
        self.enabled && self.allowed_readers.contains(&consumer)
    }

    pub fn can_write(&self, writer: MemoryWriter) -> bool {
        self.enabled && self.allowed_writers.contains(&writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn malicious_policy() -> MemoryPolicy {
        MemoryPolicy {
            enabled: true,
            allowed_readers: [
                MemoryConsumer::Prime,
                MemoryConsumer::Review,
                MemoryConsumer::Evaluate,
                MemoryConsumer::AdversarialEvaluate,
                MemoryConsumer::Commit,
            ]
            .into_iter()
            .collect(),
            allowed_writers: [MemoryWriter::ExecuteSummary].into_iter().collect(),
            max_recalled_items: 6,
            max_tokens: 1200,
        }
    }

    #[test]
    fn excluded_consumers_are_hard_denied_even_if_configured() {
        let policy = malicious_policy();
        assert!(policy.can_read(MemoryConsumer::Prime));
        assert!(!policy.can_read(MemoryConsumer::Review));
        assert!(!policy.can_read(MemoryConsumer::Evaluate));
        assert!(!policy.can_read(MemoryConsumer::AdversarialEvaluate));
        assert!(!policy.can_read(MemoryConsumer::Commit));
    }

    #[test]
    fn disabled_policy_denies_reads_and_writes() {
        let mut policy = malicious_policy();
        policy.enabled = false;
        assert!(!policy.can_read(MemoryConsumer::Prime));
        assert!(!policy.can_write(MemoryWriter::ExecuteSummary));
    }
}
