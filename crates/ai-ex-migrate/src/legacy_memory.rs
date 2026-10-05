use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use ai_ex_domain::{AppError, MemoryEntry, MemoryKind, MemorySource, TurnId};
use flate2::read::MultiGzDecoder;
use ring::digest::{SHA256, digest};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const MAX_BACKUP_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 50_000;
const MAX_RECORD_TEXT_BYTES: usize = 128 * 1024;

#[derive(Deserialize)]
struct Backup {
    schema_version: String,
    total_memories: usize,
    backup_integrity_hash: String,
    memories: Box<RawValue>,
}

#[derive(Deserialize)]
struct LegacyMemory {
    id: Uuid,
    content: String,
    metadata: Value,
}

pub fn import_backup(
    path: &Path,
    profile: &str,
    utc_offset: Option<i16>,
) -> Result<Vec<u8>, AppError> {
    if profile.trim().is_empty() || profile.chars().count() > 128 {
        return Err(AppError::configuration(
            "memory profile ID is outside supported bounds",
        ));
    }
    if utc_offset.is_some_and(|offset| !(-1439..=1439).contains(&offset)) {
        return Err(AppError::configuration(
            "UTC offset must be between -1439 and 1439 minutes",
        ));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| AppError::unavailable(format!("cannot open backup: {error}")))?;
    if file
        .metadata()
        .map_err(|error| AppError::unavailable(error.to_string()))?
        .len()
        > MAX_BACKUP_BYTES
    {
        return Err(AppError::configuration(
            "compressed memory backup exceeds 32 MiB",
        ));
    }
    let mut header = [0; 2];
    let mut reader = std::io::BufReader::new(file);
    use std::io::{Seek, SeekFrom};
    reader
        .read_exact(&mut header)
        .map_err(|_| AppError::configuration("memory backup is empty or truncated"))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| AppError::unavailable(error.to_string()))?;
    let content = if header == [0x1f, 0x8b] {
        read_bounded(MultiGzDecoder::new(reader), MAX_BACKUP_BYTES)?
    } else {
        read_bounded(reader, MAX_BACKUP_BYTES)?
    };
    convert_backup(&content, profile, utc_offset)
}

fn read_bounded(reader: impl Read, limit: u64) -> Result<Vec<u8>, AppError> {
    let mut content = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut content)
        .map_err(|_| AppError::configuration("cannot decode memory backup"))?;
    if content.len() as u64 > limit {
        return Err(AppError::configuration(
            "decompressed memory backup exceeds its byte budget",
        ));
    }
    Ok(content)
}

fn convert_backup(
    content: &[u8],
    profile: &str,
    utc_offset: Option<i16>,
) -> Result<Vec<u8>, AppError> {
    let root: Box<RawValue> = serde_json::from_slice(content)
        .map_err(|_| AppError::configuration("invalid legacy memory backup JSON"))?;
    let mut checked = String::new();
    canonical_json(&root, &mut checked, 0)?;
    drop(checked);
    let backup: Backup = serde_json::from_slice(content)
        .map_err(|_| AppError::configuration("invalid legacy memory backup JSON"))?;
    if backup.schema_version != "1.0" || backup.total_memories > MAX_RECORDS {
        return Err(AppError::configuration(
            "unsupported memory backup schema or record count",
        ));
    }
    // Python's backup writer sorts object keys, uses compact separators and
    // ensure_ascii=True for the hash. Keep numeric lexemes from the source:
    // reserializing floats can change exponent spelling and break old hashes.
    let mut canonical = String::new();
    canonical_json(&backup.memories, &mut canonical, 0)?;
    let hash = digest(&SHA256, canonical.as_bytes());
    let expected = hash
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if expected != backup.backup_integrity_hash {
        return Err(AppError::configuration(
            "memory backup integrity check failed",
        ));
    }
    let memories: Vec<LegacyMemory> = serde_json::from_str(backup.memories.get())
        .map_err(|_| AppError::configuration("invalid legacy memory record"))?;
    if memories.len() != backup.total_memories {
        return Err(AppError::configuration(
            "memory backup record count does not match",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut output = Vec::new();
    for (index, memory) in memories.into_iter().enumerate() {
        let fail =
            |reason| AppError::configuration(format!("memory record {}: {reason}", index + 1));
        if !ids.insert(memory.id) {
            return Err(fail("duplicate ID"));
        }
        let timestamp = memory
            .metadata
            .get("timestamp")
            .and_then(Value::as_str)
            .ok_or_else(|| fail("missing timestamp"))?;
        let created_ms = timestamp_ms(timestamp, utc_offset)
            .map_err(|_| fail("invalid timestamp or missing UTC offset for local time"))?;
        let kind = match memory.metadata.get("memory_type").and_then(Value::as_str) {
            Some("INTERACTION" | "SUMMARY") => MemoryKind::Conversation,
            Some("EVENT") => MemoryKind::LiveEvent,
            _ => return Err(fail("unsupported memory type")),
        };
        let (user_text, assistant_text) = match (
            memory.metadata.get("user_input").and_then(Value::as_str),
            memory.metadata.get("ai_response").and_then(Value::as_str),
        ) {
            (Some(user), Some(assistant)) => (user.to_owned(), assistant.to_owned()),
            _ => (memory.content.clone(), String::new()),
        };
        if user_text.len().saturating_add(assistant_text.len()) > MAX_RECORD_TEXT_BYTES {
            return Err(fail("text exceeds 128 KiB"));
        }
        let entry = MemoryEntry {
            id: memory.id,
            profile_id: profile.to_owned(),
            // Stable import identity; a backup cannot prove the old turn ID.
            turn_id: TurnId(memory.id),
            created_ms,
            updated_ms: None,
            kind,
            user_text,
            assistant_text,
            source: MemorySource::Automatic,
            revision: 1,
        };
        serde_json::to_writer(&mut output, &entry)
            .map_err(|_| AppError::protocol("cannot encode migrated memory"))?;
        output.push(b'\n');
        if output.len() as u64 > MAX_BACKUP_BYTES {
            return Err(fail("migrated output exceeds 32 MiB"));
        }
    }
    Ok(output)
}

fn timestamp_ms(value: &str, utc_offset: Option<i16>) -> Result<u128, AppError> {
    let timestamp = match OffsetDateTime::parse(value, &Rfc3339) {
        Ok(timestamp) => timestamp,
        Err(_) => {
            let offset = utc_offset.ok_or_else(|| {
                AppError::configuration("local timestamp requires an explicit UTC offset")
            })?;
            let sign = if offset < 0 { '-' } else { '+' };
            let minutes = offset.unsigned_abs();
            OffsetDateTime::parse(
                &format!("{value}{sign}{:02}:{:02}", minutes / 60, minutes % 60),
                &Rfc3339,
            )
            .map_err(|_| AppError::configuration("invalid legacy timestamp"))?
        }
    };
    let nanos = u128::try_from(timestamp.unix_timestamp_nanos())
        .map_err(|_| AppError::configuration("timestamps before the Unix epoch are unsupported"))?;
    Ok(nanos / 1_000_000)
}
fn canonical_json(value: &RawValue, output: &mut String, depth: usize) -> Result<(), AppError> {
    if depth > 64 {
        return Err(AppError::configuration(
            "memory backup nesting exceeds 64 levels",
        ));
    }
    let raw = value.get().trim();
    let invalid = |_| AppError::configuration("invalid memory backup value");
    match raw.as_bytes().first() {
        Some(b'{') => {
            let UniqueObject(fields) = serde_json::from_str(raw).map_err(invalid)?;
            output.push('{');
            for (index, (key, value)) in fields.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                ascii_json_string(key, output)?;
                output.push(':');
                canonical_json(value, output, depth + 1)?;
            }
            output.push('}');
        }
        Some(b'[') => {
            let BoundedArray(fields) = serde_json::from_str(raw).map_err(invalid)?;
            output.push('[');
            for (index, value) in fields.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                canonical_json(value, output, depth + 1)?;
            }
            output.push(']');
        }
        Some(b'"') => {
            let text: String = serde_json::from_str(raw).map_err(invalid)?;
            ascii_json_string(&text, output)?;
        }
        _ => output.push_str(raw),
    }
    Ok(())
}

fn ascii_json_string(value: &str, output: &mut String) -> Result<(), AppError> {
    let encoded = serde_json::to_string(value)
        .map_err(|_| AppError::protocol("cannot encode backup string"))?;
    for character in encoded.chars() {
        if character >= '\u{7f}' {
            for unit in character.encode_utf16(&mut [0; 2]) {
                use std::fmt::Write;
                write!(output, "\\u{unit:04x}")
                    .map_err(|_| AppError::protocol("cannot encode backup string"))?;
            }
        } else {
            output.push(character);
        }
    }
    Ok(())
}

struct UniqueObject(BTreeMap<String, Box<RawValue>>);

impl<'de> Deserialize<'de> for UniqueObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = UniqueObject;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    if fields.len() >= MAX_RECORDS || fields.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom(
                            "duplicate key or object field budget exceeded",
                        ));
                    }
                }
                Ok(UniqueObject(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

struct BoundedArray(Vec<Box<RawValue>>);

impl<'de> Deserialize<'de> for BoundedArray {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ArrayVisitor;
        impl<'de> Visitor<'de> for ArrayVisitor {
            type Value = BoundedArray;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array within the element budget")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element()? {
                    if values.len() >= MAX_RECORDS {
                        return Err(serde::de::Error::custom("array element budget exceeded"));
                    }
                    values.push(value);
                }
                Ok(BoundedArray(values))
            }
        }
        deserializer.deserialize_seq(ArrayVisitor)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(memories: Value) -> Vec<u8> {
        let raw = RawValue::from_string(memories.to_string()).unwrap();
        let mut canonical = String::new();
        canonical_json(&raw, &mut canonical, 0).unwrap();
        let hash = digest(&SHA256, canonical.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        serde_json::to_vec(&json!({"schema_version":"1.0", "total_memories":memories.as_array().unwrap().len(), "memories":memories,"backup_integrity_hash":hash})).unwrap()
    }

    fn memory() -> Value {
        json!({"id":"00000000-0000-4000-8000-000000000001", "content":"User: 茶\nAI: 好", "metadata":{"timestamp":"2026-10-05T12:00:00.123456","memory_type":"INTERACTION","user_input":"茶","ai_response":"好"},"embedding":[0.0,1e-05]})
    }

    #[test]
    fn canonical_form_matches_the_legacy_hash_contract() {
        let raw =
            RawValue::from_string(r#"[{"z":1e-05,"emoji":"😀","text":"茶\n","a":0.0}]"#.to_owned())
                .unwrap();
        let mut actual = String::new();
        canonical_json(&raw, &mut actual, 0).unwrap();
        assert_eq!(
            actual,
            r#"[{"a":0.0,"emoji":"\ud83d\ude00","text":"\u8336\n","z":1e-05}]"#
        );
        // Frozen SHA-256 of the Python-compatible compact ASCII representation.
        let empty: Backup = serde_json::from_slice(br#"{"schema_version":"1.0","total_memories":0,"memories":[],"backup_integrity_hash":"4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"}"#).unwrap();
        assert!(convert_backup(&serde_json::to_vec(&json!({"schema_version":empty.schema_version,"total_memories":0,"memories":[],"backup_integrity_hash":empty.backup_integrity_hash})).unwrap(), "default", None).unwrap().is_empty());
    }

    #[test]
    fn preserves_identity_text_and_explicit_local_time_without_confirming_facts() {
        let source = fixture(json!([memory()]));
        assert!(convert_backup(&source, "character-a", None).is_err());
        let output = convert_backup(&source, "character-a", Some(480)).unwrap();
        let entry: MemoryEntry = serde_json::from_slice(&output).unwrap();
        assert_eq!(entry.profile_id, "character-a");
        assert_eq!(entry.user_text, "茶");
        assert_eq!(entry.assistant_text, "好");
        assert_eq!(entry.source, MemorySource::Automatic);
        assert_eq!(entry.created_ms, 1_791_172_800_123);
        assert_eq!(
            output,
            convert_backup(&source, "character-a", Some(480)).unwrap()
        );
    }

    #[test]
    fn rejects_corruption_duplicates_unknown_types_and_mismatched_counts() {
        let source = fixture(json!([memory()]));
        let mut corrupted: Value = serde_json::from_slice(&source).unwrap();
        corrupted["memories"][0]["content"] = json!("changed");
        assert!(
            convert_backup(&serde_json::to_vec(&corrupted).unwrap(), "default", Some(0)).is_err()
        );
        assert!(convert_backup(&fixture(json!([memory(), memory()])), "default", Some(0)).is_err());
        let mut unsupported = memory();
        unsupported["metadata"]["memory_type"] = json!("UNKNOWN");
        assert!(convert_backup(&fixture(json!([unsupported])), "default", Some(0)).is_err());
        let mut count: Value = serde_json::from_slice(&source).unwrap();
        count["total_memories"] = json!(2);
        assert!(convert_backup(&serde_json::to_vec(&count).unwrap(), "default", Some(0)).is_err());
    }

    #[test]
    fn gzip_decoding_stops_at_the_decompressed_budget() {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&vec![b'x'; 2048]).unwrap();
        let encoded = encoder.finish().unwrap();
        assert!(read_bounded(MultiGzDecoder::new(encoded.as_slice()), 1024).is_err());
        assert_eq!(
            read_bounded(MultiGzDecoder::new(encoded.as_slice()), 2048)
                .unwrap()
                .len(),
            2048
        );
    }

    #[test]
    fn timestamp_offsets_and_precision_are_explicit() {
        assert_eq!(timestamp_ms("1970-01-01T00:00:00Z", None).unwrap(), 0);
        assert_eq!(
            timestamp_ms("1970-01-01T08:00:00.123456", Some(480)).unwrap(),
            123
        );
        assert_eq!(
            timestamp_ms("1970-01-01T00:00:00-01:00", None).unwrap(),
            3_600_000
        );
        assert!(timestamp_ms("1969-01-01T00:00:00Z", None).is_err());
        assert!(timestamp_ms("invalid", Some(0)).is_err());
    }
}

#[cfg(test)]
mod hostile_input_tests {
    use super::*;

    #[test]
    fn rejects_duplicate_keys_at_all_levels_and_excessive_nesting() {
        for source in [
            r#"{"memories":[],"memories":[]}"#,
            r#"{"memories":[{"metadata":{"x":1,"x":2}}]}"#,
        ] {
            assert!(convert_backup(source.as_bytes(), "default", None).is_err());
        }
        let nested = format!("{}0{}", "[".repeat(66), "]".repeat(66));
        let raw = RawValue::from_string(nested).unwrap();
        assert!(canonical_json(&raw, &mut String::new(), 0).is_err());
        let raw =
            RawValue::from_string(format!("[{}]", vec!["0"; MAX_RECORDS + 1].join(","))).unwrap();
        assert!(canonical_json(&raw, &mut String::new(), 0).is_err());
    }

    #[test]
    fn rejects_broken_and_truncated_gzip() {
        use std::io::Write;
        assert!(read_bounded(MultiGzDecoder::new(&[0x1f, 0x8b, 0][..]), 1024).is_err());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"[]").unwrap();
        let mut encoded = encoder.finish().unwrap();
        encoded.pop();
        assert!(read_bounded(MultiGzDecoder::new(encoded.as_slice()), 1024).is_err());
        assert!(timestamp_ms("1969-12-31T23:59:59.999999Z", None).is_err());
    }
}
