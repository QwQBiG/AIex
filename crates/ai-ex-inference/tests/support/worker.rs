//! Protocol fixture, built only by the explicit test-worker feature.
#![forbid(unsafe_code)]

use ai_ex_inference::{
    InferenceOutput, InferenceTask, WORKER_PROTOCOL_VERSION, WorkerCommand, WorkerReply,
    read_frame, write_frame,
};
use tokio::io::BufReader;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    if mode == "exit" {
        return;
    }
    if mode == "slow-ready" {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
    write_frame(
        &mut output,
        &WorkerReply::Ready {
            protocol_version: if mode == "incompatible" {
                0
            } else {
                WORKER_PROTOCOL_VERSION
            },
        },
    )
    .await
    .unwrap();
    while let Some(command) = read_frame::<_, WorkerCommand>(&mut input).await.unwrap() {
        match command {
            WorkerCommand::Run { request } => {
                if mode == "uncooperative" {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                }
                if mode == "hold" || mode == "late" {
                    continue;
                }
                if mode == "crash" {
                    return;
                }
                eprintln!("{}", "diagnostics".repeat(2000));
                let mut identity = request.identity;
                if mode == "stale" {
                    identity.generation += 1;
                }
                let result = match request.task {
                    InferenceTask::Transcribe { .. } => InferenceOutput::Transcript {
                        text: "你好，世界".into(),
                    },
                    InferenceTask::Embed { space, .. } => InferenceOutput::Embedding {
                        space: if mode == "wrong-space" {
                            "other".into()
                        } else {
                            space
                        },
                        values: vec![1.0, 0.5],
                    },
                    InferenceTask::Synthesize { output, .. } => InferenceOutput::Audio {
                        file: output,
                        sample_rate: 24000,
                    },
                };
                write_frame(
                    &mut output,
                    &WorkerReply::Completed {
                        identity,
                        output: result,
                    },
                )
                .await
                .unwrap();
            }
            WorkerCommand::Cancel { identity } => {
                if mode == "late" {
                    write_frame(
                        &mut output,
                        &WorkerReply::Completed {
                            identity,
                            output: InferenceOutput::Transcript {
                                text: "discard this".into(),
                            },
                        },
                    )
                    .await
                    .unwrap();
                }
                write_frame(&mut output, &WorkerReply::Cancelled { identity })
                    .await
                    .unwrap();
            }
            WorkerCommand::Shutdown => return,
        }
    }
}
