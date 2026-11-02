use std::path::Path;

use ai_ex_model_catalog::{
    ArtifactKind, License, ModelBackend, ModelCapability, ModelManifest, RuntimeRequirement,
};
use ai_ex_model_manager::{ArtifactSource, DownloadLocation, ModelPackage, PackageSource};
use serde::Deserialize;

use super::{FileRecord, Materials, REVISION, RUNTIME_ID};
use crate::bundle_io::{Result, hash_bytes, read_json};

#[derive(Deserialize)]
struct ModelInputs {
    models: Vec<ModelInput>,
}

#[derive(Deserialize)]
struct ModelInput {
    id: String,
    revision: String,
    files: Vec<FileRecord>,
    #[serde(default)]
    license_material_ids: Vec<String>,
    declaration_url: Option<String>,
}

pub fn build(directory: &Path) -> Result<Vec<ModelPackage>> {
    let inputs: ModelInputs = read_json(&directory.join("models.json"))?;
    let materials: Materials = read_json(&directory.join("materials.json"))?;
    if inputs.models.len() != 5 {
        return Err("expected exactly five fixed models".into());
    }
    let mut result = Vec::new();
    for model in inputs.models {
        result.push(build_model(directory, &materials, model)?);
    }
    Ok(result)
}

fn route(
    id: &str,
) -> Result<(
    &'static str,
    &'static str,
    &'static str,
    usize,
    ModelBackend,
    ModelCapability,
)> {
    use ModelBackend as B;
    use ModelCapability as C;
    Ok(match id {
        "Qwen/Qwen3-4B-GGUF" => (
            "qwen3-4b",
            "Qwen3 4B 本地对话",
            "bc640142c66e1fdd12af0bd68f40445458f3869b",
            1,
            B::Ollama,
            C::Dialogue,
        ),
        "csukuangfj/sherpa-onnx-whisper-small" => (
            "whisper-small-int8",
            "Whisper small 多语种识别",
            "8f3c18b358db4d1f2fc1eae49d75cd20989e4309",
            3,
            B::SherpaWhisper,
            C::Transcription,
        ),
        "csukuangfj/kokoro-multi-lang-v1_1" => (
            "kokoro-v1.1-zh-en",
            "Kokoro 中文和英文声音",
            "423bfb9ccd6a66115a76602a9630ffea265ae176",
            374,
            B::SherpaKokoro,
            C::SpeechPreset,
        ),
        "wavekat/Qwen3-TTS-0.6B-Base-ONNX" => (
            "qwen3-tts-base-0.6b",
            "Qwen3 专属音色",
            "177cd39e2ddab0d4d7ce5152a7abc2d382621158",
            36,
            B::Qwen3TtsOnnx,
            C::SpeechReference,
        ),
        "Qdrant/bge-small-zh-v1.5" => (
            "bge-small-zh-v1.5",
            "BGE 中文记忆向量",
            "46fbe35fd4374a00fee7de77dfddaeb6dd6a2c59",
            3,
            B::OnnxEmbedding,
            C::Embedding,
        ),
        _ => return Err(format!("unrecognized fixed model: {id}")),
    })
}

fn build_model(directory: &Path, materials: &Materials, input: ModelInput) -> Result<ModelPackage> {
    let (id, name, revision, count, backend, capability) = route(&input.id)?;
    if input.revision != revision || input.files.len() != count {
        return Err(format!(
            "fixed model revision/file count changed: {}",
            input.id
        ));
    }
    let mut artifacts = Vec::new();
    let mut sources = Vec::new();
    let mut licenses = Vec::new();
    if backend == ModelBackend::Ollama {
        let weight = &input.files[0];
        let expected = &ai_ex_ollama::MANAGED_MODEL_FILES[0];
        if weight.path != "Qwen3-4B-Q4_K_M.gguf"
            || weight.bytes != expected.bytes
            || weight.sha256 != expected.sha256
        {
            return Err("dialogue model differs from the managed fixed preset".into());
        }
        for file in ai_ex_ollama::MANAGED_MODEL_FILES {
            let location =
                if let Some(bytes) = ai_ex_ollama::managed_model_generated_file(file.path) {
                    DownloadLocation::Inline {
                        content: String::from_utf8(bytes.to_vec()).map_err(|e| e.to_string())?,
                    }
                } else {
                    let remote = if file.path == "model.gguf" {
                        &weight.path
                    } else {
                        "LICENSE"
                    };
                    DownloadLocation::Https {
                        url: fixed_url(&input, remote),
                    }
                };
            artifacts.push(ai_ex_model_catalog::Artifact {
                path: file.path.into(),
                bytes: file.bytes,
                sha256: file.sha256.into(),
                kind: classify(file.path),
            });
            sources.push(ArtifactSource {
                path: file.path.into(),
                location,
            });
        }
        let material = materials
            .materials
            .iter()
            .find(|item| item.id == "qwen3-4b")
            .ok_or("missing original dialogue license provenance")?;
        material.text(directory)?;
        licenses.push(material.license());
    } else {
        for file in &input.files {
            artifacts.push(file.artifact(file.path.clone(), classify(&file.path)));
            sources.push(ArtifactSource {
                path: file.path.clone(),
                location: DownloadLocation::Https {
                    url: fixed_url(&input, &file.path),
                },
            });
        }
        for material_id in &input.license_material_ids {
            let material = materials
                .materials
                .iter()
                .find(|item| &item.id == material_id)
                .ok_or_else(|| format!("missing license material: {material_id}"))?;
            let content = material.text(directory)?;
            let path = format!("licenses/{material_id}.txt");
            artifacts.push(material.file.artifact(path.clone(), ArtifactKind::License));
            sources.push(ArtifactSource {
                path,
                location: DownloadLocation::Inline { content },
            });
            licenses.push(material.license());
        }
        if backend == ModelBackend::OnnxEmbedding {
            // This publisher supplies a license declaration, not a standalone copyright notice.
            let declaration = input
                .files
                .iter()
                .find(|file| file.path == "README.md")
                .ok_or("BGE publisher declaration is missing")?;
            // Exact 24-byte publisher file, including its original CRLF endings.
            let content = "---\r\nlicense: mit\r\n---\r\n";
            if content.len() as u64 != declaration.bytes
                || hash_bytes(content.as_bytes()) != declaration.sha256
            {
                return Err("BGE declaration bytes require the recorded publisher original".into());
            }
            sources
                .iter_mut()
                .find(|source| source.path == "README.md")
                .ok_or("missing BGE source")?
                .location = DownloadLocation::Inline {
                content: content.into(),
            };
            licenses.push(License {
                id: "MIT-publisher-declaration".into(),
                source_url: input
                    .declaration_url
                    .clone()
                    .ok_or("missing BGE declaration provenance")?,
            });
        }
    }
    Ok(ModelPackage {
        name: name.into(),
        source: PackageSource::Files { files: sources },
        manifest: ModelManifest {
            id: id.into(),
            revision: revision.into(),
            backend,
            capabilities: vec![capability],
            artifacts,
            licenses,
            runtimes: vec![RuntimeRequirement {
                id: RUNTIME_ID.into(),
                revision: REVISION.into(),
            }],
        },
    })
}

fn fixed_url(input: &ModelInput, path: &str) -> String {
    format!(
        "https://huggingface.co/{}/resolve/{}/{path}",
        input.id, input.revision
    )
}

fn classify(path: &str) -> ArtifactKind {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name.starts_with("license") || name == "readme.md" {
        ArtifactKind::License
    } else if name.contains("token") || name.contains("vocab") || name.contains("merges") {
        ArtifactKind::Tokenizer
    } else if path.ends_with(".onnx")
        || path.ends_with(".data")
        || path.ends_with(".gguf")
        || path.ends_with(".npy")
        || path.ends_with(".bin")
    {
        ArtifactKind::Model
    } else {
        ArtifactKind::Configuration
    }
}
