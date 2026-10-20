#[path = "support/chroma_mock.rs"]
mod chroma_mock;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use ai_ex_domain::MemoryKind;
use ai_ex_memory::MemoryStore;
use chroma_mock::{Mock, Step};
use serde_json::{Value, json};
use uuid::Uuid;

const TENANT: &str = "default_tenant";
const DATABASE: &str = "default_database";
const MAIN: &str = "legacy_memories";
const ENTITIES: &str = "legacy_entities";
const SPACE: &str = "synthetic-three-dimensional-chroma-v1";

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("aiex-chroma-protocol-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn snapshot(&self) -> PathBuf {
        self.0.join("snapshot.json")
    }

    fn output(&self) -> PathBuf {
        self.0.join("memory.jsonl")
    }

    fn export(&self, url: &str, entities: bool) -> Output {
        let mut command = self.command();
        command.args([
            "--chroma-export",
            "--url",
            url,
            "--collection",
            MAIN,
            "--tenant",
            TENANT,
            "--database",
            DATABASE,
            "--output",
        ]);
        command.arg(self.snapshot());
        if entities {
            command.args(["--entity-collection", ENTITIES]);
        }
        command.output().unwrap()
    }

    fn import(&self, extra: &[&str]) -> Output {
        self.command()
            .args(["--chroma-snapshot", "--input"])
            .arg(self.snapshot())
            .arg("--output")
            .arg(self.output())
            .args(["--profile", "character-a"])
            .args(extra)
            .output()
            .unwrap()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"));
        command.current_dir(&self.0);
        command
    }

    fn write_snapshot(&self, snapshot: &Value) -> Vec<u8> {
        let source = serde_json::to_vec(snapshot).unwrap();
        fs::write(self.snapshot(), &source).unwrap();
        source
    }

    fn assert_no_output(&self) {
        assert!(!self.output().exists());
        assert_eq!(
            fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from("snapshot.json")],
            "failed imports must leave no partial or temporary output"
        );
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        // Remove only the exact files this fixture owns; never traverse other paths.
        for name in ["snapshot.json", "memory.jsonl", "memory.jsonl.lock"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/chroma-v2-1.4.0.json")).unwrap()
}

fn collection_path(id_or_name: &str) -> String {
    format!("/api/v2/tenants/{TENANT}/databases/{DATABASE}/collections/{id_or_name}")
}

fn version_step(version: Value) -> Step {
    Step::json("GET", "/api/v2/version", None, version)
}

fn descriptor_step(collection: &Value, requested_name: &str) -> Step {
    Step::json(
        "GET",
        &collection_path(requested_name),
        None,
        collection["descriptor"].clone(),
    )
}

fn count_step(collection: &Value, count: usize) -> Step {
    Step::json(
        "GET",
        &format!(
            "{}/count",
            collection_path(collection["descriptor"]["id"].as_str().unwrap())
        ),
        None,
        json!(count),
    )
}

fn page(records: &[Value]) -> Value {
    let column = |key: &str| {
        records
            .iter()
            .map(|record| record[key].clone())
            .collect::<Vec<_>>()
    };
    json!({
        "ids": column("id"),
        "documents": column("document"),
        "metadatas": column("metadata"),
        "embeddings": column("embedding"),
        "uris": column("uri"),
        "include": ["documents", "metadatas", "embeddings", "uris"]
    })
}

fn page_step(collection: &Value, offset: usize, response: Value) -> Step {
    Step::json(
        "POST",
        &format!(
            "{}/get",
            collection_path(collection["descriptor"]["id"].as_str().unwrap())
        ),
        Some(
            json!({"limit":128,"offset":offset,"include":["documents","metadatas","embeddings","uris"]}),
        ),
        response,
    )
}

fn collection_steps(collection: &Value, requested_name: &str) -> Vec<Step> {
    let records = collection["records"].as_array().unwrap();
    let mut steps = vec![
        descriptor_step(collection, requested_name),
        count_step(collection, records.len()),
    ];
    for (index, chunk) in records.chunks(128).enumerate() {
        steps.push(page_step(collection, index * 128, page(chunk)));
    }
    steps.push(count_step(collection, records.len()));
    steps
}

fn export_steps(snapshot: &Value, entities: bool) -> Vec<Step> {
    let mut steps = vec![version_step(snapshot["reported_version"].clone())];
    steps.extend(collection_steps(&snapshot["main"], MAIN));
    if entities {
        steps.extend(collection_steps(&snapshot["entities"], ENTITIES));
    }
    steps
}

fn records(count: usize) -> Vec<Value> {
    let base = fixture()["main"]["records"][0].clone();
    (0..count)
        .map(|index| {
            let mut record = base.clone();
            record["id"] = json!(Uuid::from_u128(index as u128 + 1).to_string());
            record
        })
        .collect()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_rejected(output: &Output, reason: &str) {
    assert!(
        !output.status.success(),
        "unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(reason),
        "expected {reason:?}, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn paged_read_only_export_preserves_raw_archive_and_import_reopens_in_one_profile() {
    let files = Files::new();
    let mut expected = fixture();
    expected["main"]["records"] = json!(records(257));
    expected["main"]["record_count"] = json!(257);
    let mock = Mock::start(export_steps(&expected, true));
    let exported = files.export(&mock.url(), true);
    let requests = mock.finish();
    assert_success(&exported);
    let offsets: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.path
                == format!(
                    "{}/get",
                    collection_path(expected["main"]["descriptor"]["id"].as_str().unwrap())
                )
        })
        .map(|request| request.body.as_ref().unwrap()["offset"].as_u64().unwrap())
        .collect();
    assert_eq!(offsets, vec![0, 128, 256]);
    // The scripted mock rejects any query, embedding/model, mutation, or extra route.
    let archive = fs::read(files.snapshot()).unwrap();
    let snapshot: Value = serde_json::from_slice(&archive).unwrap();
    assert_eq!(snapshot, expected);
    assert_eq!(snapshot["entities"]["records"][0]["id"], "entity_like_tea");
    assert!(String::from_utf8_lossy(&exported.stdout).contains("archived only"));

    assert_rejected(
        &files.import(&["--utc-offset-minutes", "480"]),
        "--embedding-space",
    );
    files.assert_no_output();
    assert_rejected(&files.import(&["--embedding-space", SPACE]), "UTC offset");
    files.assert_no_output();
    let extra = ["--utc-offset-minutes", "480", "--embedding-space", SPACE];
    let imported = files.import(&extra);
    assert_success(&imported);
    assert!(String::from_utf8_lossy(&imported.stdout).contains("archived only"));
    let output = fs::read(files.output()).unwrap();
    let entries = read_jsonl(&files.output());
    assert_eq!(entries.len(), 257, "entity records stay archived");
    for (entry, original) in entries
        .iter()
        .zip(expected["main"]["records"].as_array().unwrap())
    {
        assert_eq!(entry["id"], original["id"]);
        assert_eq!(entry["legacy_metadata"], original["metadata"]);
        assert_eq!(
            entry["legacy_metadata"]["entities"],
            "[\"like_tea\",\"茶🍵\"]"
        );
        assert_eq!(
            entry["embedding"],
            json!({"space":SPACE,"values":[0.25,-0.5,1.0]})
        );
        assert_eq!(entry["created_ms"], 123);
        assert_eq!(entry["source"], "automatic");
    }
    let store = MemoryStore::open(files.output()).await.unwrap();
    assert_eq!(store.len().await, 0);
    store.select_profile("character-a").await.unwrap();
    assert_eq!(store.len().await, 257);
    drop(store);
    let reopened = MemoryStore::open(files.output()).await.unwrap();
    reopened.select_profile("other-profile").await.unwrap();
    assert_eq!(reopened.len().await, 0);
    reopened.select_profile("character-a").await.unwrap();
    assert_eq!(reopened.len().await, 257);
    let recalled = reopened
        .recall_kind(Some(MemoryKind::Conversation), "茶", 5)
        .await
        .unwrap();
    assert_eq!(recalled.len(), 5);
    assert!(
        recalled
            .iter()
            .all(|message| message.content.contains("茶") && message.content.contains("好"))
    );
    drop(reopened);

    assert_rejected(&files.import(&extra), "cannot commit migrated output");
    assert_eq!(fs::read(files.output()).unwrap(), output);
    assert_eq!(fs::read(files.snapshot()).unwrap(), archive);
    let unused = Mock::start(Vec::new());
    assert_rejected(&files.export(&unused.url(), true), "refusing to overwrite");
    assert!(
        unused.finish().is_empty(),
        "existing exports need no network access"
    );
    assert_eq!(fs::read(files.snapshot()).unwrap(), archive);
}

#[test]
fn empty_collection_exports_without_get_and_imports_as_an_empty_store_file() {
    let files = Files::new();
    let mut expected = fixture();
    expected["main"]["records"] = json!([]);
    expected["main"]["record_count"] = json!(0);
    expected["entities"] = Value::Null;
    let mock = Mock::start(export_steps(&expected, false));
    let output = files.export(&mock.url(), false);
    let requests = mock.finish();
    assert_success(&output);
    assert!(requests.iter().all(|request| request.method == "GET"));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(files.snapshot()).unwrap()).unwrap(),
        expected
    );
    let original = fs::read(files.snapshot()).unwrap();
    assert_success(&files.import(&[]));
    assert!(fs::read(files.output()).unwrap().is_empty());
    assert_eq!(fs::read(files.snapshot()).unwrap(), original);
}

#[test]
fn media_uri_is_archived_verbatim_but_cannot_be_imported_as_text_memory() {
    let files = Files::new();
    let mut snapshot = fixture();
    snapshot["entities"] = Value::Null;
    snapshot["main"]["records"][0]["uri"] = json!("file:///legacy-media/茶.png");
    let mock = Mock::start(export_steps(&snapshot, false));
    let output = files.export(&mock.url(), false);
    mock.finish();
    assert_success(&output);
    let original = fs::read(files.snapshot()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&original).unwrap(),
        snapshot
    );
    assert_rejected(
        &files.import(&["--utc-offset-minutes", "480", "--embedding-space", SPACE]),
        "media URIs are unsupported",
    );
    files.assert_no_output();
    assert_eq!(fs::read(files.snapshot()).unwrap(), original);
}

#[test]
fn missing_null_and_misaligned_requested_columns_abort_export() {
    for (column, replacement, remove, reason) in [
        ("uris", Value::Null, true, "omitted uris"),
        ("uris", Value::Null, false, "omitted uris"),
        ("uris", json!([]), false, "different lengths"),
        ("documents", json!([]), false, "different lengths"),
        ("metadatas", json!([]), false, "different lengths"),
        ("embeddings", json!([]), false, "different lengths"),
    ] {
        let files = Files::new();
        let snapshot = fixture();
        let main = &snapshot["main"];
        let mut response = page(main["records"].as_array().unwrap());
        if remove {
            response.as_object_mut().unwrap().remove(column).unwrap();
        } else {
            response[column] = replacement;
        }
        let mock = Mock::start(vec![
            version_step(json!("1.0.0")),
            descriptor_step(main, MAIN),
            count_step(main, 1),
            page_step(main, 0, response),
        ]);
        let output = files.export(&mock.url(), false);
        mock.finish();
        assert_rejected(&output, reason);
        assert!(!files.snapshot().exists());
        assert_eq!(fs::read_dir(&files.0).unwrap().count(), 0);
    }
}

#[test]
fn duplicate_record_ids_across_page_boundaries_abort_export() {
    let files = Files::new();
    let main = fixture()["main"].clone();
    let first = records(128);
    let mock = Mock::start(vec![
        version_step(json!("1.0.0")),
        descriptor_step(&main, MAIN),
        count_step(&main, 129),
        page_step(&main, 0, page(&first)),
        page_step(&main, 128, page(&first[..1])),
    ]);
    let output = files.export(&mock.url(), false);
    mock.finish();
    assert_rejected(&output, "duplicate Chroma record ID");
    assert!(!files.snapshot().exists());
    assert_eq!(fs::read_dir(&files.0).unwrap().count(), 0);
}

#[test]
fn truncated_pages_and_changing_counts_never_publish_an_archive() {
    for scenario in ["empty-followup", "http-truncation", "changing-count"] {
        let files = Files::new();
        let main = fixture()["main"].clone();
        let initial_count = if scenario == "empty-followup" { 2 } else { 1 };
        let mut steps = vec![
            version_step(json!("1.0.0")),
            descriptor_step(&main, MAIN),
            count_step(&main, initial_count),
        ];
        let first_page = page_step(&main, 0, page(main["records"].as_array().unwrap()));
        if scenario == "http-truncation" {
            steps.push(first_page.truncated());
        } else {
            steps.push(first_page);
            if scenario == "empty-followup" {
                steps.push(page_step(&main, 1, page(&[])));
            } else {
                steps.push(count_step(&main, 2));
            }
        }
        let mock = Mock::start(steps);
        let output = files.export(&mock.url(), false);
        mock.finish();
        assert_rejected(
            &output,
            if scenario == "http-truncation" {
                "cannot read Chroma response"
            } else {
                "count changed"
            },
        );
        assert!(!files.snapshot().exists());
        assert_eq!(fs::read_dir(&files.0).unwrap().count(), 0);
    }
}

#[test]
fn a_descriptor_for_another_tenant_database_or_name_is_rejected() {
    for (field, value, reason) in [
        ("tenant", "another-tenant", "tenant or database"),
        ("database", "another-database", "tenant or database"),
        ("name", "another-collection", "different collection name"),
    ] {
        let files = Files::new();
        let mut main = fixture()["main"].clone();
        main["descriptor"][field] = json!(value);
        let mock = Mock::start(vec![
            version_step(json!("1.0.0")),
            descriptor_step(&main, MAIN),
        ]);
        let output = files.export(&mock.url(), false);
        mock.finish();
        assert_rejected(&output, reason);
        assert!(!files.snapshot().exists());
    }
}

#[test]
fn a_requested_missing_entity_collection_aborts_the_complete_export() {
    let files = Files::new();
    let snapshot = fixture();
    let mut steps = vec![version_step(json!("1.0.0"))];
    steps.extend(collection_steps(&snapshot["main"], MAIN));
    steps.push(
        Step::json(
            "GET",
            &collection_path(ENTITIES),
            None,
            json!({"error":"NotFound"}),
        )
        .status(404),
    );
    let mock = Mock::start(steps);
    let output = files.export(&mock.url(), true);
    mock.finish();
    assert_rejected(&output, "HTTP 404");
    assert!(!files.snapshot().exists());
    assert_eq!(fs::read_dir(&files.0).unwrap().count(), 0);
}

#[test]
fn export_enforces_the_combined_main_and_entity_record_budget() {
    for entities in [false, true] {
        let files = Files::new();
        let snapshot = fixture();
        let mut steps = vec![version_step(json!("1.0.0"))];
        if entities {
            steps.extend(collection_steps(&snapshot["main"], MAIN));
            steps.push(descriptor_step(&snapshot["entities"], ENTITIES));
            steps.push(count_step(&snapshot["entities"], 50_000));
        } else {
            steps.push(descriptor_step(&snapshot["main"], MAIN));
            steps.push(count_step(&snapshot["main"], 50_001));
        }
        let mock = Mock::start(steps);
        let output = files.export(&mock.url(), entities);
        mock.finish();
        assert_rejected(&output, "50000 records");
        assert!(!files.snapshot().exists());
    }
}

#[test]
fn invalid_reported_versions_are_rejected_by_export_and_offline_import() {
    for version in [
        String::new(),
        " \t ".to_owned(),
        "1.0.0\n".to_owned(),
        "v".repeat(129),
    ] {
        let files = Files::new();
        let mock = Mock::start(vec![version_step(json!(version))]);
        let output = files.export(&mock.url(), false);
        mock.finish();
        assert_rejected(&output, "version response");
        assert!(!files.snapshot().exists());
        let mut snapshot = fixture();
        snapshot["reported_version"] = json!(version);
        let original = files.write_snapshot(&snapshot);
        let output = files.import(&["--utc-offset-minutes", "480", "--embedding-space", SPACE]);
        assert_rejected(&output, "version response");
        files.assert_no_output();
        assert_eq!(fs::read(files.snapshot()).unwrap(), original);
    }
}

#[test]
fn invalid_snapshot_envelopes_counts_record_ids_and_collection_identity_are_rejected() {
    let base = fixture();
    let mut invalid = Vec::new();
    for (field, value) in [
        ("format", json!("unknown")),
        ("schema_version", json!(2)),
        ("api_version", json!("v1")),
    ] {
        let mut snapshot = base.clone();
        snapshot[field] = value;
        invalid.push((snapshot, "unsupported Chroma snapshot format or version"));
    }
    let mut snapshot = base.clone();
    snapshot["main"]["record_count"] = json!(50_001);
    invalid.push((snapshot, "50000 records"));
    let mut snapshot = base.clone();
    snapshot["main"]["record_count"] = json!(2);
    invalid.push((snapshot, "record count does not match"));
    for section in ["main", "entities"] {
        let mut snapshot = base.clone();
        snapshot[section]["records"]
            .as_array_mut()
            .unwrap()
            .push(base[section]["records"][0].clone());
        snapshot[section]["record_count"] = json!(2);
        invalid.push((snapshot, "duplicate Chroma snapshot record ID"));
    }
    for field in ["name", "id"] {
        let mut snapshot = base.clone();
        snapshot["entities"]["descriptor"][field] = base["main"]["descriptor"][field].clone();
        invalid.push((snapshot, "must be distinct"));
    }
    let mut snapshot = base.clone();
    snapshot["unexpected"] = json!(true);
    invalid.push((snapshot, "unsupported Chroma snapshot shape"));
    for (snapshot, reason) in invalid {
        let files = Files::new();
        let original = files.write_snapshot(&snapshot);
        assert_rejected(
            &files.import(&["--utc-offset-minutes", "480", "--embedding-space", SPACE]),
            reason,
        );
        files.assert_no_output();
        assert_eq!(fs::read(files.snapshot()).unwrap(), original);
    }
}

#[test]
fn collection_uuid_aliases_are_rejected_by_export_and_offline_import() {
    let files = Files::new();
    let mut snapshot = fixture();
    snapshot["main"]["descriptor"]["id"] = json!("00000000-0000-4000-8000-00000000ABCD");
    snapshot["entities"]["descriptor"]["id"] = json!("00000000-0000-4000-8000-00000000abcd");
    let mock = Mock::start(export_steps(&snapshot, true));
    let output = files.export(&mock.url(), true);
    mock.finish();
    assert_rejected(&output, "must be distinct");
    assert!(!files.snapshot().exists());
    assert_eq!(fs::read_dir(&files.0).unwrap().count(), 0);

    let original = files.write_snapshot(&snapshot);
    assert_rejected(
        &files.import(&["--utc-offset-minutes", "480", "--embedding-space", SPACE]),
        "must be distinct",
    );
    files.assert_no_output();
    assert_eq!(fs::read(files.snapshot()).unwrap(), original);
}
