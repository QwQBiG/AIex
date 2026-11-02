use std::path::PathBuf;

use ai_ex_domain::AppError;
use tokio::io::AsyncReadExt;

pub async fn run(arguments: Vec<String>) -> Result<(), AppError> {
    let mut input = None;
    let mut output = None;
    let mut preview = false;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        if flag == "--preview" {
            if preview {
                return Err(AppError::configuration("duplicate --preview"));
            }
            preview = true;
            continue;
        }
        let slot = match flag.as_str() {
            "--input" => &mut input,
            "--output" => &mut output,
            _ => return Err(AppError::configuration("unknown memory upgrade option")),
        };
        let path = arguments
            .next()
            .ok_or_else(|| AppError::configuration("memory upgrade option requires a path"))?;
        if slot.replace(PathBuf::from(path)).is_some() {
            return Err(AppError::configuration("duplicate memory upgrade path"));
        }
    }
    let input =
        input.ok_or_else(|| AppError::configuration("--upgrade-memory requires --input"))?;
    if preview == output.is_some() {
        return Err(AppError::configuration(
            "choose exactly one of --preview or --output for memory upgrade",
        ));
    }
    if let Some(path) = &output
        && tokio::fs::try_exists(path)
            .await
            .map_err(|e| AppError::unavailable(e.to_string()))?
    {
        return Err(AppError::configuration(
            "refusing to overwrite existing memory upgrade output",
        ));
    }
    // Runtime stores can be larger than the separate 32 MiB legacy backup envelope.
    let mut file = tokio::fs::File::open(&input)
        .await
        .map_err(|e| AppError::unavailable(e.to_string()))?;
    const LIMIT: u64 = 256 * 1024 * 1024;
    let mut bytes = Vec::new();
    (&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| AppError::unavailable(e.to_string()))?;
    if bytes.len() as u64 > LIMIT {
        return Err(AppError::configuration(
            "memory upgrade input exceeds 256 MiB",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| AppError::configuration("memory JSONL must use UTF-8"))?;
    let (content, report) = ai_ex_memory::migrate_jsonl(text)?;
    if let Some(output) = output {
        super::write_new_atomic(&output, content.as_bytes()).await?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| AppError::protocol(e.to_string()))?
    );
    Ok(())
}
