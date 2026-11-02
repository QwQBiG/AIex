use ai_ex_audio::{AudioPlayer, PreviewSpeech, SpeechJob, SpeechQueue};
use ai_ex_config::native::ModelSelection;
use ai_ex_domain::{NativeOperationPhase, NativeReply, SystemEvent};
use ai_ex_inference::RequestIdentity;
use ai_ex_native_models::{NativeAudio, NativeSynthesizer};
use ai_ex_observability::EventHub;

use super::*;

#[derive(Clone)]
pub(super) struct Resources {
    queue: SpeechQueue,
    player: AudioPlayer,
    events: EventHub,
}

#[derive(Clone)]
pub(super) struct Active {
    pub id: uuid::Uuid,
    pub selection: ModelSelection,
    job: SpeechJob,
}

impl NativeRuntime {
    pub(crate) fn check_preview_generation(
        &self,
        expected_generation: u64,
    ) -> Result<(), AppError> {
        let state = self.lock()?;
        if self.closed.load(Ordering::Acquire)
            || expected_generation == u64::MAX
            || state.preview_generation != expected_generation
        {
            return Err(stale_preview());
        }
        Ok(())
    }

    pub(crate) fn attach_preview(&self, queue: SpeechQueue, player: AudioPlayer, events: EventHub) {
        *self
            .preview
            .lock()
            .expect("preview resources are not poisoned") = Some(Resources {
            queue,
            player,
            events,
        });
    }

    pub(super) async fn start_preview(
        self: &Arc<Self>,
        voice_id: String,
        text: String,
        expected_generation: u64,
    ) -> Result<NativeReply, AppError> {
        let _mutation = self.mutation.lock().await;
        self.ensure_dialogue_idle().await?;
        let generation = {
            let mut state = self.lock()?;
            if expected_generation == u64::MAX || state.preview_generation != expected_generation {
                return Err(stale_preview());
            }
            state.preview_generation += 1;
            state.preview_generation
        };
        self.stop_active_preview().await?;
        if !self.lock()?.configured {
            return Err(AppError::unavailable("请先创建原生设置"));
        }
        let config = self.config()?;
        if !config.devices.playback_enabled {
            return Err(AppError::unavailable("请先启用声音播放，再试听所选声音"));
        }
        let voice = config
            .voices
            .into_iter()
            .find(|voice| voice.id == voice_id)
            .ok_or_else(|| AppError::configuration("试听声音不存在"))?;
        let resources = self
            .preview
            .lock()
            .map_err(|_| AppError::unavailable("试听上下文不可用"))?
            .clone()
            .ok_or_else(|| AppError::unavailable("当前服务没有附加声音播放队列"))?;
        let pool = self.worker_pool()?;
        // Stop may overtake this request while its previous preview is being
        // reaped. Admission and publishing the active task share the state lock.
        let mut state = self.lock()?;
        if state.preview_generation != generation || self.closed.load(Ordering::Acquire) {
            return Err(stale_preview());
        }
        let guard = resources.queue.preview_job(text)?;
        let job = guard.job();
        if !resources.events.publish_if(
            SystemEvent::SpeechPreviewStarted {
                turn_id: job.turn_id,
            },
            || resources.queue.is_generation_current(job.generation()),
        ) {
            return Err(AppError::invalid_transition("试听已被更新的对话取代"));
        }
        let synthesizer = match NativeSynthesizer::new(pool, voice.clone()) {
            Ok(synthesizer) => synthesizer,
            Err(error) => {
                stop_generation(&resources, job);
                return Err(error);
            }
        };
        let context = match self
            .operations
            .reserve("voice-preview", NativeOperationPhase::Running)
        {
            Ok(context) => context,
            Err(error) => {
                stop_generation(&resources, job);
                return Err(error);
            }
        };
        let id = context.id;
        state.preview = Some(Active {
            id,
            selection: voice.model.clone(),
            job: job.clone(),
        });
        let runtime = self.clone();
        let task_context = context.clone();
        self.operations.launch(context, async move {
            let result = play(&task_context, &resources, &guard, &synthesizer).await;
            stop_generation(&resources, guard.job());
            drop(guard);
            if let Ok(mut state) = runtime.lock()
                && state.preview.as_ref().is_some_and(|active| active.id == id)
            {
                state.preview = None;
            }
            result
        })?;
        Ok(NativeReply::Operation(
            self.operations
                .get(id)?
                .expect("preview operation was reserved"),
        ))
    }

    pub(super) async fn stop_preview(&self) -> Result<(), AppError> {
        {
            let mut state = self.lock()?;
            state.preview_generation = state.preview_generation.saturating_add(1);
        }
        self.stop_active_preview().await
    }

    async fn stop_active_preview(&self) -> Result<(), AppError> {
        let active = self.lock()?.preview.clone();
        let Some(active) = active else {
            return Ok(());
        };
        if let Some(resources) = self
            .preview
            .lock()
            .map_err(|_| AppError::unavailable("试听上下文不可用"))?
            .clone()
        {
            stop_generation(&resources, &active.job);
        }
        self.operations.cancel(active.id)?;
        self.operations.wait(active.id).await
    }
}

fn stale_preview() -> AppError {
    AppError::invalid_transition("试听状态已改变，请刷新后重新点击试听")
}

fn stop_generation(resources: &Resources, job: &SpeechJob) {
    if resources.queue.stop_preview_generation(job.generation()) {
        let next = job.generation().wrapping_add(1);
        resources.events.publish_if(
            SystemEvent::SpeechPlayback {
                playback: job.stopped_snapshot(),
            },
            || resources.queue.is_generation_current(next),
        );
    }
}

async fn play(
    context: &operations::Context,
    resources: &Resources,
    guard: &PreviewSpeech,
    synthesizer: &NativeSynthesizer,
) -> Result<(), AppError> {
    let job = guard.job();
    let session_id = resources
        .events
        .current()
        .instance_id
        .ok_or_else(|| AppError::unavailable("声音事件实例不存在"))?;
    let identity = RequestIdentity {
        session_id,
        request_id: job.sentence_id,
        generation: job.generation(),
    };
    enum Generated {
        Audio(Result<NativeAudio, AppError>),
        Cancelled,
    }
    let generated = {
        let synthesis = synthesizer.synthesize(identity, &job.text);
        tokio::pin!(synthesis);
        tokio::select! {
            result = &mut synthesis => Generated::Audio(result),
            _ = context.cancelled() => Generated::Cancelled,
            _ = resources.player.wait_for_cancellation(job) => Generated::Cancelled,
        }
    };
    let audio = match generated {
        Generated::Audio(audio) => audio?,
        Generated::Cancelled => {
            context.cancel()?;
            synthesizer.cancel(identity).await?;
            return Err(AppError::invalid_transition("试听已停止"));
        }
    };
    context.check()?;
    let events = resources.events.clone();
    let queue = resources.queue.clone();
    let generation = job.generation();
    let playback = resources
        .player
        .play_wav_observed(job, audio.bytes, move |snapshot| {
            events.publish_if(SystemEvent::SpeechPlayback { playback: snapshot }, || {
                queue.is_generation_current(generation)
            });
        });
    tokio::pin!(playback);
    tokio::select! {
        result = &mut playback => result?,
        _ = context.cancelled() => {
            stop_generation(resources, job);
            playback.await?;
            return Err(AppError::invalid_transition("试听已停止"));
        }
        _ = resources.player.wait_for_cancellation(job) => {
            context.cancel()?;
            playback.await?;
            return Err(AppError::invalid_transition("试听已被新的对话取代"));
        }
    }
    if !resources.queue.is_generation_current(generation) {
        context.cancel()?;
    }
    context.check()
}
