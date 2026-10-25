//! Exercise voice admission through the real service loop using synthetic audio only.
use super::run_duplex;
use crate::conversation_activity::ConversationActivity;
use crate::template_host::TemplateHost;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ai_ex_audio::{SpeechQueue, SpeechReceiver};
use ai_ex_core::{AvatarPort, LanguageModelPort, ModelRequest, Runtime, SpeechPort, spawn_runtime};
use ai_ex_domain::{
    AppError, AutomationControlMode, AutomationPauseReason, AutomationRequest,
    AutomationResumeEligibility, AutomationSessionPhase, AutomationSnapshot, AutomationStepOutcome,
    ErrorKind, Role, TurnId,
};
use ai_ex_duplex::{
    AudioFrame, AudioSourcePort, DuplexController, EnergyVad, TranscriberPort, Utterance, VadConfig,
};
use ai_ex_memory::MemoryStore;
use ai_ex_observability::EventHub;
use ai_ex_safety::{SafetyGate, SafetyPolicy};
use async_trait::async_trait;
use image::{ImageFormat, Rgba, RgbaImage};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

struct SyntheticAudio(mpsc::Receiver<AudioFrame>);

#[async_trait]
impl AudioSourcePort for SyntheticAudio {
    async fn next_frame(&mut self) -> Result<Option<AudioFrame>, AppError> {
        Ok(self.0.recv().await)
    }
}

struct Transcription {
    utterance: Utterance,
    complete: oneshot::Sender<Result<String, AppError>>,
}

struct HeldTranscriber(mpsc::Sender<Transcription>);

#[async_trait]
impl TranscriberPort for HeldTranscriber {
    async fn transcribe(&mut self, utterance: Utterance) -> Result<String, AppError> {
        let (complete, result) = oneshot::channel();
        self.0
            .send(Transcription {
                utterance,
                complete,
            })
            .await
            .map_err(|_| AppError::unavailable("synthetic ASR observer closed"))?;
        result
            .await
            .map_err(|_| AppError::unavailable("synthetic ASR release closed"))?
    }
}

struct Generation {
    request: ModelRequest,
    tokens: mpsc::Sender<Result<String, AppError>>,
}

struct HeldModel(mpsc::Sender<Generation>);

#[async_trait]
impl LanguageModelPort for HeldModel {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError> {
        let (tokens, receiver) = mpsc::channel(2);
        self.0
            .send(Generation { request, tokens })
            .await
            .map_err(|_| AppError::unavailable("synthetic model observer closed"))?;
        Ok(receiver)
    }

    async fn cancel(&mut self, _turn_id: TurnId) -> Result<(), AppError> {
        Ok(())
    }
}

struct SilentSpeech;

#[async_trait]
impl SpeechPort for SilentSpeech {
    async fn enqueue(&mut self, _turn_id: TurnId, _sentence: String) -> Result<(), AppError> {
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

struct SilentAvatar;

#[async_trait]
impl AvatarPort for SilentAvatar {
    async fn set_speaking(&mut self, _speaking: bool) -> Result<(), AppError> {
        Ok(())
    }

    async fn set_neutral(&mut self) -> Result<(), AppError> {
        Ok(())
    }
}

struct Fixture {
    directory: PathBuf,
    runtime: ConversationActivity,
    host: TemplateHost,
    models: mpsc::Receiver<Generation>,
    events: EventHub,
    // Keep a healthy, idle output consumer without creating an audio device.
    _speech: SpeechQueue,
    _receiver: SpeechReceiver,
}

impl Fixture {
    async fn start() -> Self {
        let directory =
            std::env::temp_dir().join(format!("aiex-duplex-activity-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        fs::create_dir(directory.join("templates")).unwrap();
        fs::write(
            directory.join("profile.json"),
            serde_json::to_vec(&json!({
                "display_name": "Synthetic voice admission", "description": "No live input",
                "vlm_prompts": [], "default_templates": {"primary_target": "target.png"},
                "action_cooldowns": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut template = RgbaImage::new(20, 20);
        let mut frame = RgbaImage::from_pixel(24, 24, Rgba([0, 0, 0, 255]));
        for y in 0..20 {
            for x in 0..20 {
                let value = ((x * 17 + y * 31) % 251) as u8;
                let pixel = Rgba([value, value, value, 255]);
                template.put_pixel(x, y, pixel);
                frame.put_pixel(x + 2, y + 2, pixel);
            }
        }
        template
            .save_with_format(directory.join("templates/target.png"), ImageFormat::Png)
            .unwrap();
        frame
            .save_with_format(directory.join("frame.png"), ImageFormat::Png)
            .unwrap();
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1, "profile": "profile.json", "target": "primary_target",
                "mode": "click_repeat", "desktop": {"left": 0, "top": 0, "width": 100, "height": 100},
                "interval_ms": 1000, "threshold": 0.95, "max_work": 1000000,
                "frames": [
                    {"image": "frame.png", "timestamp_ms": 0},
                    {"image": "frame.png", "timestamp_ms": 1000},
                    {"image": "frame.png", "timestamp_ms": 2000}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let (speech, receiver) = SpeechQueue::new(2).unwrap();
        let host = TemplateHost::new(
            Uuid::new_v4(),
            speech.activity(),
            Arc::new(SafetyGate::new(SafetyPolicy {
                automation_enabled: false,
                allowed_capabilities: BTreeSet::new(),
                allowed_targets: Vec::new(),
            })),
        )
        .unwrap();
        let (model, models) = mpsc::channel(2);
        let events = EventHub::new(64).unwrap();
        let handle = spawn_runtime(
            Runtime::new(
                HeldModel(model),
                SilentSpeech,
                SilentAvatar,
                MemoryStore::disabled(),
                events.clone(),
            ),
            2,
        )
        .unwrap();
        let fixture = Self {
            directory,
            runtime: ConversationActivity::new(handle, host.clone()),
            host,
            models,
            events,
            _speech: speech,
            _receiver: receiver,
        };
        let initial = fixture.status().await;
        fixture
            .host
            .execute(AutomationRequest::StartTemplate {
                instance_id: initial.instance_id,
                expected_revision: initial.revision,
                operation_id: Uuid::new_v4(),
                manifest: fixture
                    .directory
                    .join("manifest.json")
                    .to_str()
                    .unwrap()
                    .into(),
            })
            .await
            .unwrap();
        let active = fixture
            .until(|state| {
                state.session.as_ref().is_some_and(|session| {
                    session.phase == AutomationSessionPhase::Active && session.actions > 0
                })
            })
            .await;
        assert_eq!(active.session.unwrap().input_frames, 3);
        fixture
    }

    fn voice(
        &self,
    ) -> (
        mpsc::Sender<AudioFrame>,
        mpsc::Receiver<Transcription>,
        JoinHandle<Result<(), AppError>>,
    ) {
        let (frames, source) = mpsc::channel(2);
        let (asr, transcriptions) = mpsc::channel(1);
        let controller = DuplexController::new(
            EnergyVad::new(VadConfig {
                start_frames: 1,
                end_silence_frames: 1,
                ..VadConfig::default()
            })
            .unwrap(),
            HeldTranscriber(asr),
        );
        let task = tokio::spawn(run_duplex(
            SyntheticAudio(source),
            controller,
            self.runtime.clone(),
            self.host.clone(),
        ));
        (frames, transcriptions, task)
    }

    async fn status(&self) -> AutomationSnapshot {
        self.host.execute(AutomationRequest::Status).await.unwrap()
    }

    async fn until(&self, predicate: impl Fn(&AutomationSnapshot) -> bool) -> AutomationSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let current = self.status().await;
                if predicate(&current) {
                    return current;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("synthetic voice/automation state settles")
    }

    async fn busy(&self) -> AutomationSnapshot {
        self.until(|state| {
            state.session.as_ref().is_some_and(|session| {
                session.control_mode == AutomationControlMode::PausedForChat
                    && session.resume_eligibility == AutomationResumeEligibility::Busy
            })
        })
        .await
    }

    async fn idle(&self) -> AutomationSnapshot {
        let idle = self
            .until(|state| {
                state.session.as_ref().is_some_and(|session| {
                    session.resume_eligibility == AutomationResumeEligibility::Idle
                })
            })
            .await;
        assert_eq!(self.host.pending_chat_count().unwrap(), 0);
        idle
    }

    async fn close(self) {
        self.runtime.shutdown().await.unwrap();
        self.host.shutdown().await.unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Delete only names created by this fixture, never scan or recursively remove a tree.
        for name in [
            "templates/target.png",
            "frame.png",
            "profile.json",
            "manifest.json",
        ] {
            let _ = fs::remove_file(self.directory.join(name));
        }
        let _ = fs::remove_dir(self.directory.join("templates"));
        let _ = fs::remove_dir(&self.directory);
    }
}

fn audio(value: f32) -> AudioFrame {
    AudioFrame::new(vec![value; 160], 16_000, 1).unwrap()
}

fn resume(state: &AutomationSnapshot) -> AutomationRequest {
    AutomationRequest::Resume {
        instance_id: state.instance_id,
        session_id: state.session.as_ref().unwrap().session_id,
        expected_revision: state.revision,
    }
}

async fn observed<T>(receiver: &mut mpsc::Receiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("synthetic provider is reached")
        .expect("synthetic provider observer stays open")
}

#[tokio::test]
async fn speech_start_and_asr_to_model_handoff_keep_busy_and_preserve_manual_pause() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut fixture = Fixture::start().await;
        let (frames, mut transcriptions, voice) = fixture.voice();
        frames.send(audio(0.5)).await.unwrap();
        let started = fixture.busy().await;
        assert_eq!(
            started.session.as_ref().unwrap().pause_reason,
            Some(AutomationPauseReason::Chat)
        );
        assert!(
            transcriptions.try_recv().is_err(),
            "pause precedes even ASR admission"
        );
        assert_eq!(fixture.events.current().turns_started, 0);
        assert_eq!(
            fixture
                .host
                .execute(resume(&started))
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidTransition
        );

        frames.send(audio(0.0)).await.unwrap();
        let transcription = observed(&mut transcriptions).await;
        assert_eq!(transcription.utterance.sample_rate, 16_000);
        assert_eq!(transcription.utterance.channels, 1);
        assert_eq!(transcription.utterance.samples.len(), 320);
        let held = fixture.busy().await;
        let manual = fixture
            .host
            .execute(AutomationRequest::Pause {
                instance_id: held.instance_id,
                session_id: held.session.as_ref().unwrap().session_id,
                expected_revision: held.revision,
            })
            .await
            .unwrap();
        assert_eq!(manual.session.unwrap().chat_deadline_ms, None);
        transcription
            .complete
            .send(Ok("synthetic voice request".into()))
            .unwrap();
        let generation = observed(&mut fixture.models).await;
        assert!(generation.request.messages.iter().any(|message| {
            message.role == Role::User && message.content == "synthetic voice request"
        }));
        // EOF ends the audio loop, but cannot release the admitted model's reservation.
        drop(frames);
        voice.await.unwrap().unwrap();
        generation
            .tokens
            .send(Ok("Synthetic ".into()))
            .await
            .unwrap();
        let handed_off = fixture.busy().await;
        assert_eq!(fixture.host.pending_chat_count().unwrap(), 1);
        let session = handed_off.session.as_ref().unwrap();
        assert_eq!(session.pause_reason, Some(AutomationPauseReason::Manual));
        assert_eq!(session.chat_deadline_ms, None);
        assert_eq!(
            fixture
                .host
                .execute(resume(&handed_off))
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidTransition
        );
        generation.tokens.send(Ok("answer.".into())).await.unwrap();
        drop(generation);
        let idle = fixture.idle().await;
        assert_eq!(fixture.events.current().turns_completed, 1);
        let session = idle.session.as_ref().unwrap();
        assert_eq!(session.control_mode, AutomationControlMode::PausedForChat);
        assert_eq!(session.pause_reason, Some(AutomationPauseReason::Manual));
        assert_eq!(
            session.chat_deadline_ms, None,
            "the earlier chat cannot own manual pause"
        );
        let resumed = fixture.host.execute(resume(&idle)).await.unwrap();
        assert_eq!(
            resumed.session.unwrap().control_mode,
            AutomationControlMode::Running
        );
        fixture.close().await;
    })
    .await
    .expect("voice admission handoff finishes within five seconds");
}

#[tokio::test]
async fn stop_during_asr_stays_terminal_after_the_transcript_and_model_finish() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut fixture = Fixture::start().await;
        let (frames, mut transcriptions, voice) = fixture.voice();
        frames.send(audio(0.5)).await.unwrap();
        fixture.busy().await;
        frames.send(audio(0.0)).await.unwrap();
        let transcription = observed(&mut transcriptions).await;
        let held = fixture.busy().await;
        fixture
            .host
            .execute(AutomationRequest::Stop {
                instance_id: held.instance_id,
                session_id: held.session.as_ref().unwrap().session_id,
                expected_revision: held.revision,
            })
            .await
            .unwrap();
        let stopped = fixture
            .until(|state| {
                state.session.as_ref().unwrap().phase == AutomationSessionPhase::Finished
            })
            .await;
        assert_eq!(
            stopped.session.as_ref().unwrap().control_mode,
            AutomationControlMode::Stopped
        );
        assert_eq!(
            stopped.session.as_ref().unwrap().resume_eligibility,
            AutomationResumeEligibility::Busy
        );
        let actions_at_stop = stopped.session.as_ref().unwrap().actions;
        transcription
            .complete
            .send(Ok("finish voice after stop".into()))
            .unwrap();
        let generation = observed(&mut fixture.models).await;
        drop(frames);
        voice.await.unwrap().unwrap();
        let generating = fixture.status().await;
        assert_eq!(
            generating.session.as_ref().unwrap().control_mode,
            AutomationControlMode::Stopped
        );
        assert_eq!(
            generating.session.as_ref().unwrap().resume_eligibility,
            AutomationResumeEligibility::Busy
        );
        assert_eq!(
            fixture
                .host
                .execute(resume(&generating))
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidTransition
        );
        generation
            .tokens
            .send(Ok("Finished safely.".into()))
            .await
            .unwrap();
        drop(generation);
        let idle = fixture.idle().await;
        let session = idle.session.as_ref().unwrap();
        assert_eq!(fixture.events.current().turns_completed, 1);
        assert_eq!(session.phase, AutomationSessionPhase::Finished);
        assert_eq!(session.control_mode, AutomationControlMode::Stopped);
        assert_eq!(session.latest_outcome, Some(AutomationStepOutcome::Stopped));
        assert_eq!(session.actions, actions_at_stop);
        assert!(fixture.host.execute(resume(&idle)).await.is_err());
        fixture.close().await;
    })
    .await
    .expect("stopping pending voice finishes within five seconds");
}

#[tokio::test]
async fn empty_transcription_asr_error_and_source_eof_release_voice_admission() {
    tokio::time::timeout(Duration::from_secs(10), async {
        for ending in ["empty", "asr_error", "source_eof"] {
            let mut fixture = Fixture::start().await;
            let (frames, mut transcriptions, voice) = fixture.voice();
            frames.send(audio(0.5)).await.unwrap();
            fixture.busy().await;
            if ending != "source_eof" {
                frames.send(audio(0.0)).await.unwrap();
                let transcription = observed(&mut transcriptions).await;
                fixture.busy().await;
                let answer = if ending == "empty" {
                    Ok(" \n\t".into())
                } else {
                    Err(AppError::unavailable("injected ASR failure"))
                };
                transcription.complete.send(answer).unwrap();
                // An empty result must release while the input source remains open.
                fixture.idle().await;
            }
            drop(frames);
            let result = voice.await.unwrap();
            if ending == "asr_error" {
                assert_eq!(
                    result.unwrap_err(),
                    AppError::unavailable("injected ASR failure")
                );
            } else {
                result.unwrap();
            }
            let idle = fixture.idle().await;
            assert_eq!(fixture.events.current().turns_started, 0, "{ending}");
            assert!(
                fixture.models.try_recv().is_err(),
                "{ending} must not invoke a model"
            );
            assert!(
                transcriptions.try_recv().is_err(),
                "no additional transcription for {ending}"
            );
            assert_eq!(
                idle.session.as_ref().unwrap().control_mode,
                AutomationControlMode::PausedForChat
            );
            // The ordinary quiet deadline may remain; explicit idle Resume needs no 30s sleep.
            let resumed = fixture.host.execute(resume(&idle)).await.unwrap();
            assert_eq!(
                resumed.session.unwrap().control_mode,
                AutomationControlMode::Running
            );
            fixture.close().await;
        }
    })
    .await
    .expect("empty/error/EOF voice paths finish within ten seconds");
}
