#![forbid(unsafe_code)]

use ai_ex_inference::{
    InferenceOutput, InferenceTask, WORKER_PROTOCOL_VERSION, WorkerCommand, WorkerReply,
    read_frame, write_frame,
};
use std::time::Duration;
use tokio::io::BufReader;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    std::fs::write("loaded.pid", std::process::id().to_string()).unwrap();
    if std::env::current_exe()
        .unwrap()
        .file_stem()
        .and_then(|name| name.to_str())
        == Some("startup-hold")
    {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    write_frame(
        &mut output,
        &WorkerReply::Ready {
            protocol_version: WORKER_PROTOCOL_VERSION,
        },
    )
    .await
    .unwrap();
    while let Some(command) = read_frame(&mut input).await.unwrap() {
        match command {
            WorkerCommand::Run { request } => {
                std::fs::write(
                    "active.json",
                    serde_json::to_vec(&request.identity).unwrap(),
                )
                .unwrap();
                let text = match &request.task {
                    InferenceTask::Embed { text, .. } | InferenceTask::Synthesize { text, .. } => {
                        text.as_str()
                    }
                    _ => "",
                };
                if text == "hold" {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
                if text == "delay" {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                }
                let result = match request.task {
                    InferenceTask::Embed { space, .. } => {
                        let mut values = vec![0.0; 512];
                        values[0] = 1.0;
                        InferenceOutput::Embedding { space, values }
                    }
                    InferenceTask::Synthesize { output, .. } => {
                        std::fs::write(
                            &output.path,
                            ai_ex_inference::encode_mono_pcm16_wav(&[0.25; 2400], 24000).unwrap(),
                        )
                        .unwrap();
                        InferenceOutput::Audio {
                            file: output,
                            sample_rate: 24000,
                        }
                    }
                    InferenceTask::Transcribe { .. } => InferenceOutput::Transcript {
                        text: "中文文件用例".into(),
                    },
                };
                write_frame(
                    &mut output,
                    &WorkerReply::Completed {
                        identity: request.identity,
                        output: result,
                    },
                )
                .await
                .unwrap();
            }
            WorkerCommand::Cancel { identity } => {
                write_frame(&mut output, &WorkerReply::Cancelled { identity })
                    .await
                    .unwrap();
            }
            WorkerCommand::Shutdown => return,
        }
    }
}
