use ai_ex_domain::{
    AppError, Emotion, MemoryRequest, MemoryResponse, Message, SystemEvent, TurnId,
};
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Optional semantic encoder. The stable space identifies the model revision and
/// preprocessing shared by all comparable vectors; changing either needs a new space.
#[async_trait]
pub trait EmbeddingPort: Send + Sync {
    fn space(&self) -> &str;
    async fn embed(&self, text: &str) -> Result<Vec<f32>, AppError>;
}
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub turn_id: TurnId,
    pub messages: Vec<Message>,
}

#[async_trait]
pub trait LanguageModelPort: Send {
    async fn stream(
        &mut self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<Result<String, AppError>>, AppError>;

    async fn cancel(&mut self, turn_id: TurnId) -> Result<(), AppError>;
}

#[async_trait]
pub trait SpeechPort: Send {
    async fn enqueue(&mut self, turn_id: TurnId, sentence: String) -> Result<(), AppError>;
    async fn enqueue_expressive(
        &mut self,
        turn_id: TurnId,
        sentence: String,
        _emotion: Emotion,
    ) -> Result<(), AppError> {
        self.enqueue(turn_id, sentence).await
    }
    async fn interrupt(&mut self) -> Result<(), AppError>;
}

#[async_trait]
pub trait AvatarPort: Send {
    async fn set_speaking(&mut self, speaking: bool) -> Result<(), AppError>;
    async fn set_neutral(&mut self) -> Result<(), AppError>;

    async fn set_subtitle(&mut self, _text: String) -> Result<(), AppError> {
        Ok(())
    }

    async fn interrupt_presentation(&mut self) -> Result<(), AppError> {
        self.set_neutral().await
    }

    async fn set_emotion(&mut self, _emotion: Emotion) -> Result<(), AppError> {
        Ok(())
    }
}

#[async_trait]
pub trait EventSink: Send {
    async fn publish(&mut self, event: SystemEvent);
}

#[async_trait]
pub trait MemoryPort: Send + Sync {
    async fn manage(&mut self, _request: MemoryRequest) -> Result<MemoryResponse, AppError> {
        Err(AppError::unavailable(
            "memory adapter does not support memory management",
        ))
    }

    /// Switch all subsequent operations atomically, or leave the old scope unchanged.
    async fn select_profile(&mut self, _profile_id: &str) -> Result<(), AppError> {
        Err(AppError::unavailable(
            "memory adapter does not support isolated profiles",
        ))
    }

    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<Message>, AppError>;

    async fn recall_for_context(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>, AppError> {
        self.recall(query, limit).await
    }

    async fn remember(
        &mut self,
        turn_id: TurnId,
        user_text: String,
        assistant_text: String,
    ) -> Result<(), AppError>;
}
