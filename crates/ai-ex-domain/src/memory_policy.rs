use serde::{Deserialize, Serialize};

/// Hybrid preserves the current Rust ranking; Legacy reproduces old candidate and ranking rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRecallPolicy {
    #[default]
    Hybrid,
    Legacy,
}
