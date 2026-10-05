//! Imports explicit JSON dataclass snapshots. The old module had no disk export format.
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

use ai_ex_domain::{
    AppError, MemoryEntry, MemoryKind, MemoryKnowledge, MemoryMedia, MemoryModality, MemorySource,
    TurnId,
};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, value::RawValue};
use uuid::Uuid;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 50_000;

#[derive(Deserialize)]
struct LegacySnapshot {
    memory_id: String,
    modality: MemoryModality,
    #[serde(default)]
    text_content: Option<String>,
    #[serde(default)]
    audio_features: Option<Map<String, Value>>,
    #[serde(default)]
    visual_features: Option<Map<String, Value>>,
    timestamp: Box<RawValue>,
    #[serde(default = "default_importance")]
    importance_score: f64,
    #[serde(default, rename = "access_count")]
    _access_count: u64,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    metadata: Map<String, Value>,
}

fn default_importance() -> f64 {
    0.5
}

#[derive(Serialize)]
struct ImportedSnapshot {
    #[serde(flatten)]
    entry: MemoryEntry,
    knowledge: MemoryKnowledge,
    // Keep the full compact snapshot, including the original ID, access count,
    // importance float and timestamp. No reserved key is injected into user metadata.
    legacy_metadata: Box<RawValue>,
}

pub fn import_snapshot(path: &Path, profile: &str) -> Result<Vec<u8>, AppError> {
    let file = std::fs::File::open(path).map_err(|error| {
        AppError::unavailable(format!("cannot open multimodal snapshot: {error}"))
    })?;
    let mut content = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut content)
        .map_err(|_| AppError::configuration("cannot read multimodal snapshot"))?;
    convert_snapshot(&content, profile)
}

fn convert_snapshot(content: &[u8], profile: &str) -> Result<Vec<u8>, AppError> {
    if profile.trim().is_empty() || profile.chars().count() > 128 {
        return Err(AppError::configuration(
            "memory profile ID is outside supported bounds",
        ));
    }
    if content.len() > MAX_BYTES {
        return Err(AppError::configuration(
            "multimodal snapshot exceeds 32 MiB",
        ));
    }
    let root: Box<RawValue> = serde_json::from_slice(content)
        .map_err(|_| AppError::configuration("invalid multimodal snapshot JSON"))?;
    let mut compact = String::new();
    // Shared validator rejects duplicate JSON keys, excessive nesting and array counts.
    // Compact raw objects also keep source whitespace from breaking the JSONL output.
    crate::legacy_memory::canonical_json(&root, &mut compact, 0)?;
    let snapshots: Vec<Box<RawValue>> = serde_json::from_str(&compact)
        .map_err(|_| AppError::configuration("multimodal snapshot must be a JSON array"))?;
    if snapshots.len() > MAX_RECORDS {
        return Err(AppError::configuration(
            "multimodal snapshot exceeds 50000 records",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut output = Vec::new();
    for (index, snapshot) in snapshots.into_iter().enumerate() {
        let fail = |reason: &str| {
            AppError::configuration(format!("multimodal record {}: {reason}", index + 1))
        };
        let record: LegacySnapshot = serde_json::from_str(snapshot.get())
            .map_err(|_| fail("invalid or missing dataclass fields"))?;
        let id = import_id(&record.memory_id).map_err(|_| fail("invalid memory_id"))?;
        if !ids.insert(id) {
            return Err(fail("duplicate memory ID"));
        }
        let created_ms = epoch_millis(&record.timestamp).map_err(|_| {
            fail("timestamp must be nonnegative finite epoch seconds within u64 milliseconds")
        })?;
        if !record.importance_score.is_finite() || !(0.0..=1.0).contains(&record.importance_score) {
            return Err(fail("importance_score must be between 0 and 1"));
        }
        let user_text = match record.text_content {
            Some(text) if !text.trim().is_empty() => text,
            _ => match record.modality {
                MemoryModality::Text => return Err(fail("text memories require text_content")),
                MemoryModality::Audio => "Audio observation".to_owned(),
                MemoryModality::Visual => "Visual observation".to_owned(),
                MemoryModality::Multimodal => "Multimodal observation".to_owned(),
            },
        };
        if user_text.chars().count() > 4096 {
            return Err(fail("text_content exceeds 4096 characters"));
        }
        let media = MemoryMedia {
            modality: record.modality,
            tags: record.tags,
            audio_features: record.audio_features,
            visual_features: record.visual_features,
            metadata: record.metadata,
            importance: (record.importance_score * 1000.0).round() as u16,
        };
        media.validate().map_err(|error| fail(&error.message))?;
        let imported = ImportedSnapshot {
            entry: MemoryEntry {
                id,
                profile_id: profile.to_owned(),
                turn_id: TurnId(id),
                created_ms: u128::from(created_ms),
                updated_ms: None,
                kind: MemoryKind::Conversation,
                user_text,
                assistant_text: String::new(),
                source: MemorySource::Automatic,
                revision: 1,
            },
            knowledge: MemoryKnowledge {
                entities: Vec::new(),
                media: Some(media),
            },
            legacy_metadata: snapshot,
        };
        serde_json::to_writer(&mut output, &imported)
            .map_err(|_| AppError::protocol("cannot encode imported multimodal memory"))?;
        output.push(b'\n');
        if output.len() > MAX_BYTES {
            return Err(AppError::configuration(
                "multimodal JSONL output exceeds 32 MiB",
            ));
        }
    }
    Ok(output)
}

fn import_id(value: &str) -> Result<Uuid, ()> {
    if let Ok(id) = Uuid::parse_str(value) {
        return Ok(id);
    }
    let (random, seconds) = value
        .strip_prefix("mm_")
        .and_then(|value| value.split_once('_'))
        .ok_or(())?;
    if random.len() != 16
        || !random.bytes().all(|byte| byte.is_ascii_hexdigit())
        || seconds.is_empty()
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || seconds.parse::<u64>().is_err()
    {
        return Err(());
    }
    let mut hash = Context::new(&SHA256);
    hash.update(b"AIex legacy multimodal memory v1\0");
    hash.update(value.as_bytes());
    let digest = hash.finish();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_ref()[..16]);
    // A stable, domain-separated UUIDv8 mapping; the old string remains in the snapshot.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Uuid::from_bytes(bytes))
}

/// Convert decimal epoch seconds by truncating sub-millisecond digits exactly.
/// Floating point multiplication would turn inputs such as 1.001 into 1000 ms.
fn epoch_millis(value: &RawValue) -> Result<u64, ()> {
    let raw = value.get();
    if raw.len() > 128
        || raw.starts_with('-')
        || serde_json::from_str::<serde_json::Number>(raw).is_err()
    {
        return Err(());
    }
    let (mantissa, exponent) = match raw.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().map_err(|_| ())?),
        None => (raw, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Ok(0);
    }
    let scale = exponent
        .checked_add(3)
        .and_then(|value| value.checked_sub(fraction.len() as i32))
        .ok_or(())?;
    if scale < 0 {
        let drop = scale.unsigned_abs() as usize;
        if drop >= digits.len() {
            return Ok(0);
        }
        return digits[..digits.len() - drop].parse::<u64>().map_err(|_| ());
    }
    if digits.len().saturating_add(scale as usize) > 20 {
        return Err(());
    }
    digits
        .parse::<u64>()
        .map_err(|_| ())?
        .checked_mul(10_u64.checked_pow(scale as u32).ok_or(())?)
        .ok_or(())
}

#[cfg(test)]
#[path = "multimodal_memory_tests.rs"]
mod tests;
