use std::path::{Path, PathBuf};

use ai_ex_config::AppConfig;
use ai_ex_domain::AppError;

use crate::navigation::{Destination, Navigation};
use crate::service_process::ManagedService;

struct Bootstrap {
    path: PathBuf,
    native_path: PathBuf,
    config: AppConfig,
    token: String,
}

fn prepare(legacy_path: &Path) -> Result<Bootstrap, AppError> {
    let legacy = std::path::absolute(legacy_path)
        .map_err(|error| AppError::configuration(error.to_string()))?;
    let directory = legacy
        .parent()
        .ok_or_else(|| AppError::configuration("configuration requires a directory"))?;
    let path = directory.join("native-bootstrap.toml");
    let native_path = directory.join(ai_ex_config::native::NATIVE_CONFIG_FILENAME);
    let config = if path.exists() {
        crate::startup::read_config(&path)?
    } else {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| {
            AppError::unavailable(format!("cannot reserve native control address: {error}"))
        })?;
        let mut config = AppConfig::default();
        config.control.enabled = true;
        config.control.bind = listener
            .local_addr()
            .map_err(|error| AppError::unavailable(error.to_string()))?
            .to_string();
        config.control.max_message_bytes = 4 * 1024 * 1024;
        config.control.token_path = directory
            .join("native-control.token")
            .to_string_lossy()
            .into_owned();
        config.model.backend = ai_ex_config::ModelBackend::Ollama;
        config.vts.enabled = false;
        config.duplex.enabled = false;
        config.tts.enabled = false;
        config.memory.path = directory
            .join("native-memory.jsonl")
            .to_string_lossy()
            .into_owned();
        config.safety.audit_path = directory
            .join("native-automation.jsonl")
            .to_string_lossy()
            .into_owned();
        crate::setup_storage::save(&path, &config.to_toml()?, None)?;
        config
    };
    if !config.control.enabled || config.control.max_message_bytes < 4 * 1024 * 1024 {
        return Err(AppError::configuration(
            "native-bootstrap.toml requires control enabled and a 4 MiB message limit; existing file was preserved",
        ));
    }
    let token = crate::startup::read_token(&config)?;
    Ok(Bootstrap {
        path,
        native_path,
        config,
        token,
    })
}

pub fn run(
    options: &crate::launch::LaunchOptions,
    start_in_chat: bool,
) -> Result<Option<Destination>, AppError> {
    let bootstrap = prepare(&options.config_path)?;
    let service = if crate::startup::service_is_running(&bootstrap.config, &bootstrap.token)? {
        None
    } else {
        Some(ManagedService::spawn_native(
            &bootstrap.path,
            &bootstrap.native_path,
        )?)
    };
    let launch = crate::scene_resume::ResumeLaunch::new(
        &bootstrap.path,
        &bootstrap.config.control.bind,
        service.is_some(),
    )?;
    let worker = crate::worker::spawn_worker(crate::worker::WorkerSettings {
        address: bootstrap.config.control.bind,
        token: bootstrap.token,
        max_message_bytes: bootstrap.config.control.max_message_bytes,
    })?;
    let navigation = Navigation::default();
    let result = navigation.clone();
    let developer = options.developer;
    eframe::run_native(
        "AIex · 准备本地伙伴",
        crate::navigation::companion_window([1120.0, 760.0], [720.0, 520.0]),
        Box::new(move |context| {
            Ok(Box::new(
                crate::app::DesktopApp::new(context, worker, developer, launch)
                    .with_navigation(navigation)
                    .with_service(service)
                    .with_native_setup(start_in_chat),
            ))
        }),
    )
    .map_err(|error| AppError::unavailable(error.to_string()))?;
    Ok(result.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_bootstrap_is_separate_and_reuses_original_files() {
        let root = std::env::temp_dir().join(format!("native-bootstrap-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let legacy = root.join("personal.toml");
        std::fs::write(&legacy, "personal configuration must remain byte-identical").unwrap();
        let first = prepare(&legacy).unwrap();
        let before = std::fs::read(&first.path).unwrap();
        let second = prepare(&legacy).unwrap();
        assert_eq!(first.token, second.token);
        assert_eq!(std::fs::read(&first.path).unwrap(), before);
        assert!(!first.native_path.exists());
        assert_eq!(
            std::fs::read_to_string(&legacy).unwrap(),
            "personal configuration must remain byte-identical"
        );
        assert!(
            !first.config.duplex.enabled && !first.config.tts.enabled && !first.config.vts.enabled
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
