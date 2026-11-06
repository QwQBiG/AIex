//! Two-file candidate work is confined to the private backup worker.

use std::path::Path;

use ai_ex_control::{MemoryMergeConflict, MemoryMergeSummary};
use ai_ex_memory::{MemoryMergeError, merge_profile_candidates_checked};
use serde::{Deserialize, Serialize};

use super::{Reply, read_selected};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MergeRequest {
    pub base_path: String,
    pub incoming_path: String,
    pub profile: String,
    pub write: Option<MergeWrite>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MergeWrite {
    pub base_digest: [u8; 32],
    pub incoming_digest: [u8; 32],
    pub output: String,
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.chars().count() <= 1024
        && !path.chars().any(char::is_control)
        && Path::new(path).is_absolute()
}

pub(super) fn inspect(request: MergeRequest) -> Reply {
    let writing = request.write.is_some();
    let rejected = || {
        if writing {
            Reply::CandidateRejected
        } else {
            Reply::Invalid
        }
    };
    if !valid_path(&request.base_path)
        || !valid_path(&request.incoming_path)
        || request.profile.trim().is_empty()
        || request.profile.chars().count() > 128
        || request.profile.chars().any(char::is_control)
    {
        return rejected();
    }
    let Some(base) = read_selected(Path::new(&request.base_path)) else {
        return rejected();
    };
    let Some(incoming) = read_selected(Path::new(&request.incoming_path)) else {
        return rejected();
    };
    let base_digest = ai_ex_migrate::legacy_memory::backup_digest(&base);
    let incoming_digest = ai_ex_migrate::legacy_memory::backup_digest(&incoming);
    if let Some(write) = &request.write
        && (write.base_digest != base_digest || write.incoming_digest != incoming_digest)
    {
        return Reply::CandidateRejected;
    }
    let candidate = match merge_profile_candidates_checked(&base, &incoming, &request.profile) {
        Ok(candidate) => candidate,
        Err(error) => {
            return if writing {
                Reply::CandidateRejected
            } else {
                Reply::MergeConflict {
                    reason: conflict(error),
                }
            };
        }
    };
    if let Some(write) = request.write {
        if !valid_path(&write.output) {
            return Reply::CandidateRejected;
        }
        return match ai_ex_migrate::write_new_atomic_existing_dir(
            Path::new(&write.output),
            &candidate.content,
        ) {
            Ok(temporary_retained) => Reply::CandidateWritten { temporary_retained },
            Err(_) => Reply::CandidateUncertain,
        };
    }
    let report = candidate.report;
    Reply::MergeReady {
        summary: MemoryMergeSummary {
            base_memory_records: report.base_memory_records,
            incoming_memory_records: report.incoming_memory_records,
            entity_candidates: report.entity_candidates,
            source_associations: report.source_associations,
            inactive_source_associations: report.inactive_source_associations,
            distinct_embedding_spaces: report.distinct_embedding_spaces,
            base_empty: report.base_empty,
            incoming_empty: report.incoming_empty,
        },
        base_digest,
        incoming_digest,
    }
}

fn conflict(error: MemoryMergeError) -> MemoryMergeConflict {
    match error {
        MemoryMergeError::InvalidData => MemoryMergeConflict::InvalidData,
        MemoryMergeError::ProfileOrVersion => MemoryMergeConflict::ProfileOrVersion,
        MemoryMergeError::LimitExceeded => MemoryMergeConflict::LimitExceeded,
        MemoryMergeError::MemoryIdentityOverlap => MemoryMergeConflict::MemoryIdentityOverlap,
        MemoryMergeError::EntityIdentityOverlap => MemoryMergeConflict::EntityIdentityOverlap,
        MemoryMergeError::SourceAssociationOverlap => MemoryMergeConflict::SourceAssociationOverlap,
        MemoryMergeError::UnstableSources => MemoryMergeConflict::UnstableSources,
        MemoryMergeError::EmbeddingSpaceOverlap => MemoryMergeConflict::EmbeddingSpaceOverlap,
        MemoryMergeError::EmbeddingDimensionsConflict => {
            MemoryMergeConflict::EmbeddingDimensionsConflict
        }
        MemoryMergeError::EmptyInputs => MemoryMergeConflict::EmptyInputs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    fn record() -> Vec<u8> {
        serde_json::json!({
            "schema_version": 2, "record_type": "memory", "id": Uuid::new_v4(),
            "profile_id": "test", "turn_id": Uuid::new_v4(), "created_ms": 1,
            "user_text": "synthetic", "assistant_text": "", "source": "automatic",
            "revision": 1, "unknown_extension": {"kept": true}
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn preview_then_write_checks_both_sources_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("merge-worker-test-{}", Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let base_path = dir.join("base.jsonl");
        let incoming_path = dir.join("incoming.jsonl");
        let output = dir.join("candidate.jsonl");
        let base = record();
        let incoming = record();
        fs::write(&base_path, &base).unwrap();
        fs::write(&incoming_path, &incoming).unwrap();
        let request = |write| MergeRequest {
            base_path: base_path.to_str().unwrap().into(),
            incoming_path: incoming_path.to_str().unwrap().into(),
            profile: "test".into(),
            write,
        };
        let Reply::MergeReady {
            summary,
            base_digest,
            incoming_digest,
        } = inspect(request(None))
        else {
            panic!("synthetic preview must succeed");
        };
        assert_eq!(summary.base_memory_records, 1);
        assert_eq!(summary.incoming_memory_records, 1);
        let write = || {
            Some(MergeWrite {
                base_digest,
                incoming_digest,
                output: output.to_str().unwrap().into(),
            })
        };
        for path in [&base_path, &incoming_path] {
            let original = fs::read(path).unwrap();
            fs::write(path, record()).unwrap();
            assert!(matches!(
                inspect(request(write())),
                Reply::CandidateRejected
            ));
            assert!(!output.exists());
            fs::write(path, original).unwrap();
        }
        assert!(matches!(
            inspect(request(write())),
            Reply::CandidateWritten {
                temporary_retained: false
            }
        ));
        let mut expected = base.clone();
        expected.push(b'\n');
        expected.extend_from_slice(&incoming);
        assert_eq!(fs::read(&output).unwrap(), expected);
        assert!(matches!(
            inspect(request(write())),
            Reply::CandidateUncertain
        ));
        assert_eq!(fs::read(&output).unwrap(), expected);
        assert_eq!(fs::read(&base_path).unwrap(), base);
        assert_eq!(fs::read(&incoming_path).unwrap(), incoming);
        for path in [&base_path, &incoming_path, &output] {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }
}
