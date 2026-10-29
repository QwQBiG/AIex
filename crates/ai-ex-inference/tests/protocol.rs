#![forbid(unsafe_code)]

use ai_ex_inference::{
    AudioFile, InferenceOutput, InferenceRequest, InferenceTask, RequestIdentity, VoiceSelection,
};
use uuid::Uuid;

fn request(task: InferenceTask) -> InferenceRequest {
    InferenceRequest {
        identity: RequestIdentity {
            session_id: Uuid::new_v4(),
            request_id: Uuid::new_v4(),
            generation: 0,
        },
        model_id: "fixed-model".into(),
        task,
    }
}

#[test]
fn reference_cannot_overwrite_its_own_audio_and_paths_stay_relative() {
    let job = request(InferenceTask::Synthesize {
        text: "你好".into(),
        language: "zh".into(),
        voice: VoiceSelection::Reference {
            audio: AudioFile {
                path: "voice/REF.wav".into(),
            },
            text: "参考文本".into(),
        },
        output: AudioFile {
            path: "voice/ref.wav".into(),
        },
    });
    assert!(job.validate().is_err());
    let mut unicode_job = job.clone();
    if let InferenceTask::Synthesize { voice, output, .. } = &mut unicode_job.task {
        *voice = VoiceSelection::Reference {
            audio: AudioFile {
                path: "voice/Ä.wav".into(),
            },
            text: "参考文本".into(),
        };
        output.path = "voice/ä.wav".into();
    }
    assert!(unicode_job.validate().is_err());
    for path in [
        "../escape.wav",
        "C:/voice.wav",
        "file.wav:stream",
        "NUL.wav",
    ] {
        assert!(
            AudioFile { path: path.into() }.validate().is_err(),
            "{path}"
        );
    }
}

#[test]
fn embedding_requires_a_space_and_nonzero_finite_values() {
    assert!(
        request(InferenceTask::Embed {
            text: "你好".into(),
            space: " ".into()
        })
        .validate()
        .is_err()
    );
    for values in [vec![], vec![0.0], vec![f32::NAN], vec![f32::INFINITY]] {
        assert!(
            InferenceOutput::Embedding {
                space: "bge-fixed-space".into(),
                values
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        InferenceOutput::Embedding {
            space: "bge-fixed-space".into(),
            values: vec![0.5, -0.5]
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn nil_identity_and_unknown_protocol_fields_are_rejected() {
    let mut job = request(InferenceTask::Embed {
        text: "你好".into(),
        space: "bge".into(),
    });
    job.identity.request_id = Uuid::nil();
    assert!(job.validate().is_err());
    let mut serialized = serde_json::to_value(job).unwrap();
    serialized["extra"] = serde_json::json!(true);
    assert!(serde_json::from_value::<InferenceRequest>(serialized).is_err());
}
