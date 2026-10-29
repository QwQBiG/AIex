use ai_ex_domain::AppError;
use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Includes the terminating newline; audio is transferred by file reference.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>, AppError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut frame = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| AppError::connectivity(format!("worker read failed: {error}")))?;
        if available.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(AppError::protocol("worker frame ended before newline"))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |position| position + 1);
        if count > MAX_FRAME_BYTES - frame.len() {
            return Err(AppError::protocol("worker frame exceeds transport limit"));
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            return serde_json::from_slice(&frame)
                .map(Some)
                .map_err(|_| AppError::protocol("worker frame is not valid protocol JSON"));
        }
    }
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), AppError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut frame =
        serde_json::to_vec(value).map_err(|_| AppError::protocol("cannot encode worker frame"))?;
    if frame.len() >= MAX_FRAME_BYTES {
        return Err(AppError::protocol("worker frame exceeds transport limit"));
    }
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .await
        .map_err(|error| AppError::connectivity(format!("worker write failed: {error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| AppError::connectivity(format!("worker flush failed: {error}")))
}
