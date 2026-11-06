#[path = "../src/backup_inspection_coordinator.rs"]
mod backup_inspection_coordinator;
#[allow(dead_code)]
#[path = "../src/backup_inspection_worker.rs"]
mod backup_inspection_worker;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use backup_inspection_coordinator::BackupInspector;
use backup_inspection_worker::Request;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn backup_inspection_real_service_worker_uses_only_selected_synthetic_file() {
    let directory = std::env::temp_dir().join(format!("backup-worker-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path: PathBuf = directory.join("private-path-marker.jsonl");
    let original = format!(
        "{}\n",
        json!({
            "schema_version":2,"record_type":"memory","id":Uuid::new_v4(),
            "profile_id":"synthetic","turn_id":Uuid::new_v4(),"created_ms":1,
            "user_text":"private-text-marker","assistant_text":"private-answer-marker"
        })
    );
    fs::write(&path, &original).unwrap();
    let inspector =
        BackupInspector::new(PathBuf::from(env!("CARGO_BIN_EXE_ai-ex-service"))).unwrap();
    let request = |profile: &str| Request {
        path: path.to_string_lossy().into_owned(),
        profile: profile.into(),
        comparison_space: None,
        legacy: false,
        candidate: None,
    };
    let summary = inspector
        .inspect(request("synthetic"), Duration::from_secs(12))
        .await
        .unwrap();
    assert_eq!(summary.memory_records, 1);
    assert_eq!(summary.matching_embeddings, None);
    let text = serde_json::to_string(&summary).unwrap();
    assert!(!text.contains("private-path-marker"));
    assert!(!text.contains("private-text-marker"));
    let error = inspector
        .inspect(request("other"), Duration::from_secs(12))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("private-path-marker"));
    assert!(!error.to_string().contains("private-text-marker"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    inspector.shutdown().await.unwrap();
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[tokio::test]
async fn legacy_backup_inspection_real_service_worker_is_bounded_and_read_only() {
    use std::io::Write;

    let directory = std::env::temp_dir().join(format!("legacy-worker-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let path = directory.join("private-path-marker.json.gz");
    let fixture = include_bytes!("../../ai-ex-migrate/tests/fixtures/legacy-memory.json");
    let encode = |bytes: &[u8]| {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    };
    let original = encode(fixture);
    fs::write(&path, &original).unwrap();
    let inspector =
        BackupInspector::new(PathBuf::from(env!("CARGO_BIN_EXE_ai-ex-service"))).unwrap();
    let request = || Request {
        path: path.to_string_lossy().into_owned(),
        profile: "synthetic".into(),
        comparison_space: None,
        legacy: true,
        candidate: None,
    };
    let (summary, digest) = inspector
        .start(request(), Duration::from_secs(12))
        .unwrap()
        .wait_legacy_digest()
        .await
        .unwrap();
    assert_eq!(summary.records, 1);
    assert_eq!(
        digest,
        ai_ex_migrate::legacy_memory::backup_digest(&original)
    );
    assert_eq!(summary.local_timestamp_records, 1);
    assert_eq!(summary.nonempty_embedding_records, 1);
    assert!(!serde_json::to_string(&summary).unwrap().contains("private"));
    assert_eq!(fs::read(&path).unwrap(), original);

    for invalid in [
        encode(
            std::str::from_utf8(fixture)
                .unwrap()
                .replace("7ae7e7b2", "0ae7e7b2")
                .as_bytes(),
        ),
        vec![0x1f, 0x8b, 0],
        encode(&vec![b' '; 32 * 1024 * 1024 + 1]),
    ] {
        fs::write(&path, &invalid).unwrap();
        let error = inspector
            .start(request(), Duration::from_secs(12))
            .unwrap()
            .wait_legacy()
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-path-marker"));
        assert_eq!(fs::read(&path).unwrap(), invalid);
    }
    inspector.shutdown().await.unwrap();
    fs::remove_file(path).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[tokio::test]
async fn legacy_candidate_uses_checked_bytes_and_never_overwrites() {
    use backup_inspection_coordinator::CandidateOutcome;
    use backup_inspection_worker::CandidateWrite;

    let directory = std::env::temp_dir().join(format!("candidate-worker-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let input = directory.join("synthetic.json");
    let output = directory.join("candidate.jsonl");
    let fixture = include_bytes!("../../ai-ex-migrate/tests/fixtures/legacy-memory.json");
    fs::write(&input, fixture).unwrap();
    let inspector =
        BackupInspector::new(PathBuf::from(env!("CARGO_BIN_EXE_ai-ex-service"))).unwrap();
    let request = || Request {
        path: input.to_string_lossy().into_owned(),
        profile: "synthetic".into(),
        comparison_space: None,
        legacy: true,
        candidate: Some(CandidateWrite {
            digest: ai_ex_migrate::legacy_memory::backup_digest(fixture),
            output: output.to_string_lossy().into_owned(),
            utc_offset: Some(480),
            embedding_space: Some("synthetic-space".into()),
        }),
    };
    fs::write(&input, b"changed").unwrap();
    assert!(matches!(
        inspector
            .start(request(), Duration::from_secs(12))
            .unwrap()
            .wait_candidate()
            .await
            .unwrap(),
        CandidateOutcome::Rejected
    ));
    assert!(!output.exists());
    fs::write(&input, fixture).unwrap();
    assert!(matches!(
        inspector
            .start(request(), Duration::from_secs(12))
            .unwrap()
            .wait_candidate()
            .await
            .unwrap(),
        CandidateOutcome::Written {
            temporary_retained: false
        }
    ));
    let original_output = fs::read(&output).unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&original_output).is_ok());
    assert!(matches!(
        inspector
            .start(request(), Duration::from_secs(12))
            .unwrap()
            .wait_candidate()
            .await
            .unwrap(),
        CandidateOutcome::Uncertain
    ));
    assert_eq!(fs::read(&output).unwrap(), original_output);
    assert_eq!(fs::read(&input).unwrap(), fixture);
    inspector.shutdown().await.unwrap();
    fs::remove_file(input).unwrap();
    fs::remove_file(output).unwrap();
    fs::remove_dir(directory).unwrap();
}

#[tokio::test]
async fn merge_candidate_real_worker_checks_both_copies_and_never_overwrites() {
    use backup_inspection_coordinator::{CandidateOutcome, MergePreviewOutcome};
    use backup_inspection_worker::{MergeRequest, MergeWrite};

    let directory = std::env::temp_dir().join(format!("merge-worker-{}", Uuid::new_v4()));
    fs::create_dir(&directory).unwrap();
    let base_path = directory.join("base.jsonl");
    let incoming_path = directory.join("incoming.jsonl");
    let output_path = directory.join("candidate.jsonl");
    let make_record = || {
        json!({
            "schema_version":2,"record_type":"memory","id":Uuid::new_v4(),
            "profile_id":"synthetic","turn_id":Uuid::new_v4(),"created_ms":1,
            "user_text":"private-text-marker","assistant_text":"",
            "unknown_extension":{"retained":true}
        })
        .to_string()
        .into_bytes()
    };
    let base = make_record();
    let incoming = make_record();
    fs::write(&base_path, &base).unwrap();
    fs::write(&incoming_path, &incoming).unwrap();
    let inspector =
        BackupInspector::new(PathBuf::from(env!("CARGO_BIN_EXE_ai-ex-service"))).unwrap();
    let request = |write| MergeRequest {
        base_path: base_path.to_str().unwrap().into(),
        incoming_path: incoming_path.to_str().unwrap().into(),
        profile: "synthetic".into(),
        write,
    };
    let MergePreviewOutcome::Ready {
        summary,
        base_digest,
        incoming_digest,
    } = inspector
        .start(request(None), Duration::from_secs(12))
        .unwrap()
        .wait_merge_preview()
        .await
        .unwrap()
    else {
        panic!("preview failed")
    };
    assert_eq!(
        (summary.base_memory_records, summary.incoming_memory_records),
        (1, 1)
    );
    assert!(!serde_json::to_string(&summary).unwrap().contains("private"));
    let write = || {
        Some(MergeWrite {
            base_digest,
            incoming_digest,
            output: output_path.to_str().unwrap().into(),
        })
    };
    for path in [&base_path, &incoming_path] {
        let original = fs::read(path).unwrap();
        fs::write(path, make_record()).unwrap();
        assert!(matches!(
            inspector
                .start(request(write()), Duration::from_secs(12))
                .unwrap()
                .wait_candidate()
                .await
                .unwrap(),
            CandidateOutcome::Rejected
        ));
        assert!(!output_path.exists());
        fs::write(path, original).unwrap();
    }
    assert!(matches!(
        inspector
            .start(request(write()), Duration::from_secs(12))
            .unwrap()
            .wait_candidate()
            .await
            .unwrap(),
        CandidateOutcome::Written {
            temporary_retained: false
        }
    ));
    let mut expected = base.clone();
    expected.push(b'\n');
    expected.extend_from_slice(&incoming);
    assert_eq!(fs::read(&output_path).unwrap(), expected);
    assert!(matches!(
        inspector
            .start(request(write()), Duration::from_secs(12))
            .unwrap()
            .wait_candidate()
            .await
            .unwrap(),
        CandidateOutcome::Uncertain
    ));
    assert_eq!(fs::read(&output_path).unwrap(), expected);
    assert_eq!(fs::read(&base_path).unwrap(), base);
    assert_eq!(fs::read(&incoming_path).unwrap(), incoming);
    fs::write(&incoming_path, &base).unwrap();
    assert!(matches!(
        inspector
            .start(request(None), Duration::from_secs(12))
            .unwrap()
            .wait_merge_preview()
            .await
            .unwrap(),
        MergePreviewOutcome::Conflict(ai_ex_control::MemoryMergeConflict::MemoryIdentityOverlap)
    ));
    fs::write(&incoming_path, &incoming).unwrap();
    inspector.shutdown().await.unwrap();
    for path in [&base_path, &incoming_path, &output_path] {
        fs::remove_file(path).unwrap();
    }
    fs::remove_dir(directory).unwrap();
}
