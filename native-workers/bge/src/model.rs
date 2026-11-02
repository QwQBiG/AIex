use ai_ex_native_worker_support::{
    AppError, ArtifactKind, Engine, InferenceOutput, InferenceRequest, InferenceTask,
    VerifiedConfig,
};
use ort::{
    session::{Session, SessionInputValue},
    value::Tensor,
};
use serde::Deserialize;
use tokenizers::{Tokenizer, TruncationParams};

pub const SPACE: &str = "bge-small-zh-v1.5:46fbe35:tokenizer-48cea5d4:raw-cls512-l2-max512";
const REVISION: &str = "46fbe35fd4374a00fee7de77dfddaeb6dd6a2c59";
const MODEL_SHA: &str = "1294ea4b6331115a353d81f96b85e8c8d7fdcc284453d5b2fab5b016230aad38";
const TOKENIZER_SHA: &str = "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Files {
    runtime_dll: String,
    model: String,
    tokenizer: String,
}

pub struct Bge {
    id: String,
    tokenizer: Tokenizer,
    session: Session,
}

impl Bge {
    pub fn load(config: VerifiedConfig<Files>) -> Result<Self, AppError> {
        for (path, digest) in [
            (&config.backend.model, MODEL_SHA),
            (&config.backend.tokenizer, TOKENIZER_SHA),
        ] {
            if config.manifest.revision != REVISION
                || !config
                    .manifest
                    .artifacts
                    .iter()
                    .any(|item| item.path == *path && item.sha256.eq_ignore_ascii_case(digest))
            {
                return Err(AppError::configuration(
                    "BGE space requires the fixed model and tokenizer revision",
                ));
            }
        }
        let dll = config.artifact(&config.backend.runtime_dll, ArtifactKind::NativeRuntime)?;
        let model = config.artifact(&config.backend.model, ArtifactKind::Model)?;
        let tokenizer_path = config.artifact(&config.backend.tokenizer, ArtifactKind::Tokenizer)?;
        if !ort::init_from(&dll)
            .map_err(failure)?
            .with_telemetry(false)
            .commit()
        {
            return Err(AppError::configuration("ORT already initialized"));
        }
        let mut tokenizer = Tokenizer::from_file(tokenizer_path).map_err(failure)?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: 512,
                ..Default::default()
            }))
            .map_err(failure)?;
        let session = Session::builder()
            .map_err(failure)?
            .with_intra_threads(4)
            .map_err(failure)?
            .with_inter_threads(1)
            .map_err(failure)?
            .commit_from_file(model)
            .map_err(failure)?;
        Ok(Self {
            id: config.manifest.id,
            tokenizer,
            session,
        })
    }
}

impl Engine for Bge {
    fn model_id(&self) -> &str {
        &self.id
    }

    fn run(&mut self, request: &InferenceRequest) -> Result<InferenceOutput, AppError> {
        let InferenceTask::Embed { text, space } = &request.task else {
            return Err(AppError::configuration("BGE only supports embedding"));
        };
        if space != SPACE {
            return Err(AppError::configuration("BGE embedding space mismatch"));
        }
        let encoding = self
            .tokenizer
            .encode(text.as_str(), true)
            .map_err(failure)?;
        let count = encoding.len();
        let mut inputs: Vec<(String, SessionInputValue<'static>)> = Vec::new();
        for input in self.session.inputs() {
            let values: Vec<i64> = match input.name() {
                "input_ids" => encoding
                    .get_ids()
                    .iter()
                    .map(|value| i64::from(*value))
                    .collect(),
                "attention_mask" => encoding
                    .get_attention_mask()
                    .iter()
                    .map(|value| i64::from(*value))
                    .collect(),
                "token_type_ids" => encoding
                    .get_type_ids()
                    .iter()
                    .map(|value| i64::from(*value))
                    .collect(),
                _ => return Err(AppError::configuration("unexpected BGE input")),
            };
            inputs.push((
                input.name().to_owned(),
                Tensor::from_array(([1, count], values))
                    .map_err(failure)?
                    .into(),
            ));
        }
        eprintln!("NATIVE_INFERENCE_BEGIN {}", request.identity.request_id);
        let outputs = self.session.run(inputs).map_err(failure)?;
        let (shape, hidden) = outputs[0].try_extract_tensor::<f32>().map_err(failure)?;
        if shape.len() != 3 || shape[0] != 1 || shape[1] != count as i64 || shape[2] != 512 {
            return Err(AppError::configuration("unexpected BGE output shape"));
        }
        let mut values = hidden[..512].to_vec();
        let norm = values
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Err(AppError::unavailable("invalid BGE norm"));
        }
        for value in &mut values {
            *value = (f64::from(*value) / norm) as f32;
        }
        Ok(InferenceOutput::Embedding {
            space: space.clone(),
            values,
        })
    }
}

fn failure(error: impl std::fmt::Display) -> AppError {
    AppError::unavailable(format!("BGE native inference failed: {error}"))
}
