use ai_ex_core::{EmbeddingPort, MemoryPort};
use ai_ex_domain::{AppError, MemoryKind, MemoryRecallPolicy, MemoryRequest, TurnId};
use ai_ex_memory::MemoryStore;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;
struct Files {
    memory: PathBuf,
    archive: PathBuf,
}
impl Files {
    fn new() -> Self {
        let name = format!("aiex-policy-{}", Uuid::new_v4());
        Self {
            memory: std::env::temp_dir().join(format!("{name}.jsonl")),
            archive: std::env::temp_dir().join(format!("{name}-archive.jsonl")),
        }
    }
    fn write(&self, rows: &[Value]) {
        std::fs::write(
            &self.memory,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        for path in [&self.memory, &self.archive] {
            let _ = std::fs::remove_file(path);
            let mut lock = path.as_os_str().to_os_string();
            lock.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(lock));
        }
    }
}
fn row(text: &str, vector: f64, importance: f64) -> Value {
    json!({"id":Uuid::new_v4(),"profile_id":"default","turn_id":TurnId::new(),"created_ms":1,"kind":"conversation","user_text":text,"assistant_text":"","source":"automatic","revision":1,"embedding":{"space":"fixture-l2","values":[vector,0.0]},"legacy_metadata":{"importance_score":importance,"access_count":7}})
}
struct Encoder(bool);
#[async_trait]
impl EmbeddingPort for Encoder {
    fn space(&self) -> &str {
        "fixture-l2"
    }
    async fn embed(&self, _: &str) -> Result<Vec<f32>, AppError> {
        if self.0 {
            Err(AppError::unavailable("mock failure"))
        } else {
            Ok(vec![1., 0.])
        }
    }
}
async fn store(files: &Files, exclude: usize) -> MemoryStore {
    MemoryStore::open(&files.memory)
        .await
        .unwrap()
        .with_recall_policy(MemoryRecallPolicy::Legacy, exclude)
        .unwrap()
}
#[tokio::test]
async fn raw_l2_threshold_and_candidate_first_ranking_are_preserved() {
    let files = Files::new();
    files.write(&[
        row("near low", 1.1, 0.2),
        row("near high", 1.2, 0.8),
        row("later highest", 1.3, 1.),
        row("outside threshold", 1.8, 1.),
    ]);
    let memory = store(&files, 0)
        .await
        .with_embedder(Arc::new(Encoder(false)));
    let result = memory
        .recall_kind(None, "unrelated query", 2)
        .await
        .unwrap();
    assert!(result[0].content.contains("near high"));
    assert!(result[1].content.contains("near low"));
    let result = memory
        .recall_kind(None, "unrelated query", 10)
        .await
        .unwrap();
    assert_eq!(result.len(), 3);
    assert!(
        result
            .iter()
            .all(|m| !m.content.contains("outside threshold"))
    );
    // All vectors have cosine one; rejection proves this mode uses raw L2 magnitude.
}
#[tokio::test]
async fn vector_space_and_dimensions_must_match() {
    let files = Files::new();
    let mut wrong = row("wrong space", 1., 1.);
    wrong["embedding"]["space"] = json!("other");
    let mut dimensions = row("wrong dims", 1., 1.);
    dimensions["embedding"]["values"] = json!([1., 0., 0.]);
    files.write(&[row("valid", 1., 0.2), wrong, dimensions]);
    let memory = store(&files, 0)
        .await
        .with_embedder(Arc::new(Encoder(false)));
    let result = memory.recall_kind(None, "query", 10).await.unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].content.contains("valid"));
}
#[tokio::test]
async fn fallback_and_recent_session_exclusion_do_not_mutate_access_counts() {
    let files = Files::new();
    files.write(&[row("shared old", 1., 0.2)]);
    let original = std::fs::read(&files.memory).unwrap();
    let memory = store(&files, 0)
        .await
        .with_embedder(Arc::new(Encoder(true)));
    assert_eq!(
        memory.recall_kind(None, "shared", 5).await.unwrap().len(),
        1
    );
    assert_eq!(std::fs::read(&files.memory).unwrap(), original);
    drop(memory);
    let mut memory = store(&files, 1).await;
    memory
        .remember(TurnId::new(), "shared newest".into(), "reply".into())
        .await
        .unwrap();
    let result = memory.recall_kind(None, "shared", 5).await.unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].content.contains("shared old"));
    memory
        .manage(MemoryRequest::Remember {
            profile_id: "default".into(),
            text: "蓝色笔记".into(),
        })
        .await
        .unwrap();
    let after_note = memory.recall_kind(None, "shared", 5).await.unwrap();
    assert_eq!(after_note.len(), 1);
    assert!(after_note[0].content.contains("shared old"));
    memory.select_profile("other").await.unwrap();
    assert!(
        memory
            .recall_kind(None, "shared", 5)
            .await
            .unwrap()
            .is_empty()
    );
    drop(memory);
    let memory = store(&files, 1).await;
    assert_eq!(
        memory.recall_kind(None, "shared", 5).await.unwrap().len(),
        2
    );
}
#[tokio::test]
async fn confirmed_notes_reserve_context_slots_and_hybrid_remains_default() {
    let files = Files::new();
    files.write(&[row("shared automatic", 1., 1.)]);
    let mut memory = store(&files, 0).await;
    memory
        .manage(MemoryRequest::Remember {
            profile_id: "default".into(),
            text: "confirmed identity".into(),
        })
        .await
        .unwrap();
    let result = memory
        .recall_kinds(
            &[MemoryKind::Conversation, MemoryKind::Persona],
            "shared",
            1,
        )
        .await
        .unwrap();
    assert_eq!(result.len(), 1);
    assert!(result[0].content.contains("confirmed identity"));
    drop(memory);
    let mut memory = MemoryStore::open(&files.memory).await.unwrap();
    memory
        .remember(TurnId::new(), "shared recent".into(), "".into())
        .await
        .unwrap();
    assert!(
        memory.recall_kind(None, "shared recent", 1).await.unwrap()[0]
            .content
            .contains("shared recent")
    );
}
