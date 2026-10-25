#![forbid(unsafe_code)]

#[cfg(any(feature = "native-playback", test))]
mod envelope;

#[cfg(feature = "native-playback")]
mod native;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ai_ex_core::SpeechPort;
use ai_ex_domain::{AppError, ComponentHealth, Emotion, SpeechPlaybackSnapshot, TurnId};
use ai_ex_stage::{StageAction, StageCapability, StageExecutor};
use ai_ex_text::clean_for_speech;
use async_trait::async_trait;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct AudioPlayer {
    generation: Arc<AtomicU64>,
}

impl AudioPlayer {
    #[cfg(feature = "native-playback")]
    pub async fn play_wav(&self, job: &SpeechJob, bytes: Vec<u8>) -> Result<(), AppError> {
        self.play_wav_observed(job, bytes, |_| {}).await
    }

    #[cfg(not(feature = "native-playback"))]
    pub async fn play_wav_observed<F>(
        &self,
        job: &SpeechJob,
        bytes: Vec<u8>,
        _observer: F,
    ) -> Result<(), AppError>
    where
        F: Fn(ai_ex_domain::SpeechPlaybackSnapshot) + Send + 'static,
    {
        self.play_wav(job, bytes).await
    }

    #[cfg(not(feature = "native-playback"))]
    pub async fn play_wav(&self, job: &SpeechJob, _bytes: Vec<u8>) -> Result<(), AppError> {
        if self.cancelled(job) {
            return Ok(());
        }
        Err(AppError::unavailable(
            "binary was built without the native-playback feature",
        ))
    }

    #[cfg(feature = "native-playback")]
    pub async fn health(&self) -> ComponentHealth {
        match tokio::task::spawn_blocking(rodio::OutputStreamBuilder::open_default_stream).await {
            Ok(Ok(_stream)) => ComponentHealth::ready("audio-output"),
            Ok(Err(error)) => ComponentHealth::unavailable("audio-output", error.to_string()),
            Err(error) => ComponentHealth::unavailable("audio-output", error.to_string()),
        }
    }

    fn cancelled(&self, job: &SpeechJob) -> bool {
        job.generation != self.generation.load(Ordering::Acquire)
    }

    pub async fn wait_for_cancellation(&self, job: &SpeechJob) {
        while !self.cancelled(job) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[cfg(not(feature = "native-playback"))]
    pub async fn health(&self) -> ComponentHealth {
        ComponentHealth::unavailable(
            "audio-output",
            "binary was built without the native-playback feature",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechJob {
    pub turn_id: TurnId,
    pub text: String,
    pub sentence_id: uuid::Uuid,
    pub emotion: Emotion,
    generation: u64,
}

impl SpeechJob {
    pub fn playback_snapshot(
        &self,
        position_ms: u64,
        duration_ms: u64,
        mouth_level: u16,
    ) -> SpeechPlaybackSnapshot {
        SpeechPlaybackSnapshot {
            turn_id: Some(self.turn_id),
            active: true,
            sentence_id: Some(self.sentence_id),
            text: self.text.clone(),
            emotion: Some(self.emotion),
            position_ms: position_ms.min(duration_ms),
            duration_ms,
            mouth_level: mouth_level.min(1000),
        }
    }
}

/// An authoritative point-in-time view of one queue and its single consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechActivitySnapshot {
    Idle,
    /// Pending includes queued jobs and sends waiting for channel capacity.
    Busy {
        pending: usize,
        active: bool,
    },
    /// Consumer loss or untrustworthy accounting must never be treated as idle.
    Unavailable,
}

#[derive(Debug)]
struct SpeechActivityState {
    pending: usize,
    active: bool,
    receiver_alive: bool,
    healthy: bool,
}

/// Clone before moving the queue into a stage router; snapshots hold no async lock.
#[derive(Debug, Clone)]
pub struct SpeechActivity {
    inner: Arc<Mutex<SpeechActivityState>>,
}

impl SpeechActivity {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SpeechActivityState {
                pending: 0,
                active: false,
                receiver_alive: true,
                healthy: true,
            })),
        }
    }

    pub fn snapshot(&self) -> SpeechActivitySnapshot {
        let Ok(state) = self.inner.lock() else {
            return SpeechActivitySnapshot::Unavailable;
        };
        if !state.healthy || !state.receiver_alive {
            SpeechActivitySnapshot::Unavailable
        } else if state.pending == 0 && !state.active {
            SpeechActivitySnapshot::Idle
        } else {
            SpeechActivitySnapshot::Busy {
                pending: state.pending,
                active: state.active,
            }
        }
    }

    fn reserve(&self) -> Result<SpeechReservation, AppError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| AppError::unavailable("speech activity accounting is unavailable"))?;
        if !state.receiver_alive {
            return Err(AppError::unavailable("audio queue consumer stopped"));
        }
        if !state.healthy {
            return Err(AppError::unavailable(
                "speech activity accounting is unavailable",
            ));
        }
        let Some(pending) = state.pending.checked_add(1) else {
            state.healthy = false;
            return Err(AppError::unavailable(
                "speech activity count exceeded its bounds",
            ));
        };
        state.pending = pending;
        Ok(SpeechReservation {
            activity: self.clone(),
            reserved: true,
        })
    }

    fn finish_active(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.active = false;
        }
    }

    fn receive_job(&self, active: bool) {
        if let Ok(mut state) = self.inner.lock() {
            match state.pending.checked_sub(1) {
                Some(pending) => state.pending = pending,
                None => state.healthy = false,
            }
            // Release pending and mark the delivered job active under the same lock.
            state.active = active;
        }
    }

    fn consumer_stopped(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.receiver_alive = false;
        }
    }
}

struct SpeechReservation {
    activity: SpeechActivity,
    reserved: bool,
}

impl SpeechReservation {
    fn sent(mut self) {
        // The receiver now owns the pending count; it can already have consumed it.
        self.reserved = false;
    }
}

impl Drop for SpeechReservation {
    fn drop(&mut self) {
        if self.reserved
            && let Ok(mut state) = self.activity.inner.lock()
        {
            match state.pending.checked_sub(1) {
                Some(pending) => state.pending = pending,
                None => state.healthy = false,
            }
        }
    }
}

pub struct SpeechQueue {
    sender: mpsc::Sender<SpeechJob>,
    generation: Arc<AtomicU64>,
    activity: SpeechActivity,
}

pub struct SpeechReceiver {
    receiver: mpsc::Receiver<SpeechJob>,
    generation: Arc<AtomicU64>,
    activity: SpeechActivity,
}

impl SpeechQueue {
    pub fn new(capacity: usize) -> Result<(Self, SpeechReceiver), AppError> {
        if capacity == 0 {
            return Err(AppError::configuration(
                "audio queue capacity must be positive",
            ));
        }
        let (sender, receiver) = mpsc::channel(capacity);
        let generation = Arc::new(AtomicU64::new(0));
        let activity = SpeechActivity::new();
        Ok((
            Self {
                sender,
                generation: Arc::clone(&generation),
                activity: activity.clone(),
            },
            SpeechReceiver {
                receiver,
                generation,
                activity,
            },
        ))
    }

    pub fn activity(&self) -> SpeechActivity {
        self.activity.clone()
    }

    pub fn health(&self) -> ComponentHealth {
        if self.sender.is_closed() {
            ComponentHealth::unavailable("audio-queue", "consumer stopped")
        } else {
            ComponentHealth::ready("audio-queue")
        }
    }
}

impl SpeechReceiver {
    pub fn player(&self) -> AudioPlayer {
        AudioPlayer {
            generation: Arc::clone(&self.generation),
        }
    }

    /// Calling receive again signals that the single consumer finished the prior job.
    /// Activity remains busy through synthesis, playback, and cancellation cleanup.
    pub async fn receive(&mut self) -> Option<SpeechJob> {
        self.activity.finish_active();
        while let Some(job) = self.receiver.recv().await {
            let current = job.generation == self.generation.load(Ordering::Acquire);
            self.activity.receive_job(current);
            if current {
                return Some(job);
            }
        }
        self.activity.consumer_stopped();
        None
    }
}

impl Drop for SpeechReceiver {
    fn drop(&mut self) {
        self.activity.consumer_stopped();
    }
}

#[async_trait]
impl SpeechPort for SpeechQueue {
    async fn enqueue(&mut self, turn_id: TurnId, sentence: String) -> Result<(), AppError> {
        self.enqueue_expressive(turn_id, sentence, Emotion::Neutral)
            .await
    }

    async fn enqueue_expressive(
        &mut self,
        turn_id: TurnId,
        sentence: String,
        emotion: Emotion,
    ) -> Result<(), AppError> {
        let sentence = clean_for_speech(&sentence);
        if sentence.is_empty() {
            return Err(AppError::configuration("speech sentence must not be empty"));
        }
        let job = SpeechJob {
            turn_id,
            text: sentence,
            sentence_id: uuid::Uuid::new_v4(),
            emotion,
            generation: self.generation.load(Ordering::Acquire),
        };
        let reservation = self.activity.reserve()?;
        self.sender
            .send(job)
            .await
            .map_err(|_| AppError::unavailable("audio queue consumer stopped"))?;
        reservation.sent();
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), AppError> {
        self.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[async_trait]
impl StageExecutor for SpeechQueue {
    fn capabilities(&self) -> BTreeSet<StageCapability> {
        BTreeSet::from([StageCapability::Interrupt, StageCapability::Speech])
    }

    async fn health(&self) -> ComponentHealth {
        SpeechQueue::health(self)
    }

    async fn execute(&mut self, action: StageAction) -> Result<(), AppError> {
        action.validate()?;
        match action {
            StageAction::Speak {
                turn_id,
                text,
                emotion,
                ..
            } => {
                SpeechPort::enqueue_expressive(
                    self,
                    turn_id,
                    text,
                    emotion.unwrap_or(Emotion::Neutral),
                )
                .await
            }
            StageAction::Stop => SpeechPort::interrupt(self).await,
            StageAction::Expression { .. }
            | StageAction::Mouth { .. }
            | StageAction::Subtitle { .. }
            | StageAction::Scene { .. }
            | StageAction::Hotkey { .. } => Err(AppError::configuration(
                "audio stage does not support this action",
            )),
        }
    }

    async fn interrupt(&mut self) -> Result<(), AppError> {
        SpeechPort::interrupt(self).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_wakes_pending_synthesis_without_cancelling_new_jobs() {
        let (mut queue, mut receiver) = SpeechQueue::new(2).unwrap();
        let player = receiver.player();
        queue
            .enqueue(TurnId::new(), "old".to_owned())
            .await
            .unwrap();
        let old = receiver.receive().await.unwrap();
        SpeechPort::interrupt(&mut queue).await.unwrap();
        queue
            .enqueue(TurnId::new(), "new".to_owned())
            .await
            .unwrap();
        let new = receiver.receive().await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            player.wait_for_cancellation(&old),
        )
        .await
        .unwrap();
        assert!(!player.cancelled(&new));
    }

    #[tokio::test]
    async fn stage_executor_routes_speech_and_interrupts() {
        let (mut queue, mut receiver) = SpeechQueue::new(2).expect("queue created");
        let capabilities = StageExecutor::capabilities(&queue);
        assert!(capabilities.contains(&StageCapability::Speech));
        StageExecutor::execute(
            &mut queue,
            StageAction::Speak {
                turn_id: TurnId::new(),
                text: "hello".to_owned(),
                interruptible: true,
                emotion: None,
            },
        )
        .await
        .expect("speech action queues");
        assert_eq!(receiver.receive().await.expect("speech job").text, "hello");
        StageExecutor::execute(&mut queue, StageAction::Stop)
            .await
            .expect("stop interrupts");
        let error = StageExecutor::execute(
            &mut queue,
            StageAction::Scene {
                scene: "main".to_owned(),
            },
        )
        .await
        .expect_err("audio stage rejects scene");
        assert!(error.to_string().contains("audio stage"));
    }
    #[tokio::test]
    async fn interruption_invalidates_queued_speech() {
        let (mut queue, mut receiver) = SpeechQueue::new(4).expect("queue created");
        queue
            .enqueue(TurnId::new(), "old".to_owned())
            .await
            .expect("queued");
        SpeechPort::interrupt(&mut queue)
            .await
            .expect("interrupted");
        queue
            .enqueue(TurnId::new(), "new".to_owned())
            .await
            .expect("queued");
        let job = receiver.receive().await.expect("new job available");
        assert_eq!(job.text, "new");
    }

    #[tokio::test]
    async fn interruption_cancels_a_job_already_received() {
        let (mut queue, mut receiver) = SpeechQueue::new(4).expect("queue created");
        let player = receiver.player();
        queue
            .enqueue(TurnId::new(), "old".to_owned())
            .await
            .expect("queued");
        let job = receiver.receive().await.expect("job received");
        SpeechPort::interrupt(&mut queue)
            .await
            .expect("interrupted");

        player
            .play_wav(&job, vec![1, 2, 3])
            .await
            .expect("cancelled playback is skipped");
    }
}

#[cfg(test)]
mod activity_tests {
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::task::Poll;
    use std::time::Duration;

    use super::*;

    async fn bounded<F: Future>(future: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .expect("speech activity workflow completed within its bound")
    }

    async fn assert_pending<F: Future + ?Sized>(mut future: Pin<&mut F>) {
        poll_fn(|context| {
            assert!(future.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test]
    async fn activity_covers_queue_processing_and_next_receive_boundary() {
        bounded(async {
            let (mut queue, mut receiver) = SpeechQueue::new(2).unwrap();
            let activity = queue.activity();
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Idle);

            queue.enqueue(TurnId::new(), "first".into()).await.unwrap();
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 1,
                    active: false,
                }
            );
            assert_eq!(receiver.receive().await.unwrap().text, "first");
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 0,
                    active: true,
                }
            );

            queue.enqueue(TurnId::new(), "second".into()).await.unwrap();
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 1,
                    active: true,
                }
            );
            assert_eq!(receiver.receive().await.unwrap().text, "second");
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 0,
                    active: true,
                }
            );

            let mut waiting = Box::pin(receiver.receive());
            assert_pending(waiting.as_mut()).await;
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Idle);
        })
        .await;
    }

    #[tokio::test]
    async fn dropping_blocked_send_releases_only_its_reservation() {
        bounded(async {
            let (mut queue, mut receiver) = SpeechQueue::new(1).unwrap();
            let activity = queue.activity();
            queue.enqueue(TurnId::new(), "queued".into()).await.unwrap();

            let mut blocked = Box::pin(queue.enqueue(TurnId::new(), "cancelled".into()));
            assert_pending(blocked.as_mut()).await;
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 2,
                    active: false,
                }
            );
            drop(blocked);
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 1,
                    active: false,
                }
            );
            assert_eq!(receiver.receive().await.unwrap().text, "queued");

            let mut waiting = Box::pin(receiver.receive());
            assert_pending(waiting.as_mut()).await;
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Idle);
        })
        .await;
    }

    #[tokio::test]
    async fn successful_blocked_send_transfers_reservation_to_receiver_once() {
        bounded(async {
            let (mut queue, mut receiver) = SpeechQueue::new(1).unwrap();
            let activity = queue.activity();
            queue.enqueue(TurnId::new(), "first".into()).await.unwrap();
            let mut blocked = Box::pin(queue.enqueue(TurnId::new(), "second".into()));
            assert_pending(blocked.as_mut()).await;
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 2,
                    active: false,
                }
            );

            assert_eq!(receiver.receive().await.unwrap().text, "first");
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 1,
                    active: true,
                }
            );
            blocked.await.unwrap();
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 1,
                    active: true,
                }
            );
            assert_eq!(receiver.receive().await.unwrap().text, "second");
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 0,
                    active: true,
                }
            );

            let mut waiting = Box::pin(receiver.receive());
            assert_pending(waiting.as_mut()).await;
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Idle);
        })
        .await;
    }

    #[tokio::test]
    async fn interruption_keeps_active_cleanup_busy_and_drains_stale_jobs() {
        bounded(async {
            let (mut queue, mut receiver) = SpeechQueue::new(3).unwrap();
            let activity = queue.activity();
            let player = receiver.player();
            queue
                .enqueue(TurnId::new(), "active old".into())
                .await
                .unwrap();
            queue
                .enqueue(TurnId::new(), "queued old".into())
                .await
                .unwrap();
            let old = receiver.receive().await.unwrap();
            SpeechPort::interrupt(&mut queue).await.unwrap();
            assert!(player.cancelled(&old));
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 1,
                    active: true,
                }
            );
            queue.enqueue(TurnId::new(), "fresh".into()).await.unwrap();
            let fresh = receiver.receive().await.unwrap();
            assert_eq!(fresh.text, "fresh");
            assert!(!player.cancelled(&fresh));
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 0,
                    active: true,
                }
            );

            let mut waiting = Box::pin(receiver.receive());
            assert_pending(waiting.as_mut()).await;
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Idle);
        })
        .await;
    }

    #[tokio::test]
    async fn consumer_loss_is_unavailable_and_rolls_back_failed_send() {
        bounded(async {
            let (mut queue, receiver) = SpeechQueue::new(1).unwrap();
            let activity = queue.activity();
            queue.enqueue(TurnId::new(), "queued".into()).await.unwrap();
            let mut blocked = Box::pin(queue.enqueue(TurnId::new(), "blocked".into()));
            assert_pending(blocked.as_mut()).await;
            drop(receiver);
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);
            assert!(blocked.await.is_err());
            assert_eq!(activity.inner.lock().unwrap().pending, 1);
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);
            assert!(queue.enqueue(TurnId::new(), "closed".into()).await.is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn consumer_exhaustion_clears_active_and_marks_source_unavailable() {
        bounded(async {
            let (mut queue, mut receiver) = SpeechQueue::new(1).unwrap();
            let activity = queue.activity();
            queue.enqueue(TurnId::new(), "last".into()).await.unwrap();
            assert_eq!(receiver.receive().await.unwrap().text, "last");
            drop(queue);
            assert_eq!(
                activity.snapshot(),
                SpeechActivitySnapshot::Busy {
                    pending: 0,
                    active: true,
                }
            );
            assert!(receiver.receive().await.is_none());
            assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);
        })
        .await;
    }

    #[tokio::test]
    async fn accounting_overflow_and_poison_cannot_report_idle() {
        let (mut queue, _receiver) = SpeechQueue::new(1).unwrap();
        let activity = queue.activity();
        activity.inner.lock().unwrap().pending = usize::MAX;
        assert!(
            queue
                .enqueue(TurnId::new(), "overflow".into())
                .await
                .is_err()
        );
        assert_eq!(activity.inner.lock().unwrap().pending, usize::MAX);
        assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);
        activity.inner.lock().unwrap().pending = 0;
        assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);

        let (mut queue, receiver) = SpeechQueue::new(1).unwrap();
        let activity = queue.activity();
        let poisoned = std::panic::catch_unwind(|| {
            let _guard = activity.inner.lock().unwrap();
            panic!("deliberately poison the accounting lock");
        });
        assert!(poisoned.is_err());
        assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);
        assert!(
            queue
                .enqueue(TurnId::new(), "poisoned".into())
                .await
                .is_err()
        );
        drop(receiver);
        assert_eq!(activity.snapshot(), SpeechActivitySnapshot::Unavailable);
    }
}
