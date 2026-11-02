mod materials;

use std::path::{Path, PathBuf};

use ai_ex_model_catalog::{ArtifactKind, ModelBackend};
use ai_ex_model_manager::{RuntimeManifest, RuntimeWorker};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::bundle_io::{Result, hash_file, read_json, verify_file, write_json};
use crate::bundle_zip::Entry;
use crate::catalog_build::{FileRecord, REVISION, RUNTIME_ID};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleSpec {
    schema_version: u16,
    distribution_directory: PathBuf,
    project_root: PathBuf,
    source_commit: String,
    source_tree_dirty: bool,
    workers: WorkerInputs,
    sherpa_directory: PathBuf,
    qwen_ort_directory: PathBuf,
    bge_ort_directory: PathBuf,
    ollama_directory: PathBuf,
    #[serde(default)]
    corresponding_sources: Option<WorkerInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerInputs {
    sherpa: WorkerInput,
    qwen: WorkerInput,
    bge: WorkerInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerInput {
    source: PathBuf,
    bytes: u64,
    sha256: String,
}

#[derive(Deserialize)]
struct RuntimeInputs {
    runtimes: Vec<RuntimeInput>,
}

#[derive(Deserialize)]
struct RuntimeInput {
    id: String,
    version: String,
    files: Option<Vec<FileRecord>>,
}

#[derive(Deserialize)]
struct OllamaInput {
    version: String,
    source_asset_id: u64,
    files: Vec<OllamaFile>,
}

#[derive(Deserialize)]
struct OllamaFile {
    #[serde(flatten)]
    file: FileRecord,
    crc32_matches_central: bool,
    signature_status: Option<String>,
    signer: Option<String>,
    signer_thumbprint: Option<String>,
}

pub fn build(spec: &Path, output: &Path, manifest_path: &Path) -> Result<(Value, bool)> {
    if output.exists() || manifest_path.exists() || output == manifest_path {
        return Err("runtime ZIP and manifest outputs must be distinct new paths".into());
    }
    let spec: BundleSpec = read_json(spec)?;
    if spec.schema_version != 1
        || spec.source_commit.len() != 40
        || !spec
            .source_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("bundle requires schema 1 and the exact project source commit".into());
    }
    let mut entries = runtime_entries(&spec)?;
    append_sources(&mut entries, spec.corresponding_sources.as_ref())?;
    let licenses = materials::append(
        &spec.distribution_directory,
        &spec.project_root,
        &spec.source_commit,
        &mut entries,
    )?;
    let build_inputs = json!({"schema_version":1, "source_commit":spec.source_commit,
        "source_tree_dirty":spec.source_tree_dirty,
        "runtime_id":RUNTIME_ID, "runtime_revision":REVISION,
        "workers":entries.iter().filter(|entry| entry.artifact.path.ends_with("worker.exe"))
            .map(|entry| &entry.artifact).collect::<Vec<_>>()});
    entries.push(Entry::generated(
        "build-inputs.json",
        serde_json::to_vec_pretty(&build_inputs).map_err(|error| error.to_string())?,
    ));
    entries.sort_by(|a, b| a.artifact.path.cmp(&b.artifact.path));
    let manifest = RuntimeManifest {
        id: RUNTIME_ID.into(),
        revision: REVISION.into(),
        protocol_version: 1,
        backends: vec![
            ModelBackend::Ollama,
            ModelBackend::SherpaWhisper,
            ModelBackend::SherpaKokoro,
            ModelBackend::Qwen3TtsOnnx,
            ModelBackend::OnnxEmbedding,
        ],
        artifacts: entries.iter().map(|entry| entry.artifact.clone()).collect(),
        licenses,
        workers: vec![
            worker(
                ModelBackend::SherpaWhisper,
                "sherpa/ai-ex-native-sherpa-worker.exe",
            ),
            worker(
                ModelBackend::SherpaKokoro,
                "sherpa/ai-ex-native-sherpa-worker.exe",
            ),
            worker(
                ModelBackend::Qwen3TtsOnnx,
                "qwen/ai-ex-native-qwen-worker.exe",
            ),
            worker(
                ModelBackend::OnnxEmbedding,
                "bge/ai-ex-native-bge-worker.exe",
            ),
        ],
        ollama_executable: Some("ollama/ollama.exe".into()),
    };
    manifest.validate().map_err(|error| error.to_string())?;
    crate::bundle_zip::write_archive(output, &entries)?;
    crate::bundle_zip::verify_archive(output, &manifest.artifacts)?;
    let (bytes, sha256) = hash_file(output)?;
    write_json(manifest_path, &manifest)?;
    Ok((
        json!({"status":"passed", "output":output, "manifest":manifest_path,
        "bytes":bytes, "sha256":sha256, "artifacts":entries.len(),
        "source_commit":spec.source_commit,
        "source_tree_dirty":spec.source_tree_dirty,
        "signature_verification":"bytes match the recorded valid publisher-signature evidence"}),
        true,
    ))
}

pub(super) fn append_sources(entries: &mut Vec<Entry>, input: Option<&WorkerInput>) -> Result<()> {
    let Some(input) = input else { return Ok(()) };
    verify_file(&input.source, input.bytes, &input.sha256)?;
    let artifact = FileRecord {
        path: String::new(),
        bytes: input.bytes,
        sha256: input.sha256.clone(),
    }
    .artifact(
        "sources/corresponding-sources.zip".into(),
        ArtifactKind::SourceArchive,
    );
    artifact.validate().map_err(|error| error.to_string())?;
    zip::ZipArchive::new(crate::bundle_io::regular_file(&input.source)?)
        .map_err(|error| format!("invalid corresponding sources ZIP: {error}"))?;
    entries.push(Entry {
        artifact,
        input: input.source.clone().into(),
    });
    Ok(())
}

fn worker(backend: ModelBackend, path: &str) -> RuntimeWorker {
    RuntimeWorker {
        backend,
        executable: path.into(),
        runtime_library: match backend {
            ModelBackend::Qwen3TtsOnnx => Some("qwen/onnxruntime.dll".into()),
            ModelBackend::OnnxEmbedding => Some("bge/onnxruntime.dll".into()),
            _ => None,
        },
    }
}

fn append(entries: &mut Vec<Entry>, root: &Path, folder: &str, files: &[FileRecord]) -> Result<()> {
    for file in files {
        ai_ex_model_catalog::validate_relative_path(&file.path)
            .map_err(|error| error.to_string())?;
        let input = root.join(&file.path);
        verify_file(&input, file.bytes, &file.sha256)?;
        let executable = file.path.ends_with(".exe") || file.path.ends_with(".dll");
        let kind = if executable {
            ArtifactKind::NativeRuntime
        } else {
            ArtifactKind::License
        };
        entries.push(Entry {
            input: input.into(),
            artifact: file.artifact(format!("{folder}/{}", file.path), kind),
        });
    }
    Ok(())
}

fn runtime_entries(spec: &BundleSpec) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for (folder, input, name) in [
        (
            "sherpa",
            &spec.workers.sherpa,
            "ai-ex-native-sherpa-worker.exe",
        ),
        ("qwen", &spec.workers.qwen, "ai-ex-native-qwen-worker.exe"),
        ("bge", &spec.workers.bge, "ai-ex-native-bge-worker.exe"),
    ] {
        if input.source.file_name().and_then(|value| value.to_str()) != Some(name) {
            return Err(format!(
                "unexpected worker executable: {}",
                input.source.display()
            ));
        }
        verify_file(&input.source, input.bytes, &input.sha256)?;
        let artifact = FileRecord {
            path: name.into(),
            bytes: input.bytes,
            sha256: input.sha256.clone(),
        }
        .artifact(format!("{folder}/{name}"), ArtifactKind::NativeRuntime);
        artifact.validate().map_err(|error| error.to_string())?;
        entries.push(Entry {
            artifact,
            input: input.source.clone().into(),
        });
    }
    let inputs: RuntimeInputs = read_json(&spec.distribution_directory.join("runtime-files.json"))?;
    for (id, version, expected_count, root, folder) in [
        ("sherpa-onnx", "1.13.8", 4, &spec.sherpa_directory, "sherpa"),
        ("onnxruntime", "1.28.2", 2, &spec.qwen_ort_directory, "qwen"),
        ("onnxruntime", "1.28.2", 2, &spec.bge_ort_directory, "bge"),
    ] {
        let input = inputs
            .runtimes
            .iter()
            .find(|input| input.id == id && input.version == version)
            .ok_or_else(|| format!("missing fixed {id} runtime evidence"))?;
        let files = input.files.as_ref().ok_or("missing runtime files")?;
        if files.len() != expected_count {
            return Err(format!("unexpected {id} runtime file count"));
        }
        append(&mut entries, root, folder, files)?;
    }
    let ollama: OllamaInput =
        read_json(&spec.distribution_directory.join("ollama-cpu-files.json"))?;
    if ollama.version != ai_ex_ollama::MANAGED_VERSION
        || ollama.source_asset_id != 672866806
        || ollama.files.len() != 32
    {
        return Err("Ollama CPU input differs from the verified fixed asset subset".into());
    }
    for file in &ollama.files {
        let executable = file.file.path.ends_with(".exe") || file.file.path.ends_with(".dll");
        if !file.crc32_matches_central
            || executable
                && (file.signature_status.as_deref() != Some("Valid")
                    || file
                        .signer
                        .as_ref()
                        .is_none_or(|signer| !signer.starts_with("CN=Ollama Inc.,"))
                    || file.signer_thumbprint.as_deref()
                        != Some("716CD3BC8C02361431A18F56F98C72DE88066103"))
        {
            return Err(format!(
                "missing verified publisher identity: {}",
                file.file.path
            ));
        }
    }
    append(
        &mut entries,
        &spec.ollama_directory,
        "ollama",
        &ollama
            .files
            .into_iter()
            .map(|file| file.file)
            .collect::<Vec<_>>(),
    )?;
    Ok(entries)
}
