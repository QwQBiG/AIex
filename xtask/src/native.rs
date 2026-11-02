use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use ai_ex_inference::{InferenceOutput, InferenceTask, InferenceWorker, WorkerLaunch};
use ai_ex_model_catalog::{
    DirectoryVerification, ModelCapability, ModelManifest, PreparationStatus, verify_directory,
};
use serde_json::{Value, json};

use crate::spec::{ProbeSpec, ProbeStep, read_json};

pub fn verify(manifest: &Path, directory: &Path) -> Result<(Value, bool), String> {
    let manifest: ModelManifest = read_json(manifest)?;
    let report = verify_directory(&manifest, directory).map_err(|error| error.to_string())?;
    let passed = report.status == PreparationStatus::Ready;
    Ok((
        json!({"verification": report, "inference_evaluated": false,
        "distribution_materials_evaluated": false}),
        passed,
    ))
}

pub async fn probe(path: &Path) -> Result<(Value, bool), String> {
    let spec: ProbeSpec = read_json(path)?;
    spec.validate()?;
    let (models, verifications) = verified_models(&spec)?;
    let verified = verifications
        .iter()
        .all(|r| r.status == PreparationStatus::Ready);
    let mut report = json!({
        "schema_version": 1, "status": "failed", "models": verifications,
        "steps": [], "device_access_evaluated": false, "product_integration_evaluated": false,
        "distribution_materials_evaluated": false,
    });
    if !verified {
        report["error"] = json!("model file verification failed; worker was not started");
        return Ok((report, false));
    }
    for step in &spec.steps {
        let model = models
            .get(&step.request.model_id)
            .ok_or("request references an undeclared model")?;
        if !model.capabilities.contains(&capability(&step.request.task)) {
            return Err("request capability is not declared by its model".into());
        }
    }
    let started = Instant::now();
    let launch = WorkerLaunch {
        executable: spec.executable,
        arguments: spec.arguments.into_iter().map(Into::into).collect(),
        environment: spec
            .environment
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        job_directory: spec.job_directory.clone(),
        ready_timeout: Duration::from_millis(spec.ready_timeout_ms),
    };
    let mut worker = match InferenceWorker::spawn(launch).await {
        Ok(worker) => worker,
        Err(error) => {
            report["error"] = json!(error);
            report["startup_ms"] = json!(started.elapsed().as_millis());
            return Ok((report, false));
        }
    };
    report["startup_ms"] = json!(started.elapsed().as_millis());
    report["process_id"] = json!(worker.process_id());
    let mut steps = Vec::new();
    let mut passed = true;
    for step in &spec.steps {
        let (result, success) = run_step(&mut worker, step, &spec.job_directory).await;
        steps.push(result);
        if !success {
            passed = false;
            break;
        }
    }
    report["steps"] = json!(steps);
    if let Err(error) = worker.shutdown().await {
        report["shutdown_error"] = json!(error);
        passed = false;
    }
    report["worker_reaped"] = json!(worker.process_id().is_none());
    report["worker_exit"] = json!(worker.exit_status().map(|status| json!({
        "success": status.success(), "code": status.code(),
        "status": status.to_string(), "forced": worker.was_force_terminated(),
    })));
    if worker
        .exit_status()
        .is_some_and(|status| !status.success() && !worker.was_force_terminated())
    {
        report["exit_error"] = json!("native worker failed without a host-requested kill");
        passed = false;
    }
    passed &= worker.process_id().is_none();
    report["stderr_tail"] = json!(worker.diagnostics().await);
    report["elapsed_ms"] = json!(started.elapsed().as_millis());
    report["status"] = json!(if passed { "passed" } else { "failed" });
    Ok((report, passed))
}

fn verified_models(
    spec: &ProbeSpec,
) -> Result<(BTreeMap<String, ModelManifest>, Vec<DirectoryVerification>), String> {
    let mut models = BTreeMap::new();
    let mut ids = HashSet::new();
    let mut verifications = Vec::new();
    for input in &spec.models {
        let manifest: ModelManifest = read_json(&input.manifest)?;
        manifest.validate().map_err(|error| error.to_string())?;
        if !ids.insert(manifest.id.to_ascii_lowercase()) {
            return Err("duplicate probe model identifier".into());
        }
        verifications.push(
            verify_directory(&manifest, &input.directory).map_err(|error| error.to_string())?,
        );
        models.insert(manifest.id.clone(), manifest);
    }
    Ok((models, verifications))
}

fn capability(task: &InferenceTask) -> ModelCapability {
    use ai_ex_inference::VoiceSelection;
    match task {
        InferenceTask::Transcribe { .. } => ModelCapability::Transcription,
        InferenceTask::Embed { .. } => ModelCapability::Embedding,
        InferenceTask::Synthesize { voice, .. } => match voice {
            VoiceSelection::Preset { .. } => ModelCapability::SpeechPreset,
            VoiceSelection::Reference { .. } => ModelCapability::SpeechReference,
        },
    }
}

async fn run_step(worker: &mut InferenceWorker, step: &ProbeStep, job: &Path) -> (Value, bool) {
    let started = Instant::now();
    let request_started = tokio::time::Instant::now();
    let deadline = request_started + Duration::from_millis(step.timeout_ms);
    let mut report = json!({"identity": step.request.identity, "status": "failed"});
    if let Err(error) = worker.start_until(step.request.clone(), deadline).await {
        report["error"] = json!(error);
        report["elapsed_ms"] = json!(started.elapsed().as_millis());
        return (report, false);
    }
    if let Some(delay) = step.cancel_after_ms {
        // This timer verifies the host's cancellation path. Active native compute
        // needs a separately observed backend marker and CPU-time evidence.
        tokio::time::sleep_until(request_started + Duration::from_millis(delay)).await;
        let cancellation = Instant::now();
        match worker.cancel().await {
            Ok(outcome) => {
                report["status"] = json!("cancelled");
                report["cancellation_outcome"] = json!(format!("{outcome:?}"));
                report["cancellation_ms"] = json!(cancellation.elapsed().as_millis());
                report["active_compute_evaluated"] = json!(false);
                report["elapsed_ms"] = json!(started.elapsed().as_millis());
                return (
                    report,
                    outcome != ai_ex_inference::CancellationOutcome::Idle,
                );
            }
            Err(error) => report["error"] = json!(error),
        }
    } else {
        match tokio::time::timeout_at(deadline, worker.wait()).await {
            Ok(Ok(output)) => {
                if let InferenceOutput::Audio { file, sample_rate } = &output {
                    match crate::wav::inspect(job, &file.path, *sample_rate) {
                        Ok(format) => report["audio"] = format,
                        Err(error) => {
                            report["error"] = json!(error);
                            return (report, false);
                        }
                    }
                }
                report["status"] = json!("completed");
                report["output"] = json!(output);
                report["elapsed_ms"] = json!(started.elapsed().as_millis());
                return (report, true);
            }
            Ok(Err(error)) => report["error"] = json!(error),
            Err(_) => {
                report["error"] = json!("native request timed out");
                if let Err(error) = worker.cancel().await {
                    report["cancellation_error"] = json!(error);
                }
            }
        }
    }
    report["elapsed_ms"] = json!(started.elapsed().as_millis());
    (report, false)
}
