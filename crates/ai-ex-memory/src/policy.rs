//! Pure compatibility formulas from the old Python memory modules.
//!
//! Callers decide which policy to use and whether to persist a result. These helpers
//! do not count accesses, select vector candidates, delete records, or run timers.
//! The clock is explicit and future timestamps have zero elapsed age.

use ai_ex_domain::{AppError, MemoryModality};
use serde_json::{Map, Value};

const HOUR_MS: u128 = 3_600_000;
const THIRTY_DAYS_MS: u128 = 30 * 24 * HOUR_MS;

/// Fallback only for metadata without an importance score (memory_core.py:2500-2551).
/// Valid legacy fields use Unicode character counts, array lengths and integer
/// access counts. Missing or incorrectly typed fields contribute no extra weight.
/// Existing explicit importance is deliberately left for the caller to preserve.
pub(crate) fn legacy_core_importance(metadata: &Map<String, Value>) -> f64 {
    let content_chars = metadata
        .get("content")
        .and_then(Value::as_str)
        .map_or(0, |content| content.chars().count());
    let entity_count = metadata
        .get("entities")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let access_count = metadata
        .get("access_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let interaction_type = metadata
        .get("interaction_type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut score: f64 = 0.3;
    for passed in [
        content_chars > 50,
        content_chars > 150,
        entity_count > 0,
        entity_count > 2,
        access_count > 3,
        access_count > 10,
    ] {
        if passed {
            score += 0.1;
        }
    }
    score += match interaction_type {
        "preference" | "personal_info" | "goal" => 0.2,
        "emotion" | "context" => 0.1,
        _ => 0.0,
    };
    score.clamp(0.0, 1.0)
}

/// The actual formula in memory_core.py:2573 reaches zero at 16.8 hours.
/// Its old "one week" comment described a different formula.
pub(crate) fn legacy_recency_boost(created_ms: u128, now_ms: u128) -> f64 {
    let age_hours = now_ms.saturating_sub(created_ms) as f64 / HOUR_MS as f64;
    (0.1 - age_hours / 168.0).max(0.0)
}

/// Score only the already selected candidates, as memory_core.py:1182-1216 did.
/// No similarity term is added, and no stored importance is modified here.
pub(crate) fn legacy_rank_score(
    importance: f64,
    created_ms: u128,
    now_ms: u128,
) -> Result<f64, AppError> {
    validate_importance(importance)?;
    Ok((importance + legacy_recency_boost(created_ms, now_ms)).min(1.0))
}

/// The old multimodal_memory.py:451-472 content/feature heuristic.
/// The caller supplies the explicit modality, including legacy multimodal records
/// with absent feature maps. Its unused access_frequency weight is not included.
pub(crate) fn legacy_multimodal_importance(
    modality: MemoryModality,
    text_chars: usize,
    entity_count: usize,
) -> f64 {
    let modality_factor = match modality {
        MemoryModality::Text => 1.0,
        MemoryModality::Audio | MemoryModality::Visual => 1.1,
        MemoryModality::Multimodal => 1.2,
    };
    (0.3 * (text_chars.min(500) as f64 / 500.0)
        + 0.2 * (entity_count.min(10) as f64 / 10.0)
        + 0.2 * modality_factor)
        .clamp(0.1, 1.0)
}

/// The intended arithmetic in multimodal_memory.py:355-366, without its missing-np
/// failure. Thirty days is the e-folding time, not the half-life. This uses the
/// supplied score and full age; repeated calls on adjusted scores compound decay.
pub(crate) fn legacy_adjusted_importance(
    importance: f64,
    access_count: u64,
    created_ms: u128,
    now_ms: u128,
) -> Result<f64, AppError> {
    validate_importance(importance)?;
    let age = now_ms.saturating_sub(created_ms) as f64;
    let decay = (-age / THIRTY_DAYS_MS as f64).exp();
    let access_factor = 1.0 + access_count as f64 * 0.1;
    Ok((importance * access_factor * decay).clamp(0.1, 1.0))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PolicyMemory {
    pub(crate) created_ms: u128,
    pub(crate) importance: f64,
}

/// Return positions eligible under multimodal_memory.py:390-398, in input order.
/// The pressure test uses the whole input count, not a shrinking survivor count.
/// This is a preview: it neither deletes records nor guarantees a capacity limit.
/// The caller decides when to request the preview; no automatic cleanup is implied.
pub(crate) fn legacy_cleanup_candidates(
    memories: &[PolicyMemory],
    now_ms: u128,
    max_memories: usize,
) -> Result<Vec<usize>, AppError> {
    if max_memories == 0 {
        return Err(AppError::configuration(
            "memory cleanup capacity must be positive",
        ));
    }
    for memory in memories {
        validate_importance(memory.importance)?;
    }
    let under_pressure = memories.len() as u128 * 10 > max_memories as u128 * 9;
    Ok(memories
        .iter()
        .enumerate()
        .filter_map(|(index, memory)| {
            let old = now_ms.saturating_sub(memory.created_ms) > THIRTY_DAYS_MS;
            ((old && memory.importance < 0.3) || (under_pressure && memory.importance < 0.2))
                .then_some(index)
        })
        .collect())
}

fn validate_importance(importance: f64) -> Result<(), AppError> {
    if importance.is_finite() && (0.0..=1.0).contains(&importance) {
        Ok(())
    } else {
        Err(AppError::configuration(
            "memory importance must be finite and in 0..=1",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u128 = 1_800_000_000_000;

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12,
            "expected {expected}, got {actual}"
        );
    }

    fn metadata_score(value: Value) -> f64 {
        legacy_core_importance(value.as_object().expect("fixture is an object"))
    }

    #[test]
    fn core_content_boundaries_count_unicode_characters() {
        for (length, expected) in [(0, 0.3), (50, 0.3), (51, 0.4), (150, 0.4), (151, 0.5)] {
            close(
                metadata_score(json!({"content": "界".repeat(length)})),
                expected,
            );
        }
    }

    #[test]
    fn core_entity_and_access_thresholds_are_strict() {
        for (count, expected) in [(0, 0.3), (1, 0.4), (2, 0.4), (3, 0.5)] {
            close(
                metadata_score(json!({"entities": vec!["synthetic"; count]})),
                expected,
            );
        }
        for (count, expected) in [(0, 0.3), (3, 0.3), (4, 0.4), (10, 0.4), (11, 0.5)] {
            close(metadata_score(json!({"access_count": count})), expected);
        }
    }

    #[test]
    fn core_type_weights_and_combined_cap_match_legacy() {
        for (kind, expected) in [
            ("unknown", 0.3),
            ("preference", 0.5),
            ("personal_info", 0.5),
            ("goal", 0.5),
            ("emotion", 0.4),
            ("context", 0.4),
            ("PREFERENCE", 0.3),
        ] {
            close(metadata_score(json!({"interaction_type": kind})), expected);
        }
        close(
            metadata_score(json!({
                "content": "x".repeat(151),
                "entities": ["one", "two", "three"],
                "interaction_type": "goal",
                "access_count": 11
            })),
            1.0,
        );
        close(metadata_score(json!({})), 0.3);
    }

    #[test]
    fn ranking_uses_actual_16_point_8_hour_window() {
        for (age_ms, expected_boost) in [
            (0, 0.1),
            (30_240_000, 0.05),
            (60_480_000, 0.0),
            (168 * HOUR_MS, 0.0),
        ] {
            close(legacy_recency_boost(NOW - age_ms, NOW), expected_boost);
            close(
                legacy_rank_score(0.5, NOW - age_ms, NOW).unwrap(),
                0.5 + expected_boost,
            );
        }
        close(legacy_rank_score(0.95, NOW, NOW).unwrap(), 1.0);
        close(legacy_recency_boost(NOW + HOUR_MS, NOW), 0.1);
        close(legacy_recency_boost(0, u128::MAX), 0.0);
        let recent = legacy_rank_score(0.65, NOW, NOW).unwrap();
        let old = legacy_rank_score(0.7, NOW - THIRTY_DAYS_MS, NOW).unwrap();
        assert!(recent > old);
    }

    #[test]
    fn multimodal_fixed_fixtures_keep_audio_and_visual_equal() {
        for (modality, entities, expected) in [
            (MemoryModality::Text, 0, 0.35),
            (MemoryModality::Audio, 2, 0.41),
            (MemoryModality::Visual, 2, 0.41),
            (MemoryModality::Multimodal, 2, 0.43),
        ] {
            close(
                legacy_multimodal_importance(modality, 250, entities),
                expected,
            );
        }
        for (modality, expected) in [
            (MemoryModality::Text, 0.7),
            (MemoryModality::Audio, 0.72),
            (MemoryModality::Visual, 0.72),
            (MemoryModality::Multimodal, 0.74),
        ] {
            close(legacy_multimodal_importance(modality, 500, 10), expected);
            close(
                legacy_multimodal_importance(modality, usize::MAX, usize::MAX),
                expected,
            );
        }
        // The old text add path supplied no entities; its automatic maximum was .5.
        close(
            legacy_multimodal_importance(MemoryModality::Text, 500, 0),
            0.5,
        );
        close(
            legacy_multimodal_importance(MemoryModality::Multimodal, 0, 0),
            0.24,
        );
    }

    #[test]
    fn adjustment_preserves_e_folding_and_full_age_compounding() {
        let created = NOW - THIRTY_DAYS_MS;
        let first = legacy_adjusted_importance(0.8, 0, created, NOW).unwrap();
        close(first, 0.294_303_552_937_153_9);
        let second = legacy_adjusted_importance(first, 0, created, NOW).unwrap();
        close(second, 0.108_268_226_589_290_16);
        close(
            legacy_adjusted_importance(second, 0, created, NOW).unwrap(),
            0.1,
        );
        close(
            legacy_adjusted_importance(0.8, 2, NOW + HOUR_MS, NOW).unwrap(),
            0.96,
        );
        close(
            legacy_adjusted_importance(1.0, u64::MAX, NOW, NOW).unwrap(),
            1.0,
        );
        close(legacy_adjusted_importance(0.0, 0, NOW, NOW).unwrap(), 0.1);
        close(
            legacy_adjusted_importance(0.8, 0, 0, u128::MAX).unwrap(),
            0.1,
        );
    }

    #[test]
    fn cleanup_strict_age_score_and_ninety_percent_boundaries() {
        let mut memories = vec![
            PolicyMemory {
                created_ms: NOW - THIRTY_DAYS_MS,
                importance: 0.29,
            },
            PolicyMemory {
                created_ms: NOW - THIRTY_DAYS_MS - 1,
                importance: 0.29,
            },
            PolicyMemory {
                created_ms: NOW - THIRTY_DAYS_MS - 1,
                importance: 0.30,
            },
            PolicyMemory {
                created_ms: NOW,
                importance: 0.199,
            },
            PolicyMemory {
                created_ms: NOW,
                importance: 0.20,
            },
            PolicyMemory {
                created_ms: NOW,
                importance: 0.5,
            },
            PolicyMemory {
                created_ms: NOW,
                importance: 0.5,
            },
            PolicyMemory {
                created_ms: NOW,
                importance: 0.5,
            },
            PolicyMemory {
                created_ms: NOW + HOUR_MS,
                importance: 0.19,
            },
        ];
        assert_eq!(legacy_cleanup_candidates(&memories, NOW, 10).unwrap(), [1]);
        memories.push(PolicyMemory {
            created_ms: NOW,
            importance: 0.5,
        });
        let original = memories.clone();
        assert_eq!(
            legacy_cleanup_candidates(&memories, NOW, 10).unwrap(),
            [1, 3, 8]
        );
        assert_eq!(memories, original);
    }

    #[test]
    fn cleanup_preview_does_not_promise_capacity_or_stop_at_ninety_percent() {
        let high = vec![
            PolicyMemory {
                created_ms: NOW,
                importance: 0.5
            };
            11
        ];
        assert!(
            legacy_cleanup_candidates(&high, NOW, 10)
                .unwrap()
                .is_empty()
        );
        let low = vec![
            PolicyMemory {
                created_ms: NOW,
                importance: 0.19
            };
            10
        ];
        assert_eq!(
            legacy_cleanup_candidates(&low, NOW, 10).unwrap(),
            (0..10).collect::<Vec<_>>()
        );
        assert!(legacy_cleanup_candidates(&[], NOW, 10).unwrap().is_empty());
        assert!(legacy_cleanup_candidates(&[], NOW, 0).is_err());
    }

    #[test]
    fn explicit_scores_reject_nonfinite_or_out_of_range_values() {
        for importance in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01, 1.01] {
            assert!(legacy_rank_score(importance, NOW, NOW).is_err());
            assert!(legacy_adjusted_importance(importance, 0, NOW, NOW).is_err());
            assert!(
                legacy_cleanup_candidates(
                    &[PolicyMemory {
                        created_ms: NOW,
                        importance
                    }],
                    NOW,
                    10,
                )
                .is_err()
            );
        }
    }
}
