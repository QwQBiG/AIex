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
async fn cancelling_cold_startup_explicitly_reaps_before_ready_deadline() {
    let job = Job::new();
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let launch = WorkerLaunch {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_inference-test-worker")),
        arguments: vec!["slow-ready".into()],
        environment: BTreeMap::new(),
        job_directory: job.0.clone(),
        ready_timeout: Duration::from_secs(60),
    };
    let task = tokio::spawn(InferenceWorker::spawn_cancellable(launch, receiver));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let began = Instant::now();
    cancel.send(true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    let Err(error) = result else {
        panic!("startup must have been cancelled");
    };
    assert!(began.elapsed() < Duration::from_secs(3));
    assert!(error.message.contains("startup cancelled"));
    assert!(error.message.contains("exit_status"));
    assert!(error.message.contains("\"forced\":true"));
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
    assert!(worker.exit_status().unwrap().success());
    assert!(!worker.was_force_terminated());
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
    assert!(worker.was_force_terminated());
    assert!(!worker.exit_status().unwrap().success());
    // The nonzero status of this expected cancellation is not a normal-close error.
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn stalled_reader_send_deadline_reaps_worker_without_reusing_partial_frame() {
    let job = Job::new();
    let mut worker = job.worker("ready-no-read").await.unwrap();
    let mut large = request();
    large.task = InferenceTask::Embed {
        text: "x".repeat(128 * 1024),
        space: "test-space".into(),
    };
    let started = Instant::now();
    let error = worker
        .start_until(
            large,
            tokio::time::Instant::now() + Duration::from_millis(100),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("send timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(worker.process_id().is_none());
    assert!(worker.was_force_terminated());
    assert!(worker.start(request()).await.is_err());
    worker.shutdown().await.unwrap();
}

#[tokio::test]
async fn completed_request_with_failed_shutdown_preserves_exit_code_and_reports_error() {
    let job = Job::new();
    let mut worker = job.worker("shutdown-failure").await.unwrap();
    worker.start(request()).await.unwrap();
    worker.wait().await.unwrap();
    let error = worker.shutdown().await.unwrap_err();
    assert!(
        error
            .message
            .contains("exited unsuccessfully during shutdown")
    );
    assert!(worker.process_id().is_none());
    assert_eq!(worker.exit_status().unwrap().code(), Some(1));
    assert!(!worker.was_force_terminated());
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
    let error = job.worker("incompatible").await.err().unwrap();
    assert_eq!(error.kind, ai_ex_domain::ErrorKind::Protocol);
    assert!(
        error
            .message
            .starts_with("native worker handshake is incompatible")
    );
    let mut worker = job.worker("crash").await.unwrap();
    worker.start(request()).await.unwrap();
    assert!(worker.wait().await.is_err());
    worker.terminate().await.unwrap();
}

#[tokio::test]
async fn startup_failure_keeps_bounded_stderr_and_observed_exit_without_launch_secrets() {
    let job = Job::new();
    let secret = "private-config-and-environment-sentinel";
    let error = InferenceWorker::spawn(WorkerLaunch {
        executable: PathBuf::from(env!("CARGO_BIN_EXE_inference-test-worker")),
        arguments: vec!["exit".into(), secret.into()],
        environment: BTreeMap::from([("PRIVATE_WORKER_CONFIG".into(), secret.into())]),
        job_directory: job.0.clone(),
        ready_timeout: Duration::from_secs(5),
    })
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, ai_ex_domain::ErrorKind::Unavailable);
    assert!(
        error
            .message
            .starts_with("native worker closed its reply stream")
    );
    assert!(!error.message.contains(secret));
    let (_, diagnostic) = error
        .message
        .split_once("native startup diagnostics: ")
        .unwrap();
    let diagnostic: serde_json::Value = serde_json::from_str(diagnostic).unwrap();
    let stderr = diagnostic["stderr_tail"].as_str().unwrap();
    assert!(stderr.len() <= 8192);
    assert!(stderr.ends_with("NATIVE_LOAD_FAILED: fixture protobuf\n"));
    assert!(diagnostic["exit_status"].is_string());
    assert!(diagnostic["forced"].is_boolean());
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
