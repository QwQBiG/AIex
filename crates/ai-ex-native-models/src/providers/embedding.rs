use ai_ex_config::native::{ModelSelection, NativeBackend};
use ai_ex_core::EmbeddingPort;
use ai_ex_domain::AppError;
use ai_ex_inference::{InferenceOutput, InferenceRequest, InferenceTask, RequestIdentity};
use async_trait::async_trait;
use uuid::Uuid;

use crate::{BGE_SPACE, NativeWorkerPool, files};

#[derive(Clone)]
pub struct NativeEmbedder {
    pool: NativeWorkerPool,
    model: ModelSelection,
    session: Uuid,
}
impl NativeEmbedder {
    pub fn new(pool: NativeWorkerPool, model: ModelSelection) -> Result<Self, AppError> {
        if model.backend != NativeBackend::OnnxEmbedding {
            return Err(files::error("native embedding requires its ONNX backend"));
        }
        pool.warmup(&model)?;
        Ok(Self {
            pool,
            model,
            session: Uuid::new_v4(),
        })
    }
}

#[async_trait]
impl EmbeddingPort for NativeEmbedder {
    fn space(&self) -> &str {
        BGE_SPACE
    }
    async fn embed(&self, text: &str) -> Result<Vec<f32>, AppError> {
        let files = self.pool.new_request(&self.model)?;
        let request = InferenceRequest {
            identity: RequestIdentity {
                session_id: self.session,
                request_id: Uuid::new_v4(),
                generation: 0,
            },
            model_id: self.model.model_id.clone(),
            task: InferenceTask::Embed {
                text: text.to_owned(),
                space: BGE_SPACE.into(),
            },
        };
        let result = self.pool.execute(&self.model, request, files).await?;
        match result.output {
            InferenceOutput::Embedding { space, values }
                if space == BGE_SPACE && values.len() == 512 =>
            {
                Ok(values)
            }
            _ => Err(AppError::protocol(
                "native embedding output does not match its fixed vector space",
            )),
        }
    }
}
