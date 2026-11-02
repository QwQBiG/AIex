use std::path::Path;
use std::time::{Duration, Instant};

use ai_ex_domain::AppError;
use futures_util::StreamExt;
use ring::digest::{Context, SHA256};
use tokio::io::AsyncWriteExt;

use crate::files::{self, io_error};
use crate::manager::{Package, TaskContext};
use crate::{DownloadLocation, PackageSource};

mod archive;
mod copy;

#[cfg(test)]
pub(crate) use copy::copy as copy_for_test;
pub(crate) use copy::import;

pub(crate) async fn download(
    context: &TaskContext,
    package: &Package,
    staging: &Path,
    data: &Path,
) -> Result<(), AppError> {
    let origin = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(network_error)?;
    let direct = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(network_error)?;
    match package.source() {
        PackageSource::Files { files: sources } => {
            for source in sources {
                context.cancel.check()?;
                let artifact = package
                    .artifacts()
                    .iter()
                    .find(|file| file.path == source.path)
                    .expect("catalog source is validated");
                match &source.location {
                    DownloadLocation::Inline { content } => {
                        let mut file =
                            tokio::fs::File::from_std(files::new_output(data, &source.path)?);
                        file.write_all(content.as_bytes()).await.map_err(io_error)?;
                        file.sync_all().await.map_err(io_error)?;
                        context.progress(content.len() as u64)?;
                    }
                    DownloadLocation::Https { url } => {
                        download_file(
                            context,
                            &origin,
                            &direct,
                            url,
                            data,
                            &source.path,
                            artifact.bytes,
                            &artifact.sha256,
                        )
                        .await?;
                    }
                }
            }
        }
        PackageSource::Archive { url, bytes, sha256 } => {
            download_file(
                context,
                &origin,
                &direct,
                url,
                staging,
                "download.zip",
                *bytes,
                sha256,
            )
            .await?;
            let context = context.clone();
            let package = package.clone();
            let archive = staging.join("download.zip");
            let output = data.to_path_buf();
            tokio::task::spawn_blocking(move || {
                archive::extract(&context, &package, &archive, &output)
            })
            .await
            .map_err(io_error)??;
            tokio::fs::remove_file(staging.join("download.zip"))
                .await
                .map_err(io_error)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_file(
    context: &TaskContext,
    origin: &reqwest::Client,
    direct: &reqwest::Client,
    source: &str,
    root: &Path,
    relative: &str,
    expected: u64,
    digest: &str,
) -> Result<(), AppError> {
    let response = response(context, origin, direct, source).await?;
    if !response.status().is_success() {
        return Err(AppError::connectivity(format!(
            "download HTTP {}",
            response.status().as_u16()
        )));
    }
    if response
        .content_length()
        .is_some_and(|length| length != expected)
    {
        return Err(io_error(
            "download Content-Length differs from the fixed manifest",
        ));
    }
    let mut file = tokio::fs::File::from_std(files::new_output(root, relative)?);
    let mut stream = response.bytes_stream();
    let started = Instant::now();
    let mut bytes = 0_u64;
    let mut hash = Context::new(&SHA256);
    loop {
        let chunk = tokio::select! {
            _ = context.cancel.cancelled() => return context.cancel.check(),
            result = tokio::time::timeout(Duration::from_secs(30), stream.next()) => result
                .map_err(|_| AppError::connectivity("model download stalled"))?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(network_error)?;
        bytes = bytes
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| io_error("download size overflow"))?;
        if bytes > expected {
            return Err(io_error("download exceeds declared size"));
        }
        file.write_all(&chunk).await.map_err(io_error)?;
        hash.update(&chunk);
        context.progress(chunk.len() as u64)?;
        let target = Duration::from_secs_f64(bytes as f64 / (16.0 * 1024.0 * 1024.0));
        if let Some(delay) = target.checked_sub(started.elapsed()) {
            tokio::select! { _ = context.cancel.cancelled() => return context.cancel.check(), _ = tokio::time::sleep(delay) => {} }
        }
    }
    let actual: String = hash
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if bytes != expected || !actual.eq_ignore_ascii_case(digest) {
        return Err(io_error("download size or SHA256 mismatch"));
    }
    file.sync_all().await.map_err(io_error)
}

async fn response(
    context: &TaskContext,
    origin: &reqwest::Client,
    direct: &reqwest::Client,
    source: &str,
) -> Result<reqwest::Response, AppError> {
    let mut url =
        reqwest::Url::parse(source).map_err(|_| AppError::configuration("invalid source URL"))?;
    for redirect in 0..=5 {
        let client = if redirect == 0 { origin } else { direct };
        let response = tokio::select! {
            _ = context.cancel.cancelled() => return Err(AppError::invalid_transition("model preparation cancelled")),
            result = tokio::time::timeout(Duration::from_secs(30), client.get(url.clone()).send()) => result
                .map_err(|_| AppError::connectivity("model download connection timed out"))?.map_err(network_error)?,
        };
        if !response.status().is_redirection() {
            return Ok(response);
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| AppError::connectivity("download redirect lacks a valid location"))?;
        url = url
            .join(location)
            .map_err(|_| AppError::connectivity("invalid download redirect"))?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(AppError::connectivity(
                "download redirect must remain HTTPS without credentials",
            ));
        }
    }
    Err(AppError::connectivity("too many model download redirects"))
}

fn network_error(error: reqwest::Error) -> AppError {
    AppError::connectivity(error.without_url().to_string())
}
