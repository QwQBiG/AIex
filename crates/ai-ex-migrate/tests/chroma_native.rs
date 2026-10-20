//! Opt-in acceptance against the pinned native Chroma CLI, never a Python/PATH fallback.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use ai_ex_domain::{MemoryEntry, MemoryKind, MemorySource};
use ai_ex_memory::MemoryStore;
use reqwest::{Client, Method, StatusCode};
use ring::digest::{Context, SHA256};
use serde_json::{Value, json};
use uuid::Uuid;

const NATIVE_SHA256: &str = "71884ca430ae3118d46d51a10e733f77100aff368cc0cd64363a602d1ecdeb35";
const COLLECTIONS: &str = "/api/v2/tenants/default_tenant/databases/default_database/collections";
const MAIN: &str = "vtuber_memories";
const ENTITIES: &str = "vtuber_entities";
const PROFILE: &str = "native-chroma-fixture";
const SPACE: &str = "synthetic-native-three-dimensional-v1";
const RECORD_COUNT: usize = 129; // The bridge's page size is 128.

#[tokio::test]
#[ignore = "requires the pinned AIEX_CHROMA_NATIVE_BIN and creates only temporary synthetic databases"]
async fn native_database_copy_exports_and_imports_without_touching_the_stopped_original() {
    let binary = pinned_native_binary();
    let fixture = FixtureDirectory::new();
    let source = fixture.root.join("source-db");
    let copied = fixture.root.join("copied-db");
    let snapshot = fixture.root.join("snapshot.json");
    let output = fixture.root.join("memory.jsonl");
    assert!(!source.exists() && !copied.exists() && !snapshot.exists() && !output.exists());
    let records = main_records();
    let entity = json!({
        "id": "entity:tea/non-uuid",
        "document": "Synthetic entity archive; no restored graph is implied.",
        "metadata": {"entity_type": "preference", "value": "合成茶", "mentions": 7},
        "embedding": [0.5, 1.0, -0.25],
        "uri": "aiex-fixture://entity/tea"
    });
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    {
        let (mut server, url) = start_native(&binary, &source, &fixture, "source").await;
        let main_id = create_collection(&client, &url, MAIN).await;
        let entity_id = create_collection(&client, &url, ENTITIES).await;
        add_records(&client, &url, &main_id, &records).await;
        add_records(&client, &url, &entity_id, std::slice::from_ref(&entity)).await;
        assert_eq!(
            api(
                &client,
                Method::GET,
                &format!("{url}{COLLECTIONS}/{main_id}/count"),
                None
            )
            .await,
            json!(RECORD_COUNT)
        );
        // Reading explicit vectors materializes the native persistent segment without an EF/query.
        let materialized = api(
            &client,
            Method::POST,
            &format!("{url}{COLLECTIONS}/{main_id}/get"),
            Some(json!({"limit": RECORD_COUNT, "include": ["embeddings"]})),
        )
        .await;
        assert_eq!(materialized["ids"].as_array().unwrap().len(), RECORD_COUNT);
        server.assert_alive();
        server.stop();
    }

    // The original is closed before both the byte inventory and the whole-directory copy.
    let original_files = file_inventory(&source).unwrap();
    assert!(original_files.contains_key(Path::new("chroma.sqlite3")));
    for name in ["header.bin", "data_level0.bin"] {
        assert!(
            original_files.iter().any(|(path, bytes)| path
                .file_name()
                .is_some_and(|part| part == name)
                && !bytes.is_empty()),
            "native fixture must contain persisted HNSW {name}"
        );
    }
    copy_fixture_database(&fixture.root, &source, &copied).unwrap();
    assert_eq!(file_inventory(&copied).unwrap(), original_files);

    let (main_descriptor, entity_descriptor);
    {
        let (mut server, url) = start_native(&binary, &copied, &fixture, "copy").await;
        main_descriptor = api(
            &client,
            Method::GET,
            &format!("{url}{COLLECTIONS}/{MAIN}"),
            None,
        )
        .await;
        entity_descriptor = api(
            &client,
            Method::GET,
            &format!("{url}{COLLECTIONS}/{ENTITIES}"),
            None,
        )
        .await;
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"));
        command
            .args(["--chroma-export", "--url", &url, "--collection", MAIN])
            .args([
                "--tenant",
                "default_tenant",
                "--database",
                "default_database",
            ])
            .arg("--output")
            .arg(&snapshot)
            .args(["--entity-collection", ENTITIES]);
        let stdout = run_cli(command, &fixture, "export").await;
        assert!(stdout.contains("archived only"), "{stdout}");
        server.assert_alive();
        server.stop();
    }
    assert_eq!(file_inventory(&source).unwrap(), original_files);
    let snapshot_bytes = fs::read(&snapshot).unwrap();
    let exported: Value = serde_json::from_slice(&snapshot_bytes).unwrap();
    assert_eq!(exported["format"], "ai-ex-chroma-snapshot");
    assert_eq!(exported["schema_version"], 1);
    assert_eq!(exported["api_version"], "v2");
    assert_eq!(exported["reported_version"], "1.0.0");
    assert_eq!(exported["tenant"], "default_tenant");
    assert_eq!(exported["database"], "default_database");
    assert_eq!(exported["main"]["descriptor"], main_descriptor);
    assert_eq!(exported["entities"]["descriptor"], entity_descriptor);
    assert_eq!(exported["main"]["record_count"], RECORD_COUNT);
    assert_eq!(exported["entities"]["record_count"], 1);
    assert_records(&exported["main"]["records"], &records);
    assert_records(&exported["entities"]["records"], &[entity]);

    // Both native servers are stopped: this import and MemoryStore reopen are offline.
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-ex-migrate"));
    command
        .args(["--chroma-snapshot", "--input"])
        .arg(&snapshot)
        .arg("--output")
        .arg(&output)
        .args(["--profile", PROFILE, "--embedding-space", SPACE]);
    let stdout = run_cli(command, &fixture, "import").await;
    assert!(stdout.contains("archived only"), "{stdout}");
    let imported_bytes = fs::read(&output).unwrap();
    let imported: Vec<Value> = std::str::from_utf8(&imported_bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(imported.len(), RECORD_COUNT); // The non-UUID entity was archived, not imported.
    let by_id: BTreeMap<_, _> = records
        .iter()
        .map(|record| (record["id"].as_str().unwrap(), record))
        .collect();
    for memory in &imported {
        let expected = by_id[memory["id"].as_str().unwrap()];
        assert_eq!(memory["legacy_metadata"], expected["metadata"]);
        assert_eq!(memory["embedding"]["values"], expected["embedding"]);
        assert_eq!(memory["embedding"]["space"], SPACE);
        let entry: MemoryEntry = serde_json::from_value(memory.clone()).unwrap();
        assert_eq!(entry.profile_id, PROFILE);
        assert_eq!(entry.source, MemorySource::Automatic);
        assert_eq!(entry.created_ms, 1_792_499_696_789);
        assert_eq!(entry.revision, 1);
        assert_eq!(
            entry.user_text,
            expected["metadata"]["user_input"].as_str().unwrap()
        );
        assert_eq!(
            entry.assistant_text,
            expected["metadata"]["ai_response"].as_str().unwrap()
        );
        assert_eq!(
            entry.kind,
            if expected["metadata"]["memory_type"] == "EVENT" {
                MemoryKind::LiveEvent
            } else {
                MemoryKind::Conversation
            }
        );
        assert!(memory.get("knowledge").is_none());
    }

    let store = MemoryStore::open(&output).await.unwrap();
    assert_eq!(store.len().await, 0);
    store.select_profile(PROFILE).await.unwrap();
    assert_eq!(store.len().await, RECORD_COUNT);
    assert!(
        !store
            .recall_kind(None, "native-paging-token-0128", 5)
            .await
            .unwrap()
            .is_empty()
    );
    store.select_profile("other-native-profile").await.unwrap();
    assert_eq!(store.len().await, 0);
    assert!(
        store
            .recall_kind(None, "native-paging-token-0128", 5)
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);
    let reopened = MemoryStore::open(&output).await.unwrap();
    reopened.select_profile(PROFILE).await.unwrap();
    assert_eq!(reopened.len().await, RECORD_COUNT);
    let recalled = reopened
        .recall_kind(None, "native-paging-token-0128", 5)
        .await
        .unwrap();
    assert!(
        recalled
            .iter()
            .any(|message| message.content.contains("native-paging-token-0128"))
    );
    drop(reopened);
    assert_eq!(fs::read(&output).unwrap(), imported_bytes);
    assert_eq!(fs::read(&snapshot).unwrap(), snapshot_bytes);
    assert_eq!(file_inventory(&source).unwrap(), original_files);
}

fn main_records() -> Vec<Value> {
    (0..RECORD_COUNT)
        .map(|index| {
            let marker = format!("native-paging-token-{index:04}");
            let user = format!("合成茶 {marker}");
            let assistant = format!("已保存合成记录 {index}");
            json!({
                "id": Uuid::from_u128(0x00000000_0000_4000_8000_000000000001 + index as u128).to_string(),
                "document": format!("User: {user}\nAI: {assistant}"),
                "metadata": {
                    "timestamp": "2026-10-20T12:34:56.789Z",
                    "memory_type": (["INTERACTION", "SUMMARY", "EVENT"][index % 3]),
                    "user_input": user,
                    "ai_response": assistant,
                    "fixture_index": index,
                    "fixture_weight": 0.25,
                    "fixture_confirmed": false,
                    "opaque_legacy_json": "{\"nested\":[\"茶\",1],\"preserve\":true}"
                },
                "embedding": [1.0, index as f32 / 256.0, -0.5],
                "uri": null
            })
        })
        .collect()
}

fn assert_records(actual: &Value, expected: &[Value]) {
    let actual = actual.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    let actual: BTreeMap<_, _> = actual
        .iter()
        .map(|record| (record["id"].as_str().unwrap(), record))
        .collect();
    assert_eq!(
        actual.len(),
        expected.len(),
        "record IDs must be unique across pages"
    );
    for expected in expected {
        assert_eq!(actual[expected["id"].as_str().unwrap()], expected);
    }
}

async fn create_collection(client: &Client, url: &str, name: &str) -> String {
    let result = api(
        client,
        Method::POST,
        &format!("{url}{COLLECTIONS}"),
        Some(json!({
            "name": name,
            "metadata": {"fixture": "aiex-native-synthetic-only"},
            "configuration": {
                "hnsw": {"space": "l2", "batch_size": 16, "sync_threshold": 32, "num_threads": 1},
                "embedding_function": null
            },
            "get_or_create": false
        })),
    )
    .await;
    result["id"].as_str().unwrap().to_owned()
}

async fn add_records(client: &Client, url: &str, collection: &str, records: &[Value]) {
    let column = |name: &str| {
        records
            .iter()
            .map(|record| record[name].clone())
            .collect::<Vec<_>>()
    };
    api(
        client,
        Method::POST,
        &format!("{url}{COLLECTIONS}/{collection}/add"),
        Some(json!({
            "ids": column("id"), "documents": column("document"), "metadatas": column("metadata"),
            "embeddings": column("embedding"), "uris": column("uri")
        })),
    )
    .await;
}

async fn api(client: &Client, method: Method, url: &str, body: Option<Value>) -> Value {
    let mut request = client.request(method, url);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{url}: HTTP {status}: {text}");
    serde_json::from_str(&text).unwrap()
}

fn pinned_native_binary() -> PathBuf {
    let path = PathBuf::from(std::env::var_os("AIEX_CHROMA_NATIVE_BIN").expect(
        "set AIEX_CHROMA_NATIVE_BIN to the absolute path of the verified cli-1.3.0 chroma-windows.exe; no PATH/Python fallback is allowed",
    ));
    assert!(
        path.is_absolute(),
        "AIEX_CHROMA_NATIVE_BIN must be absolute"
    );
    let path = fs::canonicalize(path).expect("native Chroma binary must exist");
    assert!(
        path.is_file(),
        "native Chroma binary must be a regular file"
    );
    let mut file = File::open(&path).unwrap();
    let mut hash = Context::new(&SHA256);
    let mut buffer = [0; 64 * 1024];
    loop {
        let length = file.read(&mut buffer).unwrap();
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    let actual: String = hash
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(
        actual, NATIVE_SHA256,
        "AIEX_CHROMA_NATIVE_BIN must be the pinned official Windows native CLI asset"
    );
    path
}

async fn start_native(
    binary: &Path,
    database: &Path,
    fixture: &FixtureDirectory,
    label: &str,
) -> (ManagedChild, String) {
    assert_eq!(database.parent(), Some(fixture.root.as_path()));
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let url = format!("http://127.0.0.1:{port}");
    let mut command = Command::new(binary);
    command.arg("run").arg("--path").arg(database).args([
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
    ]);
    let mut server = ManagedChild::spawn(command, fixture, label);
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        server.assert_alive(); // The native CLI can print a run error and still exit zero.
        if let Ok(response) = client.get(format!("{url}/api/v2/healthcheck")).send().await
            && response.status() == StatusCode::OK
        {
            server.assert_alive();
            return (server, url);
        }
        assert!(
            Instant::now() < deadline,
            "native Chroma readiness timed out\n{}",
            server.logs()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_cli(command: Command, fixture: &FixtureDirectory, label: &str) -> String {
    let mut child = ManagedChild::spawn(command, fixture, label);
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.child.try_wait().unwrap() {
            assert!(
                status.success(),
                "migration CLI failed: {status}\n{}",
                child.logs()
            );
            return fs::read_to_string(&child.stdout).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "migration CLI timed out\n{}",
            child.logs()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct ManagedChild {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl ManagedChild {
    fn spawn(mut command: Command, fixture: &FixtureDirectory, label: &str) -> Self {
        let stdout = fixture.root.join(format!("{label}.stdout.log"));
        let stderr = fixture.root.join(format!("{label}.stderr.log"));
        command
            .current_dir(&fixture.root)
            .stdin(Stdio::null())
            .stdout(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stdout)
                    .unwrap(),
            )
            .stderr(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&stderr)
                    .unwrap(),
            )
            .env("ANONYMIZED_TELEMETRY", "FALSE")
            .env("TOKIO_WORKER_THREADS", "1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW; safe std API.
        }
        Self {
            child: command.spawn().expect("start explicit native/test binary"),
            stdout,
            stderr,
        }
    }

    fn logs(&self) -> String {
        format!(
            "stdout:\n{}\nstderr:\n{}",
            fs::read_to_string(&self.stdout).unwrap_or_default(),
            fs::read_to_string(&self.stderr).unwrap_or_default()
        )
    }

    fn assert_alive(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "native Chroma exited before readiness/export\n{}",
            self.logs()
        );
    }

    fn stop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
        }
        let _status: ExitStatus = self.child.wait().unwrap();
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ignored = self.child.kill();
        }
        let _ignored = self.child.wait();
        if std::thread::panicking() {
            eprintln!("{}", self.logs());
        }
    }
}

struct FixtureDirectory {
    root: PathBuf,
    temp_parent: PathBuf,
}

impl FixtureDirectory {
    fn new() -> Self {
        let temp_parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = temp_parent.join(format!("aiex-chroma-native-{}", Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self { root, temp_parent }
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        // Validate the resolved, freshly generated root and every entry before recursive removal.
        let checked = (|| -> io::Result<PathBuf> {
            reject_link(&fs::symlink_metadata(&self.root)?)?;
            let root = fs::canonicalize(&self.root)?;
            if root.parent() != Some(self.temp_parent.as_path())
                || !root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("aiex-chroma-native-")
                            && Uuid::parse_str(&name[19..]).is_ok()
                    })
            {
                return Err(io::Error::other(
                    "refusing cleanup outside the owned temporary fixture",
                ));
            }
            checked_files(&root, &root, &mut Vec::new())?;
            Ok(root)
        })();
        match checked.and_then(fs::remove_dir_all) {
            Ok(()) => {}
            Err(error) => eprintln!(
                "temporary Chroma fixture retained at {}: {error}",
                self.root.display()
            ),
        }
    }
}

fn reject_link(metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other("fixture links are forbidden"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x0400 != 0 {
            return Err(io::Error::other("fixture reparse points are forbidden"));
        }
    }
    Ok(())
}

fn checked_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(current)?;
    reject_link(&metadata)?;
    if !fs::canonicalize(current)?.starts_with(root) {
        return Err(io::Error::other("fixture path escaped its root"));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(current)? {
            checked_files(root, &entry?.path(), files)?;
        }
    } else if metadata.is_file() {
        files.push(
            current
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_owned(),
        );
    } else {
        return Err(io::Error::other("unsupported fixture file type"));
    }
    Ok(())
}

fn file_inventory(root: &Path) -> io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = Vec::new();
    checked_files(root, root, &mut files)?;
    files
        .into_iter()
        .map(|path| Ok((path.clone(), fs::read(root.join(path))?)))
        .collect()
}

fn copy_fixture_database(owned_root: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    let owned_root = fs::canonicalize(owned_root)?;
    if fs::canonicalize(source)?.parent() != Some(owned_root.as_path())
        || destination.parent() != Some(owned_root.as_path())
        || destination.exists()
    {
        return Err(io::Error::other(
            "database copy must stay inside the new owned fixture",
        ));
    }
    let mut files = Vec::new();
    checked_files(source, source, &mut files)?;
    copy_fixture_tree(source, destination)
}

fn copy_fixture_tree(source: &Path, destination: &Path) -> io::Result<()> {
    reject_link(&fs::symlink_metadata(source)?)?;
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        reject_link(&metadata)?;
        let target = destination.join(entry.file_name());
        if target.exists() {
            return Err(io::Error::other("refusing to overwrite fixture file"));
        }
        if metadata.is_dir() {
            copy_fixture_tree(&entry.path(), &target)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::other("unsupported fixture file type"));
        }
    }
    Ok(())
}
