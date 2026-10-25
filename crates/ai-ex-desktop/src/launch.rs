use std::path::PathBuf;

use ai_ex_domain::AppError;

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;

pub(crate) struct LaunchOptions {
    pub(crate) config_path: PathBuf,
    pub(crate) config_explicit: bool,
    pub(crate) setup: bool,
    pub(crate) developer: bool,
    pub(crate) preview: bool,
    pub(crate) start_service: bool,
    pub(crate) connect_only: bool,
    pub(crate) appearance_pack: Option<PathBuf>,
}

pub(crate) fn parse_options(
    arguments: impl Iterator<Item = String>,
) -> Result<LaunchOptions, AppError> {
    let mut config_path = PathBuf::from("config/ai-ex.local.toml");
    let mut config_explicit = false;
    let mut setup = false;
    let mut developer = false;
    let mut preview = false;
    let mut start_service = false;
    let mut connect_only = false;
    let mut appearance_pack = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => {
                config_explicit = true;
                config_path = PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| AppError::configuration("--config requires a path"))?,
                );
            }
            "--setup" => setup = true,
            "--preview" => preview = true,
            "--appearance-pack" => {
                appearance_pack = Some(PathBuf::from(arguments.next().ok_or_else(|| {
                    AppError::configuration("--appearance-pack requires a manifest path")
                })?));
            }
            "--start-service" => start_service = true,
            "--connect-only" => connect_only = true,
            "--developer" | "--dev" => developer = true,
            "--help" | "-h" => {
                return Err(AppError::configuration(
                    "usage: ai-ex-desktop [--config PATH] [--setup] [--developer] [--start-service | --connect-only] [--preview [--appearance-pack PATH]]",
                ));
            }
            _ => {
                return Err(AppError::configuration(format!(
                    "unknown desktop argument: {argument}",
                )));
            }
        }
    }
    if start_service && connect_only {
        return Err(AppError::configuration(
            "--start-service and --connect-only are mutually exclusive",
        ));
    }
    if appearance_pack.is_some() && !preview {
        return Err(AppError::configuration(
            "--appearance-pack requires --preview",
        ));
    }
    if preview && (setup || start_service || connect_only) {
        return Err(AppError::configuration(
            "--preview cannot be combined with --setup, --start-service, or --connect-only",
        ));
    }
    Ok(LaunchOptions {
        config_path,
        config_explicit,
        setup,
        developer,
        preview,
        start_service,
        connect_only,
        appearance_pack,
    })
}
