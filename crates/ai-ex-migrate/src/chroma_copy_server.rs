use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use ai_ex_domain::AppError;
use ai_ex_platform_windows::{OwnedJobProcess, ProcessLaunch};

use super::files::CheckedCopy;
use super::{invalid, unavailable};

fn database_argument(path: &std::path::Path) -> Result<OsString, AppError> {
    let value = path
        .to_str()
        .ok_or_else(|| invalid("native Chroma database path must be UTF-8"))?;
    // Native HNSW appends slash-separated file names, which verbatim Win32 paths reject.
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        Ok(format!(r"\\{unc}").into())
    } else {
        Ok(value.strip_prefix(r"\\?\").unwrap_or(value).into())
    }
}

fn environment(
    home: &std::path::Path,
    temporary: &std::path::Path,
) -> BTreeMap<OsString, OsString> {
    let mut values = BTreeMap::new();
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            values.insert(name.into(), value);
        }
    }
    for name in ["HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA"] {
        values.insert(name.into(), home.as_os_str().to_owned());
    }
    for name in ["TEMP", "TMP"] {
        values.insert(name.into(), temporary.as_os_str().to_owned());
    }
    values.insert("ANONYMIZED_TELEMETRY".into(), "FALSE".into());
    values.insert("TOKIO_WORKER_THREADS".into(), "1".into());
    values
}

async fn ready(
    process: &OwnedJobProcess,
    client: &reqwest::Client,
    port: u16,
) -> Result<(), AppError> {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(code) = process
            .exit_code()
            .map_err(|e| unavailable("cannot inspect owned native server", e))?
        {
            return Err(invalid(format!(
                "native Chroma server exited during startup ({code}); the working copy is retained"
            )));
        }
        if process
            .owns_loopback_listener(port)
            .map_err(|e| unavailable("cannot verify native server listener", e))?
            && client
                .get(format!("http://127.0.0.1:{port}/api/v2/healthcheck"))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(invalid(
                "native Chroma server did not become ready within 45 seconds; the working copy is retained",
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn stop(process: &OwnedJobProcess) -> Result<(), AppError> {
    process
        .terminate()
        .map_err(|e| unavailable("cannot stop owned native server", e))?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while process
        .active_processes()
        .map_err(|e| unavailable("cannot inspect owned server shutdown", e))?
        > 0
    {
        if Instant::now() >= deadline {
            return Err(invalid("owned native server shutdown timed out"));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

pub(super) async fn export(checked: &CheckedCopy) -> Result<(), AppError> {
    let reservation = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| unavailable("cannot reserve native server port", e))?;
    let port = reservation
        .local_addr()
        .map_err(|e| unavailable("cannot inspect server port", e))?
        .port();
    let options = &checked.options;
    let runtime = options
        .working_copy
        .join(format!(".native-export-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir(&runtime)
        .await
        .map_err(|error| unavailable("cannot create private native runtime directory", error))?;
    let launch = ProcessLaunch {
        executable: options.native_server.clone(),
        arguments: vec![
            "run".into(),
            "--path".into(),
            database_argument(&options.working_copy)?,
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string().into(),
        ],
        directory: options.working_copy.clone(),
        environment: environment(&runtime, &runtime),
    };
    drop(reservation);
    let process = OwnedJobProcess::spawn(launch)
        .map_err(|e| unavailable("cannot launch owned native server", e))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(1))
        .build()
        .map_err(|e| unavailable("cannot create native server readiness client", e))?;
    let result = async {
        ready(&process, &client, port).await?;
        crate::chroma_bridge::export_silently(crate::chroma_bridge::ExportOptions {
            url: reqwest::Url::parse(&format!("http://127.0.0.1:{port}"))
                .map_err(|_| invalid("invalid owned server URL"))?,
            collection: options.collection.clone(),
            tenant: options.tenant.clone(),
            database: options.database.clone(),
            entity_collection: options.entity_collection.clone(),
            output: checked.staging.clone(),
        })
        .await
    }
    .await;
    // Never terminate a PID discovered from a port; only this owned job is controlled.
    let stopped = stop(&process).await;
    result?;
    stopped
}
