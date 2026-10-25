use std::path::Path;

use ai_ex_domain::AppError;

use crate::navigation::{Destination, Navigation};
use crate::{app::DesktopApp, launch::LaunchOptions, portable, scene_resume, setup, startup};
use crate::{
    service_process::ManagedService,
    worker::{WorkerSettings, spawn_worker},
};

#[derive(Default)]
pub struct Session {
    credential: Option<Credential>,
}

struct Credential {
    config: String,
    key: String,
}

impl Session {
    fn key_for(&self, path: &Path) -> Option<&str> {
        let credential = self.credential.as_ref()?;
        let config = std::fs::read_to_string(path).ok()?;
        (config == credential.config).then_some(credential.key.as_str())
    }

    pub fn configured(&self, path: &Path) -> bool {
        portable::configured(path)
            || (self.key_for(path).is_some()
                && startup::read_config(path)
                    .is_ok_and(|config| startup::read_token(&config).is_ok()))
    }

    pub fn connect(
        &mut self,
        options: &LaunchOptions,
        portable_root: Option<&Path>,
        edit_settings: bool,
    ) -> Result<Option<Destination>, AppError> {
        if let Some(root) = portable_root {
            portable::check(root)?;
        }
        let navigation = Navigation::default();
        let config_path = &options.config_path;
        if edit_settings
            || !config_path.exists()
            || (!options.connect_only && !self.configured(config_path))
        {
            let Some(result) = setup::run(config_path.clone(), navigation.clone())? else {
                return Ok(navigation.take());
            };
            // Credentials remain in this process and only match the exact saved configuration.
            self.credential = result
                .api_key
                .map(|key| {
                    std::fs::read_to_string(&result.config_path)
                        .map(|config| Credential { config, key })
                })
                .transpose()
                .map_err(|error| AppError::configuration(error.to_string()))?;
        }
        let config = startup::read_config(config_path)?;
        if !config.control.enabled {
            return Err(AppError::configuration(
                "desktop requires control.enabled = true",
            ));
        }
        let token = startup::read_token(&config)?;
        let auto_start =
            !options.connect_only && (options.start_service || config.desktop.auto_start_service);
        let managed_service = if auto_start && !startup::service_is_running(&config, &token)? {
            Some(ManagedService::spawn(
                config_path,
                self.key_for(config_path),
                &config.deepseek.api_key_env,
            )?)
        } else {
            None
        };
        let resume = scene_resume::ResumeLaunch::new(
            config_path,
            &config.control.bind,
            managed_service.is_some(),
        )?;
        let worker = spawn_worker(WorkerSettings {
            address: config.control.bind,
            token,
            max_message_bytes: config.control.max_message_bytes,
        })?;
        let developer = options.developer;
        let result = navigation.clone();
        eframe::run_native(
            "AIex",
            crate::navigation::companion_window([1120.0, 760.0], [720.0, 520.0]),
            Box::new(move |context| {
                Ok(Box::new(
                    DesktopApp::new(context, worker, developer, resume)
                        .with_navigation(navigation)
                        .with_service(managed_service),
                ))
            }),
        )
        .map_err(|error| AppError::unavailable(error.to_string()))?;
        // The window and its owned service are dropped before the next destination opens.
        Ok(result.take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_credential_is_reused_only_for_its_saved_configuration() {
        let directory =
            std::env::temp_dir().join(format!("companion-session-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("session.toml");
        let config = "[model]\nbackend = 'deepseek'\n";
        std::fs::write(&path, config).unwrap();
        let session = Session {
            credential: Some(Credential {
                config: config.to_owned(),
                key: "test-transient-key".to_owned(),
            }),
        };
        assert_eq!(session.key_for(&path), Some("test-transient-key"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), config);
        std::fs::write(&path, "[model]\nbackend = 'ollama'\n").unwrap();
        assert_eq!(session.key_for(&path), None);
        assert_eq!(Session::default().key_for(&path), None);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
