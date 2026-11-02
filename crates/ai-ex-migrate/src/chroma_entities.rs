use ai_ex_domain::{
    ImportedMemoryEntity, MemoryEntity, MemoryEntityKind, MemoryEntitySource,
    MemoryEntitySourceStatus, MemoryEntitySubject,
};
use ring::digest::{Context, SHA256};

use super::*;

fn candidate_id(collection: &str, profile: &str, id: &str) -> Uuid {
    let mut hash = Context::new(&SHA256);
    for part in ["memory entity import v2", collection, profile, id] {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.finish().as_ref()[..16]);
    bytes[6] = (bytes[6] & 15) | 0x80;
    bytes[8] = (bytes[8] & 63) | 0x80;
    Uuid::from_bytes(bytes)
}

fn append_row(output: &mut Vec<u8>, value: Value) -> Result<(), AppError> {
    serde_json::to_writer(&mut *output, &value)
        .map_err(|_| invalid("cannot encode imported entity"))?;
    output.push(b'\n');
    if output.len() > MAX_BYTES {
        return Err(invalid("migrated output exceeds 32 MiB"));
    }
    Ok(())
}

fn subject(metadata: &Map<String, Value>) -> MemoryEntitySubject {
    if let Some(value) = metadata.get("subject")
        && let Ok(subject) = serde_json::from_value::<MemoryEntitySubject>(value.clone())
        && subject.validate().is_ok()
    {
        return subject;
    }
    match metadata.get("subject_kind").and_then(Value::as_str) {
        Some("self_user") => MemoryEntitySubject::SelfUser,
        Some("third_party") => {
            let subject = MemoryEntitySubject::ThirdParty {
                id: metadata
                    .get("subject_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                name: metadata
                    .get("subject_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            };
            if subject.validate().is_ok() {
                subject
            } else {
                MemoryEntitySubject::Unknown
            }
        }
        _ => MemoryEntitySubject::Unknown,
    }
}

fn entity(metadata: &Map<String, Value>) -> Option<MemoryEntity> {
    let name = metadata.get("name")?.as_str()?.to_owned();
    let value = metadata.get("value")?.as_str()?.to_owned();
    let confidence = metadata.get("confidence")?.as_f64()?;
    if name.trim().is_empty() || !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return None;
    }
    let kind = match metadata
        .get("entity_type")?
        .as_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "user_name" | "name" => MemoryEntityKind::Name,
        "preference" => MemoryEntityKind::Preference,
        "fact" => MemoryEntityKind::Fact,
        _ => return None,
    };
    let key = match kind {
        MemoryEntityKind::Name => "user.name".into(),
        MemoryEntityKind::Preference => format!("preference:{}", name.to_lowercase()),
        MemoryEntityKind::Fact => format!("fact:{}", name.to_lowercase()),
    };
    Some(MemoryEntity {
        kind,
        key,
        name,
        value,
        confidence: (confidence * 1000.0).round() as u16,
    })
}

fn timestamp(metadata: &Map<String, Value>, key: &str, offset: Option<i16>) -> Option<u64> {
    let value = metadata.get(key)?.as_str()?;
    crate::legacy_memory::timestamp_ms(value, offset)
        .ok()?
        .try_into()
        .ok()
}

fn references(metadata: &Map<String, Value>) -> Vec<Value> {
    let Some(raw) = metadata.get("related_memories") else {
        return Vec::new();
    };
    match raw {
        Value::Array(values) => values.clone(),
        Value::String(value) => match serde_json::from_str::<Value>(value) {
            Ok(Value::Array(values)) => values,
            _ => vec![raw.clone()],
        },
        _ => vec![raw.clone()],
    }
}

pub(super) fn append(
    output: &mut Vec<u8>,
    collection: CollectionSnapshot,
    profile: &str,
    utc_offset: Option<i16>,
    memory_ids: &BTreeSet<Uuid>,
) -> Result<(usize, usize), AppError> {
    let collection_id = collection
        .descriptor
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("entity collection requires an ID"))?;
    let mut candidates = 0;
    let mut associations = 0;
    for record in collection.records {
        let id = candidate_id(collection_id, profile, &record.id);
        let empty = Map::new();
        let metadata = record.metadata.as_ref().unwrap_or(&empty);
        let candidate = ImportedMemoryEntity {
            id,
            profile_id: profile.into(),
            subject: subject(metadata),
            entity: entity(metadata),
            first_mentioned_ms: timestamp(metadata, "first_mentioned", utc_offset),
            last_updated_ms: timestamp(metadata, "last_updated", utc_offset),
            legacy_record: serde_json::to_value(&record)
                .map_err(|_| invalid("cannot preserve original entity"))?,
        };
        candidate.validate()?;
        append_row(
            output,
            json!({"schema_version":2,"record_type":"entity_candidate","candidate":candidate}),
        )?;
        candidates += 1;
        let mut seen = BTreeSet::new();
        for value in references(metadata) {
            let raw = value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            let memory_id = Uuid::parse_str(&raw).ok();
            let identity = memory_id.map_or_else(|| raw.clone(), |id| id.to_string());
            if !seen.insert(identity) {
                continue;
            }
            let active = memory_id.is_some_and(|id| memory_ids.contains(&id));
            let source = MemoryEntitySource {
                entity_id: id,
                profile_id: profile.into(),
                legacy_memory_id: raw,
                memory_id,
                memory_revision: active.then_some(1),
                status: if active {
                    MemoryEntitySourceStatus::Active
                } else if memory_id.is_some() {
                    MemoryEntitySourceStatus::Missing
                } else {
                    MemoryEntitySourceStatus::Unresolved
                },
            };
            source.validate()?;
            append_row(
                output,
                json!({"schema_version":2,"record_type":"entity_source","source":source}),
            )?;
            associations += 1;
        }
    }
    Ok((candidates, associations))
}
