#![forbid(unsafe_code)]

#[cfg(test)]
mod preview_tests;
mod speech;
mod timing;

pub use timing::{RuntimeTiming, TurnTiming};

use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use ai_ex_core::EventSink;
use ai_ex_domain::{AppError, ConversationState, Emotion, SystemEvent, TurnId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tokio::sync::{broadcast, watch};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    pub state: ConversationState,
    pub active_turn: Option<TurnId>,
    pub current_emotion: Option<Emotion>,
    pub turns_started: u64,
    pub turns_completed: u64,
    pub turns_interrupted: u64,
    pub sentences_ready: u64,
    pub faults: u64,
    pub last_fault: Option<String>,
    pub last_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub playback: ai_ex_domain::SpeechPlaybackSnapshot,
    #[serde(default)]
    pub speech_turn: Option<TurnId>,
    #[serde(default)]
    pub speech_cancelled: bool,
    /// Independent of dialogue state and event sequence; absent on older services.
    #[serde(default)]
    pub voice_input: ai_ex_domain::VoiceInputState,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            state: ConversationState::Idle,
            active_turn: None,
            current_emotion: None,
            turns_started: 0,
            turns_completed: 0,
            turns_interrupted: 0,
            sentences_ready: 0,
            faults: 0,
            last_fault: None,
            last_sequence: 0,
            instance_id: None,
            playback: ai_ex_domain::SpeechPlaybackSnapshot::default(),
            speech_turn: None,
            speech_cancelled: false,
            voice_input: ai_ex_domain::VoiceInputState::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub sequence: u64,
    pub event: SystemEvent,
}

#[derive(Clone)]
pub struct EventHub {
    events: broadcast::Sender<SequencedEvent>,
    snapshot: watch::Sender<RuntimeSnapshot>,
    sequence: Arc<AtomicU64>,
    history: Arc<Mutex<VecDeque<SequencedEvent>>>,
    timing: Arc<Mutex<timing::TimingTracker>>,
    instance_id: uuid::Uuid,
    capacity: usize,
}

impl EventHub {
    pub fn new(capacity: usize) -> Result<Self, AppError> {
        if capacity == 0 {
            return Err(AppError::configuration(
                "event hub capacity must be positive",
            ));
        }
        let (events, _receiver) = broadcast::channel(capacity);
        let instance_id = uuid::Uuid::new_v4();
        let (snapshot, _receiver) = watch::channel(RuntimeSnapshot {
            instance_id: Some(instance_id),
            ..Default::default()
        });
        Ok(Self {
            events,
            snapshot,
            sequence: Arc::new(AtomicU64::new(0)),
            history: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            timing: Arc::new(Mutex::new(timing::TimingTracker::default())),
            instance_id,
            capacity,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SequencedEvent> {
        self.events.subscribe()
    }

    pub fn watch(&self) -> watch::Receiver<RuntimeSnapshot> {
        self.snapshot.subscribe()
    }

    pub fn current(&self) -> RuntimeSnapshot {
        self.snapshot.borrow().clone()
    }

    /// Read timing and its event prefix under the same lock used to publish.
    pub fn runtime_timing(&self) -> RuntimeTiming {
        let _history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let timing = self
            .timing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        RuntimeTiming {
            instance_id: self.instance_id,
            sequence: self.sequence.load(Ordering::Acquire),
            turn: timing.current(),
        }
    }

    /// Publish an independently ordered activity observation without a new event variant.
    pub fn observe_voice_input(&self, state: ai_ex_domain::VoiceInputState) {
        self.snapshot.send_modify(|snapshot| {
            if state.revision > snapshot.voice_input.revision {
                snapshot.voice_input = state;
            }
        });
    }

    pub fn events_since(&self, after: u64, limit: usize) -> Vec<SequencedEvent> {
        let history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        history
            .iter()
            .filter(|item| item.sequence > after)
            .take(limit.min(self.capacity))
            .cloned()
            .collect()
    }

    fn update(&self, event: &SystemEvent, sequence: u64) {
        self.snapshot.send_modify(|snapshot| {
            snapshot.last_sequence = sequence;
            snapshot.observe_speech_event(event);
            if matches!(
                event,
                SystemEvent::TurnStarted { .. }
                    | SystemEvent::ProactiveStarted { .. }
                    | SystemEvent::ModelChunk { .. }
                    | SystemEvent::SpeechPlayback { .. }
                    | SystemEvent::TurnFinished { .. }
                    | SystemEvent::SpeechCancelled
                    | SystemEvent::SpeechPreviewStarted { .. }
                    | SystemEvent::TurnInterrupted { .. }
                    | SystemEvent::Fault { .. }
                    | SystemEvent::StateChanged {
                        to: ConversationState::Stopped | ConversationState::Failed,
                        ..
                    }
            ) {
                self.timing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .observe(event, snapshot, Instant::now());
            }
            match event {
                SystemEvent::TurnStarted { turn_id, .. }
                | SystemEvent::ProactiveStarted { turn_id } => {
                    snapshot.active_turn = Some(*turn_id);
                    snapshot.turns_started += 1;
                }
                SystemEvent::SentenceReady { .. } => snapshot.sentences_ready += 1,
                SystemEvent::SpeechPlayback { .. }
                | SystemEvent::SpeechPreviewStarted { .. }
                | SystemEvent::SpeechProgress { .. }
                | SystemEvent::SpeechCancelled => {}
                SystemEvent::EmotionChanged { emotion, .. } => {
                    snapshot.current_emotion = Some(*emotion);
                }
                SystemEvent::TurnFinished { .. } => {
                    snapshot.active_turn = None;
                    snapshot.current_emotion = None;
                    snapshot.turns_completed += 1;
                }
                SystemEvent::TurnInterrupted { .. } => {
                    snapshot.active_turn = None;
                    snapshot.current_emotion = None;
                    snapshot.turns_interrupted += 1;
                }
                SystemEvent::StateChanged { to, .. } => snapshot.state = *to,
                SystemEvent::Fault { message } => {
                    snapshot.faults += 1;
                    snapshot.last_fault = Some(message.clone());
                }
                SystemEvent::ModelChunk { .. }
                | SystemEvent::LiveEventReceived { .. }
                | SystemEvent::LiveResponseSuggested { .. }
                | SystemEvent::PersonaChanged { .. }
                | SystemEvent::ComponentHealthChanged { .. } => {}
            }
        });
    }

    pub fn publish_now(&self, event: SystemEvent) {
        self.publish_if(event, || true);
    }

    /// Evaluate a cancellation-clock predicate while holding event ordering.
    /// The predicate must only read state and must not publish another event.
    pub fn publish_if(&self, event: SystemEvent, predicate: impl FnOnce() -> bool) -> bool {
        // Assign sequence, update snapshot and broadcast under the same lock.
        // Audio and runtime publishers must never expose reordered events.
        let mut history = self
            .history
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !predicate() {
            return false;
        }
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        self.update(&event, sequence);
        let event = SequencedEvent { sequence, event };
        if history.len() == self.capacity {
            history.pop_front();
        }
        history.push_back(event.clone());
        let _ignored = self.events.send(event);
        true
    }
}

#[async_trait]
impl EventSink for EventHub {
    async fn publish(&mut self, event: SystemEvent) {
        self.publish_now(event);
    }
}

pub struct TeeEventSink<A, B> {
    first: A,
    second: B,
}

impl<A, B> TeeEventSink<A, B> {
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

#[async_trait]
impl<A, B> EventSink for TeeEventSink<A, B>
where
    A: EventSink,
    B: EventSink,
{
    async fn publish(&mut self, event: SystemEvent) {
        self.first.publish(event.clone()).await;
        self.second.publish(event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_identity_survives_cloning_and_events_but_changes_for_a_new_hub() {
        let first = EventHub::new(8).unwrap();
        let clone = first.clone();
        let identity = first.current().instance_id.expect("hub has an identity");
        let second = EventHub::new(8).unwrap();
        assert_ne!(second.current().instance_id, Some(identity));
        clone.publish_now(SystemEvent::Fault {
            message: "test fault".to_owned(),
        });
        assert_eq!(first.current().last_sequence, 1);
        assert_eq!(first.current().instance_id, Some(identity));
        assert_eq!(clone.current().instance_id, Some(identity));
        assert_eq!(first.watch().borrow().instance_id, Some(identity));
    }

    #[test]
    fn snapshots_read_legacy_payloads_without_an_instance_identity() {
        let current = EventHub::new(8).unwrap().current();
        let mut payload = serde_json::to_value(&current).unwrap();
        assert_eq!(
            serde_json::from_value::<RuntimeSnapshot>(payload.clone()).unwrap(),
            current
        );
        assert!(
            payload
                .as_object_mut()
                .unwrap()
                .remove("instance_id")
                .is_some()
        );
        let legacy: RuntimeSnapshot = serde_json::from_value(payload).unwrap();
        assert_eq!(legacy.instance_id, None);
        assert_eq!(legacy.last_sequence, current.last_sequence);
        assert_eq!(legacy, RuntimeSnapshot::default());
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("instance_id")
                .is_none()
        );
    }

    #[test]
    fn voice_input_snapshot_is_backward_and_forward_compatible() {
        #[derive(serde::Deserialize)]
        struct LegacySnapshot {
            state: ConversationState,
        }

        let current = RuntimeSnapshot {
            voice_input: ai_ex_domain::VoiceInputState {
                revision: 2,
                active: true,
            },
            ..Default::default()
        };
        let mut payload = serde_json::to_value(&current).unwrap();
        let legacy: LegacySnapshot = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(legacy.state, current.state);
        payload.as_object_mut().unwrap().remove("voice_input");
        let decoded: RuntimeSnapshot = serde_json::from_value(payload).unwrap();
        assert_eq!(
            decoded.voice_input,
            ai_ex_domain::VoiceInputState::default()
        );
    }

    #[tokio::test]
    async fn broadcasts_events_and_updates_snapshot() {
        let mut hub = EventHub::new(8).expect("event hub");
        let mut events = hub.subscribe();
        let turn_id = TurnId::new();

        hub.publish(SystemEvent::TurnStarted {
            turn_id,
            user_text: "hello".to_owned(),
        })
        .await;
        hub.publish(SystemEvent::StateChanged {
            from: ConversationState::Idle,
            to: ConversationState::Thinking,
        })
        .await;
        hub.publish(SystemEvent::SentenceReady {
            turn_id,
            text: "hi".to_owned(),
        })
        .await;
        hub.publish(SystemEvent::TurnFinished {
            turn_id,
            full_text: "hi".to_owned(),
        })
        .await;

        let snapshot = hub.current();
        assert_eq!(snapshot.state, ConversationState::Thinking);
        assert_eq!(snapshot.active_turn, None);
        assert_eq!(snapshot.turns_started, 1);
        assert_eq!(snapshot.turns_completed, 1);
        assert_eq!(snapshot.sentences_ready, 1);
        assert_eq!(snapshot.last_sequence, 4);
        for sequence in 1..=4 {
            assert_eq!(
                events.try_recv().expect("broadcast event").sequence,
                sequence,
            );
        }
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn replay_is_ordered_and_bounded() {
        let mut hub = EventHub::new(2).expect("event hub");
        for index in 0..3 {
            hub.publish(SystemEvent::Fault {
                message: format!("fault {index}"),
            })
            .await;
        }

        let replay = hub.events_since(0, 10);
        assert_eq!(
            replay.iter().map(|item| item.sequence).collect::<Vec<_>>(),
            vec![2, 3],
        );
        assert_eq!(hub.events_since(2, 1)[0].sequence, 3);
    }

    #[tokio::test]
    async fn replays_component_health_transitions_for_diagnostics() {
        let mut hub = EventHub::new(8).expect("event hub");
        hub.publish(SystemEvent::ComponentHealthChanged {
            component: "bilibili".to_owned(),
            ready: false,
            detail: "connecting".to_owned(),
        })
        .await;
        hub.publish(SystemEvent::ComponentHealthChanged {
            component: "bilibili".to_owned(),
            ready: true,
            detail: "connected".to_owned(),
        })
        .await;

        let replay = hub.events_since(0, 8);
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].sequence, 1);
        assert_eq!(replay[1].sequence, 2);
        assert_eq!(
            replay[1].event,
            SystemEvent::ComponentHealthChanged {
                component: "bilibili".to_owned(),
                ready: true,
                detail: "connected".to_owned(),
            },
        );
        assert_eq!(hub.current().last_sequence, 2);
    }
    #[test]
    fn rejects_zero_capacity() {
        assert!(EventHub::new(0).is_err());
    }

    #[test]
    fn concurrent_audio_and_runtime_publishers_keep_snapshot_history_and_broadcast_in_order() {
        let hub = EventHub::new(512).unwrap();
        let mut receiver = hub.subscribe();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let hub = hub.clone();
                scope.spawn(move || {
                    for _ in 0..32 {
                        hub.publish_now(SystemEvent::SpeechPlayback {
                            playback: ai_ex_domain::SpeechPlaybackSnapshot::default(),
                        });
                    }
                });
            }
        });
        let history = hub.events_since(0, 512);
        assert_eq!(history.len(), 256);
        assert_eq!(hub.current().last_sequence, 256);
        for (index, event) in history.iter().enumerate() {
            assert_eq!(event.sequence, index as u64 + 1);
            assert_eq!(receiver.try_recv().unwrap().sequence, event.sequence);
        }
    }
}
