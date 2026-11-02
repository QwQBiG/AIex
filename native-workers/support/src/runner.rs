use ai_ex_inference::{
    WORKER_PROTOCOL_VERSION, WorkerCommand, WorkerReply, read_frame, write_frame,
};
use tokio::io::BufReader;

use crate::{AppError, InferenceOutput, InferenceRequest};

pub trait Engine {
    fn model_id(&self) -> &str;
    fn run(&mut self, request: &InferenceRequest) -> Result<InferenceOutput, AppError>;
}

/// Native computation stays in this process. The host kills it after its bounded
/// cancellation grace; this loop never acknowledges work that is still running.
pub async fn serve(mut engine: impl Engine) -> Result<(), AppError> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    write_frame(
        &mut output,
        &WorkerReply::Ready {
            protocol_version: WORKER_PROTOCOL_VERSION,
        },
    )
    .await?;
    while let Some(command) = read_frame(&mut input).await? {
        match command {
            WorkerCommand::Shutdown => return Ok(()),
            WorkerCommand::Cancel { identity } => {
                identity.validate()?;
                write_frame(&mut output, &WorkerReply::Cancelled { identity }).await?;
            }
            WorkerCommand::Run { request } => {
                request.validate()?;
                let result = if request.model_id == engine.model_id() {
                    engine.run(&request).and_then(|output| {
                        output.validate()?;
                        Ok(output)
                    })
                } else {
                    Err(AppError::configuration(
                        "request model does not match loaded package",
                    ))
                };
                let reply = match result {
                    Ok(output) => WorkerReply::Completed {
                        identity: request.identity,
                        output,
                    },
                    Err(error) => WorkerReply::Failed {
                        identity: request.identity,
                        error,
                    },
                };
                write_frame(&mut output, &reply).await?;
            }
        }
    }
    Ok(())
}
