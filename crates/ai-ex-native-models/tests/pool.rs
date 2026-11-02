#![cfg(feature = "test-worker")]
#![forbid(unsafe_code)]

#[path = "support/fixtures.rs"]
mod fixtures;

use ai_ex_config::native::VoicePreset;
use ai_ex_core::EmbeddingPort;
use ai_ex_inference::RequestIdentity;
use ai_ex_model_manager::PackageState;
use ai_ex_native_models::{NativeEmbedder, NativeSynthesizer};
use fixtures::Fixture;
use std::time::{Duration, Instant};
use uuid::Uuid;

fn identity() -> RequestIdentity {
    RequestIdentity {
        session_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        generation: 1,
    }
}
fn speech(fixture: &Fixture) -> NativeSynthesizer {
    let voice = VoicePreset {
        model: fixture.selection.clone(),
        ..VoicePreset::default()
    };
    NativeSynthesizer::new(fixture.pool.clone(), voice).unwrap()
}
async fn active(fixture: &Fixture, identity: RequestIdentity) {
    let path = fixture.marker("active.json").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(bytes) = std::fs::read(&path)
                && serde_json::from_slice::<RequestIdentity>(&bytes).ok() == Some(identity)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn imported_packages_hold_leases_until_worker_shutdown() {
    let fixture = Fixture::new(false, false).await;
    let embedder = NativeEmbedder::new(fixture.pool.clone(), fixture.selection.clone()).unwrap();
    fixture
        .pool
        .wait_ready(&fixture.selection, Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(embedder.embed("你好").await.unwrap().len(), 512);
    let directory = std::fs::read_dir(fixture.root.join("jobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let merged: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.join("package.json")).unwrap()).unwrap();
    assert!(
        merged["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|artifact| artifact["kind"] != "source_archive")
    );
    assert!(
        fixture
            .manager
            .snapshot()
            .unwrap()
            .iter()
            .all(|package| package.state == PackageState::InUse)
    );
    assert!(fixture.manager.remove("fixture-model").await.is_err());
    fixture.pool.unload(&fixture.selection).await.unwrap();
    assert!(
        fixture
            .manager
            .snapshot()
            .unwrap()
            .iter()
            .all(|package| package.state == PackageState::Ready)
    );
    let removal = fixture.manager.remove("fixture-model").await.unwrap();
    assert_eq!(
        fixture.manager.wait(removal).await.unwrap().phase,
        ai_ex_model_manager::OperationPhase::Completed
    );
    fixture.finish().await;
}

#[tokio::test]
async fn dropped_speech_is_cancelled_before_immediate_next_generation_recovers() {
    let fixture = Fixture::new(true, false).await;
    let synth = speech(&fixture);
    let first = identity();
    let pending = tokio::spawn({
        let synth = synth.clone();
        async move { synth.synthesize(first, "hold").await }
    });
    active(&fixture, first).await;
    pending.abort();
    let _aborted = pending.await;
    let began = Instant::now();
    synth.cancel(first).await.unwrap();
    assert!(began.elapsed() < Duration::from_secs(4));
    let next = RequestIdentity {
        generation: 2,
        ..first
    };
    let future = tokio::spawn({
        let synth = synth.clone();
        async move { synth.synthesize(next, "delay").await }
    });
    active(&fixture, next).await;
    synth.cancel(first).await.unwrap();
    let audio = tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(audio.sample_rate, 24000);
    assert_eq!(&audio.bytes[..4], b"RIFF");
    fixture.pool.shutdown().await.unwrap();
    assert!(fixture.pool.warmup(&fixture.selection).is_err());
    fixture.finish().await;
}

#[tokio::test]
async fn cancelled_cold_speech_does_not_submit_text_and_startup_reaps_on_close() {
    let fixture = Fixture::new(true, true).await;
    let synth = speech(&fixture);
    let first = identity();
    let pending = tokio::spawn({
        let synth = synth.clone();
        async move { synth.synthesize(first, "hold").await }
    });
    fixture.marker("loaded.pid").await;
    pending.abort();
    let _aborted = pending.await;
    synth.cancel(first).await.unwrap();
    let began = Instant::now();
    fixture.pool.shutdown().await.unwrap();
    assert!(began.elapsed() < Duration::from_secs(3));
    assert!(
        std::fs::read_dir(fixture.root.join("jobs"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(
        fixture
            .manager
            .snapshot()
            .unwrap()
            .iter()
            .all(|package| package.state == PackageState::Ready)
    );
    fixture.finish().await;
}
