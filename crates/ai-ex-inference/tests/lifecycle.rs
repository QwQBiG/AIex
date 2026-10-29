#![cfg(feature = "test-worker")]
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ai_ex_inference::{
    AudioFile, CancellationOutcome, InferenceOutput, InferenceRequest, InferenceTask,
    InferenceWorker, RequestIdentity, WorkerLaunch,
};
use uuid::Uuid;

struct Job(PathBuf);

impl Job {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("inference 测试-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    async fn worker(&self, mode: &str) -> Result<InferenceWorker, ai_ex_domain::AppError> {
        self.worker_with_timeout(mode, Duration::from_secs(5)).await
    }

    async fn worker_with_timeout(
        &self,
        mode: &str,
        ready_timeout: Duration,
    ) -> Result<InferenceWorker, ai_ex_domain::AppError> {
        InferenceWorker::spawn(WorkerLaunch {
            executable: PathBuf::from(env!("CARGO_BIN_EXE_inference-test-worker")),
            arguments: vec![mode.into()],
            environment: BTreeMap::new(),
            job_directory: self.0.clone(),
            ready_timeout,
        })
        .await
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn request() -> InferenceRequest {
    InferenceRequest {
        identity: RequestIdentity {
            session_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            generation: 3,
        },
        model_id: "whisper-small-int8".into(),
        task: InferenceTask::Transcribe {
            audio: AudioFile {
                path: "input.wav".into(),
            },
            language: "zh".into(),
        },
    }
}

#[tokio::test]
async fn completes_and_shuts_down_without_leaking_stderr() {
    let job = Job::new();
    let mut worker = job.worker("normal").await.unwrap();
    worker.start(request()).await.unwrap();
    assert_eq!(
        worker.wait().await.unwrap(),
        InferenceOutput::Transcript {
            text: "你好，世界".into()
        }
    );
    assert!(worker.diagnostics().await.len() <= 8192);
    worker.shutdown().await.unwrap();
    assert!(worker.process_id().is_none());
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_discards_racing_result_and_can_accept_another_request() {
    let job = Job::new();
    let mut worker = job.worker("late").await.unwrap();
    worker.start(request()).await.unwrap();
    assert_eq!(
        worker.cancel().await.unwrap(),
        CancellationOutcome::Acknowledged
    );
    assert!(worker.wait().await.is_err());
    worker.start(request()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), worker.wait())
            .await
            .is_err()
    );
    assert_eq!(
        worker.cancel().await.unwrap(),
        CancellationOutcome::Acknowledged
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn uncooperative_computation_is_killed_after_grace() {
    let job = Job::new();
    let mut worker = job.worker("uncooperative").await.unwrap();
    worker.start(request()).await.unwrap();
    let started = Instant::now();
    assert_eq!(
        worker.cancel().await.unwrap(),
        CancellationOutcome::Terminated
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(worker.process_id().is_none());
    assert!(worker.start(request()).await.is_err());
}

#[tokio::test]
async fn interrupted_wait_preserves_the_active_request() {
    let job = Job::new();
    let mut worker = job.worker("hold").await.unwrap();
    worker.start(request()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), worker.wait())
            .await
            .is_err()
    );
    assert!(worker.start(request()).await.is_err());
    assert_eq!(
        worker.cancel().await.unwrap(),
        CancellationOutcome::Acknowledged
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_generation_is_rejected_and_worker_reaped() {
    let job = Job::new();
    let mut worker = job.worker("stale").await.unwrap();
    worker.start(request()).await.unwrap();
    assert!(worker.wait().await.is_err());
    assert!(worker.process_id().is_none());
}

#[tokio::test]
async fn vectors_from_another_model_space_are_rejected() {
    let job = Job::new();
    let mut worker = job.worker("wrong-space").await.unwrap();
    let mut embedding = request();
    embedding.task = InferenceTask::Embed {
        text: "记住这个空间".into(),
        space: "bge-small-fixed".into(),
    };
    worker.start(embedding).await.unwrap();
    assert!(worker.wait().await.is_err());
    assert!(worker.process_id().is_none());
}

#[tokio::test]
async fn crash_and_incompatible_handshake_report_errors() {
    let job = Job::new();
    assert!(job.worker("exit").await.is_err());
    assert!(job.worker("incompatible").await.is_err());
    let mut worker = job.worker("crash").await.unwrap();
    worker.start(request()).await.unwrap();
    assert!(worker.wait().await.is_err());
    worker.terminate().await.unwrap();
}

#[tokio::test]
async fn stuck_native_initialization_is_stopped_by_ready_timeout() {
    let job = Job::new();
    let started = Instant::now();
    let error = job
        .worker_with_timeout("slow-ready", Duration::from_millis(100))
        .await
        .err()
        .expect("stuck native loader must time out");
    assert!(error.message.contains("startup timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
}
