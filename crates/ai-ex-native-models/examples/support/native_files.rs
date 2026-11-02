use std::io::{Cursor, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use ai_ex_config::native::{
    ModelSelection, NativeBackend, VoiceLanguage, VoicePreset, VoiceSource,
};
use ai_ex_core::EmbeddingPort;
use ai_ex_domain::AppError;
use ai_ex_duplex::{TranscriberPort, Utterance};
use ai_ex_inference::RequestIdentity;
use ai_ex_native_models::{NativeEmbedder, NativeSynthesizer, NativeTranscriber, NativeWorkerPool};
use serde_json::{Value, json};
use uuid::Uuid;

pub async fn run(
    pool: &NativeWorkerPool,
    jobs: &Path,
    skip_embedding: bool,
) -> Result<Value, AppError> {
    let embedding = if skip_embedding {
        json!({"status":"not_run","reason":"embedding probe omitted by specification"})
    } else {
        embedding(pool).await?
    };

    let preset = VoicePreset::default();

    let synth = NativeSynthesizer::new(pool.clone(), preset.clone())?;
    let text = "你好，今天我们一起学习。我会认真听你说话，也会陪你完成计划。";
    let before = Instant::now();
    let audio = synth.synthesize(identity(), text).await?;
    let preset_ms = before.elapsed().as_millis();
    let preset_path = jobs.join("preset-output.wav");
    write_new(&preset_path, &audio.bytes)?;
    pool.unload(&preset.model).await?;

    let reference_id = pool.import_reference_wav(&preset_path).await?;
    let voice = VoicePreset {
        id: "file-reference".into(),
        label: "文件参考声音".into(),
        language: VoiceLanguage::Zh,
        model: ModelSelection::cpu(NativeBackend::Qwen3TtsOnnx, "qwen3-tts-base-0.6b"),
        source: VoiceSource::ReferenceAudio {
            audio_id: reference_id,
            text: text.into(),
        },
    };
    let cloned = NativeSynthesizer::new(pool.clone(), voice.clone())?;
    let before = Instant::now();
    let clone = cloned.synthesize(identity(), "你好，我在这里。").await?;
    let clone_ms = before.elapsed().as_millis();
    write_new(&jobs.join("clone-output.wav"), &clone.bytes)?;
    pool.unload(&voice.model).await?;

    let model = ModelSelection::cpu(NativeBackend::SherpaWhisper, "whisper-small-int8");
    let mut transcriber = NativeTranscriber::new(pool.clone(), model.clone())?;
    pool.wait_ready(&model, Duration::from_secs(120)).await?;
    let before = Instant::now();
    let preset_transcript = transcriber.transcribe(utterance(&audio.bytes)?).await?;
    let clone_transcript = transcriber.transcribe(utterance(&clone.bytes)?).await?;
    if preset_transcript.trim().is_empty() || clone_transcript.trim().is_empty() {
        return Err(AppError::protocol(
            "native recognition returned an empty file transcript",
        ));
    }
    let asr_ms = before.elapsed().as_millis();
    pool.unload(&model).await?;
    Ok(
        json!({"status":"passed","embedding":embedding,"preset":{"elapsed_ms":preset_ms,"sample_rate":audio.sample_rate,"bytes":audio.bytes.len(),"transcript":preset_transcript},"reference_clone":{"elapsed_ms":clone_ms,"sample_rate":clone.sample_rate,"bytes":clone.bytes.len(),"transcript":clone_transcript},"outputs":{"directory":jobs,"preset":preset_path,"clone":jobs.join("clone-output.wav")},"transcription_ms":asr_ms,"device_access_performed":false,"speaker_similarity_evaluated":false,"product_gui_evaluated":false}),
    )
}

async fn embedding(pool: &NativeWorkerPool) -> Result<Value, AppError> {
    let selection = ModelSelection::cpu(NativeBackend::OnnxEmbedding, "bge-small-zh-v1.5");
    let embedder = NativeEmbedder::new(pool.clone(), selection.clone())?;
    let before = Instant::now();
    pool.wait_ready(&selection, Duration::from_secs(120))
        .await?;
    let warmup_ms = before.elapsed().as_millis();
    let first = embedder.embed("我喜欢在安静的下午喝茶。").await?;
    let repeated = embedder.embed("我喜欢在安静的下午喝茶。").await?;
    let different = embedder.embed("卫星正在围绕地球运行。").await?;
    let cosine = |left: &[f32], right: &[f32]| {
        left.iter()
            .zip(right)
            .map(|(a, b)| f64::from(*a) * f64::from(*b))
            .sum::<f64>()
    };
    let repeat_cosine = cosine(&first, &repeated);
    let distinct_cosine = cosine(&first, &different);
    if repeat_cosine < 0.9999 || distinct_cosine > 0.999 {
        return Err(AppError::protocol(
            "native embedding file probe did not distinguish inputs",
        ));
    }
    pool.unload(&selection).await?;
    Ok(
        json!({"dimensions":first.len(),"space":embedder.space(),"warmup_ms":warmup_ms,"repeat_cosine":repeat_cosine,"distinct_cosine":distinct_cosine}),
    )
}

fn identity() -> RequestIdentity {
    RequestIdentity {
        session_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        generation: 1,
    }
}
fn utterance(bytes: &[u8]) -> Result<Utterance, AppError> {
    let mut wave = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|error| AppError::protocol(error.to_string()))?;
    let spec = wave.spec();
    let samples = wave
        .samples::<i16>()
        .map(|sample| sample.map(|value| f32::from(value) / 32768.0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| AppError::protocol(error.to_string()))?;
    Ok(Utterance {
        samples,
        sample_rate: spec.sample_rate,
        channels: spec.channels,
    })
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|error| AppError::configuration(error.to_string()))
}
