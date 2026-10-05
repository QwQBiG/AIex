#![forbid(unsafe_code)]

mod entities;
mod knowledge;
mod management;
pub use knowledge::{MediaStats, observation_media};
mod record;
mod retrieval;
mod writer;

#[cfg(test)]
mod persistence_tests;

#[cfg(test)]
mod management_tests;
#[cfg(test)]
mod profile_tests;

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ai_ex_core::MemoryPort;
use ai_ex_domain::{
    AppError, ComponentHealth, MemoryEntry, MemoryKind, MemoryProjection, MemorySource, Message,
    Role, TurnId,
};
use async_trait::async_trait;
use record::MemoryRecord;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;
use writer::read_records;

#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<MemoryInner>,
    embedder: Option<Arc<dyn ai_ex_core::EmbeddingPort>>,
}

struct MemoryInner {
    profile_id: RwLock<String>,
    path: PathBuf,
    records: RwLock<Vec<MemoryRecord>>,
    write_lock: Mutex<()>,
    writer_lease: Mutex<Option<std::fs::File>>,
    enabled: bool,
    embedding_failed: RwLock<bool>,
}

impl MemoryStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref().to_owned();
        let records = read_records(&path).await?;
        Ok(Self {
            inner: Arc::new(MemoryInner {
                profile_id: RwLock::new(record::default_profile()),
                path,
                records: RwLock::new(records),
                write_lock: Mutex::new(()),
                writer_lease: Mutex::new(None),
                enabled: true,
                embedding_failed: RwLock::new(false),
            }),
            embedder: None,
        })
    }

    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(MemoryInner {
                profile_id: RwLock::new(record::default_profile()),
                path: PathBuf::new(),
                records: RwLock::new(Vec::new()),
                write_lock: Mutex::new(()),
                writer_lease: Mutex::new(None),
                enabled: false,
                embedding_failed: RwLock::new(false),
            }),
            embedder: None,
        }
    }

    pub async fn len(&self) -> usize {
        self.count(None).await
    }

    pub async fn count(&self, kind: Option<MemoryKind>) -> usize {
        let profile_id = self.inner.profile_id.read().await;
        self.inner
            .records
            .read()
            .await
            .iter()
            .filter(|record| record.profile_id == *profile_id)
            .filter(|record| kind.is_none_or(|expected| record.kind == expected))
            .count()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    pub async fn select_profile(&self, profile_id: &str) -> Result<(), AppError> {
        if profile_id.trim().is_empty() || profile_id.chars().count() > 128 {
            return Err(AppError::configuration(
                "memory profile ID is outside supported bounds",
            ));
        }
        // Readers retain their scope until their complete operation has finished.
        *self.inner.profile_id.write().await = profile_id.to_owned();
        Ok(())
    }

    pub async fn health(&self) -> ComponentHealth {
        if !self.inner.enabled {
            return ComponentHealth {
                component: "memory".to_owned(),
                ready: true,
                detail: "disabled".to_owned(),
            };
        }
        ComponentHealth {
            component: "memory".to_owned(),
            ready: true,
            detail: {
                let profile_id = self.inner.profile_id.read().await;
                let records = self.inner.records.read().await;
                let scoped: Vec<_> = records
                    .iter()
                    .filter(|record| record.profile_id == *profile_id)
                    .collect();
                if let Some(provider) = &self.embedder {
                    let indexed = scoped
                        .iter()
                        .filter(|record| {
                            record
                                .embedding
                                .as_ref()
                                .is_some_and(|embedding| embedding.space == provider.space())
                        })
                        .count();
                    let state = if *self.inner.embedding_failed.read().await {
                        "provider unavailable; lexical fallback"
                    } else {
                        "semantic + lexical"
                    };
                    format!(
                        "{} record(s); {indexed} indexed in configured space; {state}",
                        scoped.len()
                    )
                } else {
                    format!("{} record(s); lexical retrieval", scoped.len())
                }
            },
        }
    }

    pub async fn recall_kind(
        &self,
        kind: Option<MemoryKind>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>, AppError> {
        let kinds = kind.map(|kind| [kind]);
        self.recall_filtered(
            kinds.as_ref().map(|kinds| kinds.as_slice()),
            query,
            limit,
            false,
        )
        .await
    }

    pub async fn recall_kinds(
        &self,
        kinds: &[MemoryKind],
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>, AppError> {
        self.recall_filtered(Some(kinds), query, limit, true).await
    }
    pub async fn remember_kind(
        &mut self,
        kind: MemoryKind,
        turn_id: TurnId,
        user_text: String,
        assistant_text: String,
    ) -> Result<(), AppError> {
        self.remember_enriched(kind, turn_id, user_text, assistant_text, None)
            .await
    }

    /// Record an observed caption/transcript and structured features without reading devices.
    pub async fn remember_media(
        &mut self,
        kind: MemoryKind,
        turn_id: TurnId,
        text: String,
        media: ai_ex_domain::MemoryMedia,
    ) -> Result<(), AppError> {
        media.validate()?;
        self.remember_enriched(kind, turn_id, text, String::new(), Some(media))
            .await
    }

    async fn remember_enriched(
        &mut self,
        kind: MemoryKind,
        turn_id: TurnId,
        user_text: String,
        assistant_text: String,
        media: Option<ai_ex_domain::MemoryMedia>,
    ) -> Result<(), AppError> {
        if !self.inner.enabled {
            return Ok(());
        }
        let profile_id = self.inner.profile_id.read().await;
        let _write_guard = self.inner.write_lock.lock().await;
        self.ensure_writer().await?;
        if let Some(parent) = self.inner.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| AppError::unavailable(error.to_string()))?;
        }
        let mut record = MemoryRecord::new(MemoryEntry {
            profile_id: profile_id.clone(),
            id: Uuid::new_v4(),
            turn_id,
            created_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            kind,
            user_text,
            assistant_text,
            updated_ms: None,
            source: MemorySource::Automatic,
            revision: 1,
        });
        record.knowledge.media = media;
        self.enrich(&mut record).await?;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.path)
            .await
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        let line = serde_json::to_string(&record)
            .map_err(|error| AppError::protocol(error.to_string()))?;
        file.write_all(format!("{line}\n").as_bytes())
            .await
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        file.flush()
            .await
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        file.sync_data()
            .await
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        self.inner.records.write().await.push(record);
        Ok(())
    }

    pub async fn remember_projection(
        &mut self,
        projection: &MemoryProjection,
    ) -> Result<(), AppError> {
        self.remember_kind(
            projection.kind,
            projection.turn_id.unwrap_or_default(),
            projection.user_text.clone(),
            projection.assistant_text.clone(),
        )
        .await
    }
    pub async fn export_kind(
        &self,
        kind: Option<MemoryKind>,
        destination: impl AsRef<Path>,
    ) -> Result<usize, AppError> {
        let profile_id = self.inner.profile_id.read().await;
        let records: Vec<_> = self
            .inner
            .records
            .read()
            .await
            .iter()
            .filter(|record| record.profile_id == *profile_id)
            .filter(|record| kind.is_none_or(|expected| record.kind == expected))
            .cloned()
            .collect();
        let content = serialize_records(&records)?;
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| AppError::unavailable(error.to_string()))?;
        }
        write_new_synced(destination, content.as_bytes())
            .await
            .map_err(|error| AppError::unavailable(error.to_string()))?;
        Ok(records.len())
    }

    pub async fn clear_kind(&mut self, kind: MemoryKind) -> Result<usize, AppError> {
        if !self.inner.enabled {
            return Ok(0);
        }
        let profile_id = self.inner.profile_id.read().await;
        let _write_guard = self.inner.write_lock.lock().await;
        self.ensure_writer().await?;
        let records = self.inner.records.read().await;
        let retained: Vec<_> = records
            .iter()
            .filter(|record| record.profile_id != *profile_id || record.kind != kind)
            .cloned()
            .collect();
        let removed = records.len().saturating_sub(retained.len());
        drop(records);
        if removed == 0 {
            return Ok(0);
        }
        self.replace_records(retained).await?;
        Ok(removed)
    }
}

#[async_trait]
impl MemoryPort for MemoryStore {
    async fn manage(
        &mut self,
        request: ai_ex_domain::MemoryRequest,
    ) -> Result<ai_ex_domain::MemoryResponse, AppError> {
        MemoryStore::manage(self, request).await
    }

    async fn select_profile(&mut self, profile_id: &str) -> Result<(), AppError> {
        MemoryStore::select_profile(self, profile_id).await
    }

    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<Message>, AppError> {
        self.recall_kind(None, query, limit).await
    }

    async fn recall_for_context(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>, AppError> {
        self.recall_kinds(
            &[
                MemoryKind::Conversation,
                MemoryKind::Persona,
                MemoryKind::Viewer,
            ],
            query,
            limit,
        )
        .await
    }

    async fn remember(
        &mut self,
        turn_id: TurnId,
        user_text: String,
        assistant_text: String,
    ) -> Result<(), AppError> {
        self.remember_kind(MemoryKind::Conversation, turn_id, user_text, assistant_text)
            .await
    }
}

async fn write_new_synced(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
    let result = async {
        file.write_all(content).await?;
        file.flush().await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    if result.is_err() {
        let _ = tokio::fs::remove_file(path).await;
    }
    result
}

fn serialize_records(records: &[MemoryRecord]) -> Result<String, AppError> {
    let mut content = String::new();
    for record in records {
        let line =
            serde_json::to_string(record).map_err(|error| AppError::protocol(error.to_string()))?;
        content.push_str(&line);
        content.push('\n');
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) async fn remove_test_files(path: &Path) {
        tokio::fs::remove_file(path)
            .await
            .expect("test memory removed");
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        match tokio::fs::remove_file(PathBuf::from(lock_path)).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => panic!("test writer sidecar removal failed: {error}"),
        }
    }

    #[tokio::test]
    async fn persists_and_recalls_relevant_turns() {
        let path = std::env::temp_dir().join(format!("ai-ex-memory-{}.jsonl", Uuid::new_v4()));
        let mut store = MemoryStore::open(&path).await.expect("store opens");
        store
            .remember(
                TurnId::new(),
                "我喜欢草莓蛋糕".to_owned(),
                "我记住了".to_owned(),
            )
            .await
            .expect("turn persisted");
        let results = store.recall("草莓", 3).await.expect("memory recalled");
        assert_eq!(results.len(), 1);

        let reopened = MemoryStore::open(&path).await.expect("store reopens");
        assert_eq!(reopened.len().await, 1);
        drop(store);
        remove_test_files(&path).await;
    }

    #[tokio::test]
    async fn filters_exports_and_clears_memory_kinds() {
        let path = std::env::temp_dir().join(format!("ai-ex-memory-{}.jsonl", Uuid::new_v4()));
        let export_path =
            std::env::temp_dir().join(format!("ai-ex-memory-export-{}.jsonl", Uuid::new_v4()));
        let mut store = MemoryStore::open(&path).await.expect("store opens");
        store
            .remember_kind(
                MemoryKind::Viewer,
                TurnId::new(),
                "观众喜欢蓝莓".to_owned(),
                "已记录".to_owned(),
            )
            .await
            .expect("viewer memory persists");
        store
            .remember_kind(
                MemoryKind::Persona,
                TurnId::new(),
                "角色喜欢星星".to_owned(),
                "保持设定".to_owned(),
            )
            .await
            .expect("persona memory persists");
        assert_eq!(store.count(Some(MemoryKind::Viewer)).await, 1);
        assert_eq!(
            store
                .recall_kind(Some(MemoryKind::Persona), "星星", 3)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .export_kind(Some(MemoryKind::Viewer), &export_path)
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.clear_kind(MemoryKind::Viewer).await.unwrap(), 1);
        assert_eq!(store.count(Some(MemoryKind::Viewer)).await, 0);

        let reopened = MemoryStore::open(&path).await.expect("store reopens");
        assert_eq!(reopened.count(Some(MemoryKind::Persona)).await, 1);
        assert_eq!(reopened.count(Some(MemoryKind::Viewer)).await, 0);
        drop(store);
        remove_test_files(&path).await;
        tokio::fs::remove_file(&export_path)
            .await
            .expect("temporary export removed");
    }

    #[tokio::test]
    async fn cloned_stores_serialize_concurrent_appends() {
        let path =
            std::env::temp_dir().join(format!("ai-ex-memory-shared-{}.jsonl", Uuid::new_v4()));
        let store = MemoryStore::open(&path).await.expect("store opens");
        let mut first = store.clone();
        let mut second = store.clone();
        let first_write = first.remember_kind(
            MemoryKind::Viewer,
            TurnId::new(),
            "观众甲".to_owned(),
            "事件一".to_owned(),
        );
        let second_write = second.remember_kind(
            MemoryKind::LiveEvent,
            TurnId::new(),
            "礼物".to_owned(),
            "事件二".to_owned(),
        );
        let (first_result, second_result) = tokio::join!(first_write, second_write);
        first_result.expect("first append succeeds");
        second_result.expect("second append succeeds");
        let reopened = MemoryStore::open(&path)
            .await
            .expect("shared memory reopens");
        assert_eq!(reopened.len().await, 2);
        drop(first);
        drop(second);
        drop(store);
        remove_test_files(&path).await;
    }
    #[tokio::test]
    async fn remembers_a_projected_live_event() {
        let path =
            std::env::temp_dir().join(format!("ai-ex-memory-projection-{}.jsonl", Uuid::new_v4()));
        let mut store = MemoryStore::open(&path).await.expect("store opens");
        let projection = MemoryProjection {
            kind: MemoryKind::LiveEvent,
            event_id: Uuid::new_v4(),
            turn_id: None,
            user_text: "收到礼物".to_owned(),
            assistant_text: "直播事件".to_owned(),
        };
        store
            .remember_projection(&projection)
            .await
            .expect("projection persists");
        assert_eq!(store.count(Some(MemoryKind::LiveEvent)).await, 1);
        drop(store);
        remove_test_files(&path).await;
    }
    #[tokio::test]
    async fn reads_legacy_records_without_a_kind() {
        let path =
            std::env::temp_dir().join(format!("ai-ex-memory-legacy-{}.jsonl", Uuid::new_v4()));
        let turn_id = serde_json::to_string(&TurnId::new()).expect("turn id serializes");
        let legacy = format!(
            "{{\"id\":\"{}\",\"turn_id\":{},\"created_ms\":1,\"user_text\":\"旧记录\",\"assistant_text\":\"兼容\"}}\n",
            Uuid::new_v4(),
            turn_id,
        );
        tokio::fs::write(&path, legacy)
            .await
            .expect("legacy memory written");
        let store = MemoryStore::open(&path).await.expect("legacy memory opens");
        assert_eq!(store.count(Some(MemoryKind::Conversation)).await, 1);
        drop(store);
        remove_test_files(&path).await;
    }

    #[tokio::test]
    async fn context_recall_excludes_live_event_memory() {
        let path =
            std::env::temp_dir().join(format!("ai-ex-memory-context-{}.jsonl", Uuid::new_v4()));
        let mut store = MemoryStore::open(&path).await.expect("store opens");
        store
            .remember_kind(
                MemoryKind::Conversation,
                TurnId::new(),
                "观众喜欢蓝莓".to_owned(),
                "对话记忆".to_owned(),
            )
            .await
            .expect("conversation memory persists");
        store
            .remember_kind(
                MemoryKind::LiveEvent,
                TurnId::new(),
                "观众喜欢蓝莓".to_owned(),
                "平台事件".to_owned(),
            )
            .await
            .expect("live event memory persists");
        let context = store
            .recall_kinds(
                &[
                    MemoryKind::Conversation,
                    MemoryKind::Persona,
                    MemoryKind::Viewer,
                ],
                "蓝莓",
                8,
            )
            .await
            .expect("context recall succeeds");
        assert_eq!(context.len(), 1);
        assert!(context[0].content.contains("对话记忆"));
        drop(store);
        remove_test_files(&path).await;
    }
}
