use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ai_ex_domain::AppError;
use reqwest::{Client, Method, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json, value::RawValue};
use uuid::Uuid;

use crate::legacy_memory::{LegacyMemory, canonical_json, convert_records};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECORDS: usize = 50_000;
const PAGE_SIZE: usize = 128;
const FORMAT: &str = "ai-ex-chroma-snapshot";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    format: String,
    schema_version: u32,
    api_version: String,
    reported_version: String,
    tenant: String,
    database: String,
    main: CollectionSnapshot,
    entities: Option<CollectionSnapshot>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionSnapshot {
    descriptor: Map<String, Value>,
    record_count: usize,
    records: Vec<Record>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    document: Option<String>,
    metadata: Option<Map<String, Value>>,
    embedding: Option<Vec<Value>>,
    uri: Option<String>,
}

#[derive(Deserialize)]
struct GetRecords {
    ids: Vec<String>,
    documents: Option<Vec<Option<String>>>,
    metadatas: Option<Vec<Option<Map<String, Value>>>>,
    embeddings: Option<Vec<Vec<Value>>>,
    uris: Option<Vec<Option<String>>>,
}

struct ExportOptions {
    url: Url,
    collection: String,
    tenant: String,
    database: String,
    entity_collection: Option<String>,
    output: PathBuf,
}

impl ExportOptions {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, AppError> {
        let mut values = std::collections::BTreeMap::new();
        let mut arguments = arguments;
        while let Some(flag) = arguments.next() {
            if !matches!(
                flag.as_str(),
                "--url"
                    | "--collection"
                    | "--tenant"
                    | "--database"
                    | "--entity-collection"
                    | "--output"
            ) {
                return Err(invalid("unknown Chroma export option"));
            }
            let value = arguments
                .next()
                .ok_or_else(|| invalid("Chroma export option requires a value"))?;
            if values.insert(flag, value).is_some() {
                return Err(invalid("duplicate Chroma export option"));
            }
        }
        let mut required = |flag: &str| {
            values
                .remove(flag)
                .ok_or_else(|| invalid(format!("--chroma-export requires {flag}")))
        };
        let url = required("--url")?;
        let collection = required("--collection")?;
        let tenant = required("--tenant")?;
        let database = required("--database")?;
        let output = PathBuf::from(required("--output")?);
        let entity_collection = values.remove("--entity-collection");
        let url = Url::parse(&url).map_err(|_| invalid("invalid Chroma URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(
                "Chroma URL must use HTTP(S) without credentials, query, or fragment",
            ));
        }
        for name in [&collection, &tenant, &database]
            .into_iter()
            .chain(entity_collection.iter())
        {
            validate_name(name)?;
        }
        if entity_collection.as_ref() == Some(&collection) {
            return Err(invalid("main and entity collections must differ"));
        }
        if output.as_os_str().is_empty() {
            return Err(invalid("output path must not be empty"));
        }
        Ok(Self {
            url,
            collection,
            tenant,
            database,
            entity_collection,
            output,
        })
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::configuration(message)
}

fn validate_name(value: &str) -> Result<(), AppError> {
    if value.trim().is_empty()
        || value.len() > 512
        || matches!(value, "." | "..")
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err(invalid(
            "invalid Chroma tenant, database, collection, or record identifier",
        ));
    }
    Ok(())
}

struct Bridge {
    client: Client,
    root: Url,
    tenant: String,
    database: String,
}

impl Bridge {
    fn endpoint(&self, segments: &[&str]) -> Result<Url, AppError> {
        let mut url = self.root.clone();
        {
            let mut path = url
                .path_segments_mut()
                .map_err(|_| invalid("invalid Chroma base URL"))?;
            path.pop_if_empty().push("api").push("v2");
            for segment in segments {
                path.push(segment);
            }
        }
        Ok(url)
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        segments: &[&str],
        body: Option<Value>,
    ) -> Result<T, AppError> {
        let mut request = self.client.request(method, self.endpoint(segments)?);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| AppError::unavailable("Chroma API request failed"))?;
        if !response.status().is_success() {
            return Err(AppError::unavailable(format!(
                "Chroma API returned HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|bytes| bytes > MAX_BYTES as u64)
        {
            return Err(invalid("Chroma response exceeds 32 MiB"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| AppError::unavailable("cannot read Chroma response"))?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_BYTES {
                return Err(invalid("Chroma response exceeds 32 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        checked_json(&bytes)?;
        serde_json::from_slice(&bytes).map_err(|_| invalid("unsupported Chroma v2 response shape"))
    }

    async fn count(&self, id: &str) -> Result<usize, AppError> {
        self.request(
            Method::GET,
            &[
                "tenants",
                &self.tenant,
                "databases",
                &self.database,
                "collections",
                id,
                "count",
            ],
            None,
        )
        .await
    }

    async fn collection(
        &self,
        name: &str,
        remaining: usize,
        bytes: &mut usize,
    ) -> Result<CollectionSnapshot, AppError> {
        let descriptor: Map<String, Value> = self
            .request(
                Method::GET,
                &[
                    "tenants",
                    &self.tenant,
                    "databases",
                    &self.database,
                    "collections",
                    name,
                ],
                None,
            )
            .await?;
        validate_descriptor(&descriptor, &self.tenant, &self.database)?;
        if descriptor.get("name").and_then(Value::as_str) != Some(name) {
            return Err(invalid("Chroma returned a different collection name"));
        }
        account_bytes(bytes, &descriptor)?;
        let id = descriptor
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing Chroma collection ID"))?;
        let count = self.count(id).await?;
        if count > remaining {
            return Err(invalid("Chroma snapshot exceeds 50000 records"));
        }
        let mut records = Vec::with_capacity(count);
        let mut ids = BTreeSet::new();
        while records.len() < count {
            let page: GetRecords = self.request(Method::POST,
                &["tenants", &self.tenant, "databases", &self.database, "collections", id, "get"],
                Some(json!({"limit": PAGE_SIZE, "offset": records.len(), "include": ["documents", "metadatas", "embeddings", "uris"]}))).await?;
            let length = page.ids.len();
            if length == 0 || length > PAGE_SIZE || length > count - records.len() {
                return Err(invalid("Chroma record count changed during export"));
            }
            let documents = page
                .documents
                .ok_or_else(|| invalid("Chroma omitted documents"))?;
            let metadatas = page
                .metadatas
                .ok_or_else(|| invalid("Chroma omitted metadatas"))?;
            let embeddings = page
                .embeddings
                .ok_or_else(|| invalid("Chroma omitted embeddings"))?;
            let uris = page.uris.ok_or_else(|| invalid("Chroma omitted uris"))?;
            if documents.len() != length
                || metadatas.len() != length
                || embeddings.len() != length
                || uris.len() != length
            {
                return Err(invalid("Chroma response columns have different lengths"));
            }
            for ((((id, document), metadata), embedding), uri) in page
                .ids
                .into_iter()
                .zip(documents)
                .zip(metadatas)
                .zip(embeddings)
                .zip(uris)
            {
                if !ids.insert(id.clone()) {
                    return Err(invalid("duplicate Chroma record ID across export pages"));
                }
                let record = Record {
                    id,
                    document,
                    metadata,
                    embedding: Some(embedding),
                    uri,
                };
                validate_record(&record)?;
                account_bytes(bytes, &record)?;
                records.push(record);
            }
        }
        if self.count(id).await? != count {
            return Err(invalid("Chroma record count changed during export"));
        }
        Ok(CollectionSnapshot {
            descriptor,
            record_count: count,
            records,
        })
    }
}

fn account_bytes(total: &mut usize, value: &impl Serialize) -> Result<(), AppError> {
    let encoded =
        serde_json::to_vec(value).map_err(|_| invalid("cannot encode Chroma snapshot"))?;
    *total = total.saturating_add(encoded.len());
    if *total > MAX_BYTES {
        return Err(invalid("Chroma snapshot exceeds 32 MiB"));
    }
    Ok(())
}

fn checked_json(content: &[u8]) -> Result<(), AppError> {
    if content.len() > MAX_BYTES {
        return Err(invalid("Chroma snapshot exceeds 32 MiB"));
    }
    let root: Box<RawValue> =
        serde_json::from_slice(content).map_err(|_| invalid("invalid Chroma JSON"))?;
    // Reuse bounded, duplicate-key-rejecting validation from the offline importer.
    canonical_json(&root, &mut String::new(), 0)
}

fn validate_descriptor(
    descriptor: &Map<String, Value>,
    tenant: &str,
    database: &str,
) -> Result<(), AppError> {
    let id = descriptor
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing Chroma collection ID"))?;
    Uuid::parse_str(id).map_err(|_| invalid("invalid Chroma collection UUID"))?;
    let name = descriptor
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing Chroma collection name"))?;
    validate_name(name)?;
    if descriptor.get("tenant").and_then(Value::as_str) != Some(tenant)
        || descriptor.get("database").and_then(Value::as_str) != Some(database)
    {
        return Err(invalid(
            "Chroma collection tenant or database does not match",
        ));
    }
    Ok(())
}

fn validate_reported_version(version: &str) -> Result<(), AppError> {
    if version.trim().is_empty() || version.len() > 128 || version.chars().any(char::is_control) {
        return Err(invalid("unsupported Chroma version response"));
    }
    Ok(())
}

fn validate_distinct_collections(
    main: &CollectionSnapshot,
    entities: Option<&CollectionSnapshot>,
) -> Result<(), AppError> {
    let Some(entities) = entities else {
        return Ok(());
    };
    let id = |collection: &CollectionSnapshot| {
        collection
            .descriptor
            .get("id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| invalid("invalid Chroma collection UUID"))
    };
    if main.descriptor.get("name") == entities.descriptor.get("name") || id(main)? == id(entities)?
    {
        return Err(invalid(
            "main and entity collection descriptors must be distinct",
        ));
    }
    Ok(())
}
fn validate_record(record: &Record) -> Result<(), AppError> {
    if record.id.is_empty() || record.id.len() > 4096 || record.id.chars().any(char::is_control) {
        return Err(invalid("invalid Chroma record identifier"));
    }
    if let Some(embedding) = &record.embedding
        && (embedding.len() > 8192
            || embedding.iter().any(|value| {
                value
                    .as_f64()
                    .is_none_or(|number| !(number as f32).is_finite())
            }))
    {
        return Err(invalid(
            "Chroma embedding must contain at most 8192 finite f32 values",
        ));
    }
    Ok(())
}

pub async fn export(arguments: impl Iterator<Item = String>) -> Result<(), AppError> {
    let options = ExportOptions::parse(arguments)?;
    if tokio::fs::try_exists(&options.output)
        .await
        .map_err(|_| invalid("cannot inspect Chroma output path"))?
    {
        return Err(invalid("refusing to overwrite existing output"));
    }
    let bridge = Bridge {
        client: Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| AppError::unavailable("cannot create Chroma HTTP client"))?,
        root: options.url,
        tenant: options.tenant.clone(),
        database: options.database.clone(),
    };
    let reported_version: String = bridge.request(Method::GET, &["version"], None).await?;
    validate_reported_version(&reported_version)?;
    // Chroma's 1.4.0 Rust server reports "1.0.0": this is provenance, not a compatibility gate.
    let mut bytes = 0;
    let main = bridge
        .collection(&options.collection, MAX_RECORDS, &mut bytes)
        .await?;
    let entities = match options.entity_collection {
        Some(name) => Some(
            bridge
                .collection(&name, MAX_RECORDS - main.record_count, &mut bytes)
                .await?,
        ),
        None => None,
    };
    validate_distinct_collections(&main, entities.as_ref())?;
    let snapshot = Snapshot {
        format: FORMAT.to_owned(),
        schema_version: 1,
        api_version: "v2".to_owned(),
        reported_version,
        tenant: options.tenant,
        database: options.database,
        main,
        entities,
    };
    let content =
        serde_json::to_vec(&snapshot).map_err(|_| invalid("cannot encode Chroma snapshot"))?;
    if content.len() > MAX_BYTES {
        return Err(invalid("Chroma snapshot exceeds 32 MiB"));
    }
    crate::write_new_atomic(&options.output, &content).await?;
    println!(
        "Chroma v2 snapshot written to {} ({} main records); reported server version: {}. No models or queries were invoked.",
        options.output.display(),
        snapshot.main.record_count,
        snapshot.reported_version
    );
    report_entities(snapshot.entities.is_some());
    println!(
        "Use a quiescent source copy: count checks cannot detect concurrent edits that leave the count unchanged."
    );
    Ok(())
}

fn report_entities(present: bool) {
    if present {
        println!("Entity collection: archived only (not restored by --chroma-snapshot)");
    } else {
        println!("Entity collection: not exported");
    }
}

pub fn import_snapshot(
    path: &Path,
    profile: &str,
    utc_offset: Option<i16>,
    embedding_space: Option<&str>,
) -> Result<Vec<u8>, AppError> {
    let file = std::fs::File::open(path).map_err(|_| invalid("cannot open Chroma snapshot"))?;
    let mut content = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut content)
        .map_err(|_| invalid("cannot read Chroma snapshot"))?;
    checked_json(&content)?;
    let snapshot: Snapshot = serde_json::from_slice(&content)
        .map_err(|_| invalid("unsupported Chroma snapshot shape"))?;
    if snapshot.format != FORMAT || snapshot.schema_version != 1 || snapshot.api_version != "v2" {
        return Err(invalid("unsupported Chroma snapshot format or version"));
    }
    validate_reported_version(&snapshot.reported_version)?;
    validate_name(&snapshot.tenant)?;
    validate_name(&snapshot.database)?;
    let total = snapshot.main.record_count.saturating_add(
        snapshot
            .entities
            .as_ref()
            .map_or(0, |collection| collection.record_count),
    );
    if total > MAX_RECORDS {
        return Err(invalid("Chroma snapshot exceeds 50000 records"));
    }
    for collection in std::iter::once(&snapshot.main).chain(snapshot.entities.iter()) {
        validate_descriptor(&collection.descriptor, &snapshot.tenant, &snapshot.database)?;
        if collection.record_count != collection.records.len() {
            return Err(invalid("Chroma snapshot record count does not match"));
        }
        let mut ids = BTreeSet::new();
        for record in &collection.records {
            validate_record(record)?;
            if !ids.insert(&record.id) {
                return Err(invalid("duplicate Chroma snapshot record ID"));
            }
        }
    }
    validate_distinct_collections(&snapshot.main, snapshot.entities.as_ref())?;
    let records = snapshot.main.records.into_iter().map(|record| {
        if record.uri.is_some() {
            return Err(invalid("main Chroma records with media URIs are unsupported; the snapshot retains the original URI"));
        }
        Ok(LegacyMemory {
            id: Uuid::parse_str(&record.id).map_err(|_| invalid("legacy main memory IDs must be UUIDs"))?,
            content: record.document.ok_or_else(|| invalid("main Chroma record requires a document"))?,
            metadata: record.metadata.ok_or_else(|| invalid("main Chroma record requires metadata"))?,
            embedding: record.embedding.map(Value::Array),
        })
    }).collect::<Result<Vec<_>, AppError>>()?;
    let converted = convert_records(records, profile, utc_offset, embedding_space)?;
    report_entities(snapshot.entities.is_some());
    Ok(converted)
}
