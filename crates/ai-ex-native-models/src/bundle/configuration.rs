use ai_ex_domain::AppError;
use ai_ex_model_catalog::ModelBackend;
use serde_json::{Value, json};

use crate::files;

pub(super) fn configuration(
    backend: ModelBackend,
    prefix: &str,
    library: Option<&str>,
) -> Result<Value, AppError> {
    let path = |name: &str| format!("{prefix}/{name}");
    Ok(match backend {
        ModelBackend::OnnxEmbedding => {
            json!({"runtime_dll": library.ok_or_else(|| files::error("embedding runtime library is not declared"))?,"model":path("model_optimized.onnx"),"tokenizer":path("tokenizer.json")})
        }
        ModelBackend::Qwen3TtsOnnx => {
            json!({"runtime_dll":library.ok_or_else(|| files::error("reference voice runtime library is not declared"))?,"model_directory":prefix})
        }
        ModelBackend::SherpaWhisper => {
            json!({"type":"whisper","encoder":path("small-encoder.int8.onnx"),"decoder":path("small-decoder.int8.onnx"),"tokens":path("small-tokens.txt")})
        }
        ModelBackend::SherpaKokoro => {
            json!({"type":"kokoro","model":path("model.onnx"),"voices":path("voices.bin"),"tokens":path("tokens.txt"),"espeak_data":path("espeak-ng-data"),"dict":path("dict"),"lexicon_en":path("lexicon-us-en.txt"),"lexicon_zh":path("lexicon-zh.txt"),"date_fst":path("date-zh.fst"),"number_fst":path("number-zh.fst"),"phone_fst":path("phone-zh.fst")})
        }
        _ => return Err(files::error("unsupported native inference backend")),
    })
}
