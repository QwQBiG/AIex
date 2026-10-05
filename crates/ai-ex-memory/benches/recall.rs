use std::collections::BTreeSet;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ai_ex_core::MemoryPort;
use ai_ex_domain::{MemoryEntry, MemoryKind, MemorySource, Message, Role, TurnId};
use ai_ex_memory::MemoryStore;
use uuid::Uuid;

const QUERY: &str = "草莓蛋糕 周末计划";
const LIMIT: usize = 5;

// Frozen 92a15d7 lexical context retrieval. Each document rebuilds both term sets,
// then every match is sorted; no model, network, disk I/O or synthetic vector work.
fn frozen_terms(text: &str) -> BTreeSet<String> {
    let lowercase = text.to_lowercase();
    let mut result: BTreeSet<String> = lowercase
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_owned)
        .collect();
    for character in lowercase
        .chars()
        .filter(|character| !character.is_whitespace())
    {
        if !character.is_ascii_punctuation() {
            result.insert(character.to_string());
        }
    }
    result
}

fn frozen_recall(records: &[MemoryEntry], query: &str, limit: usize) -> Vec<Message> {
    let kinds = [
        MemoryKind::Conversation,
        MemoryKind::Persona,
        MemoryKind::Viewer,
    ];
    let mut ranked: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.profile_id == "default")
        .filter(|(_, record)| kinds.contains(&record.kind))
        .filter_map(|(index, record)| {
            let confirmed = record.source != MemorySource::Automatic;
            let score = if confirmed {
                0
            } else {
                let query_terms = frozen_terms(query);
                let document_terms =
                    frozen_terms(&format!("{} {}", record.user_text, record.assistant_text));
                query_terms.intersection(&document_terms).count()
            };
            (confirmed || score > 0).then_some((
                (
                    confirmed,
                    score,
                    record.updated_ms.unwrap_or(record.created_ms),
                    index,
                ),
                record,
            ))
        })
        .collect();
    ranked.sort_by_key(|item| std::cmp::Reverse(item.0));
    ranked
        .into_iter()
        .take(limit)
        .map(|(_, record)| {
            Message::new(
                Role::System,
                format!(
                    "Relevant context memory [{}; source={:?}] — User: {} Assistant: {}",
                    record.kind.as_str(),
                    record.source,
                    record.user_text,
                    record.assistant_text
                ),
            )
        })
        .collect()
}

async fn frozen_open(path: &Path) -> Vec<MemoryEntry> {
    tokio::fs::read_to_string(path)
        .await
        .expect("synthetic input readable")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("synthetic record valid"))
        .collect()
}

fn fixture(size: usize) -> Vec<MemoryEntry> {
    let subjects = [
        "草莓蛋糕",
        "山间徒步",
        "读书笔记",
        "周末计划",
        "coffee meeting",
    ];
    (0..size)
        .map(|index| MemoryEntry {
            id: Uuid::new_v4(),
            profile_id: if index % 7 == 0 { "other" } else { "default" }.to_owned(),
            turn_id: TurnId::new(),
            created_ms: (index / 3) as u128, // Equal timestamps exercise index tie-breaking.
            updated_ms: (index % 13 == 0).then_some((size + index) as u128),
            kind: if index % 11 == 0 {
                MemoryKind::LiveEvent
            } else {
                MemoryKind::Conversation
            },
            user_text: format!(
                "{} 合成记忆编号{index} 周末交流",
                subjects[index % subjects.len()]
            ),
            assistant_text: "这是一条隔离测试内容，仅用于检索性能比较".to_owned(),
            source: MemorySource::Automatic,
            revision: 1,
        })
        .collect()
}

fn median<T>(mut run: impl FnMut() -> T) -> Duration {
    black_box(run());
    let mut samples = Vec::with_capacity(7);
    for _ in 0..7 {
        let start = Instant::now();
        let output = black_box(run());
        samples.push(start.elapsed());
        drop(output); // Exclude destruction for both old and new loading/recall.
    }
    samples.sort_unstable();
    samples[3]
}

struct FixtureFile(PathBuf);

impl Drop for FixtureFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn contents(messages: &[Message]) -> Vec<&str> {
    messages
        .iter()
        .map(|message| message.content.as_str())
        .collect()
}

fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("single-thread runtime starts");
    let verification_only = std::env::args().any(|argument| argument == "--test");
    println!(
        "memory_recall_benchmark version={} os={} arch={} samples=7 warmups=1 limit={LIMIT} model=none",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "Synthetic lexical retrieval only; warm filesystem cache; startup includes JSONL parsing and new term-cache construction, excludes destruction."
    );
    println!(
        "records,old_open_median_us,cached_open_median_us,old_recall_median_us,cached_recall_median_us,recall_speedup"
    );
    let sizes: &[usize] = if verification_only {
        &[100]
    } else {
        &[100, 1_000, 10_000]
    };
    for &size in sizes {
        let records = fixture(size);
        let file = FixtureFile(
            std::env::temp_dir().join(format!("ai-ex-recall-bench-{}.jsonl", Uuid::new_v4())),
        );
        let input = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&file.0, input).expect("synthetic memory fixture written");
        let store = runtime
            .block_on(MemoryStore::open(&file.0))
            .expect("cached store opens");
        let expected = frozen_recall(&records, QUERY, LIMIT);
        let actual = runtime
            .block_on(store.recall_for_context(QUERY, LIMIT))
            .expect("cached recall succeeds");
        assert_eq!(
            contents(&actual),
            contents(&expected),
            "exact ranked result parity at {size} records"
        );
        assert_eq!(actual.len(), LIMIT);
        if verification_only {
            println!(
                "{size} records: exact ranked result parity passed (timings skipped in test mode)"
            );
            continue;
        }
        let old_open = median(|| runtime.block_on(frozen_open(black_box(&file.0))));
        let cached_open = median(|| {
            runtime
                .block_on(MemoryStore::open(black_box(&file.0)))
                .unwrap()
        });
        let old_recall = median(|| frozen_recall(black_box(&records), black_box(QUERY), LIMIT));
        let cached_recall = median(|| {
            runtime
                .block_on(store.recall_for_context(black_box(QUERY), LIMIT))
                .unwrap()
        });
        println!(
            "{size},{:.3},{:.3},{:.3},{:.3},{:.2}",
            old_open.as_secs_f64() * 1e6,
            cached_open.as_secs_f64() * 1e6,
            old_recall.as_secs_f64() * 1e6,
            cached_recall.as_secs_f64() * 1e6,
            old_recall.as_secs_f64() / cached_recall.as_secs_f64()
        );
    }
}
