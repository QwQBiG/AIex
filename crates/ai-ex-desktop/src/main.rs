#![forbid(unsafe_code)]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod appearance;
mod appearance_import;
mod builtin_character;
mod character_files;
mod character_library;
#[cfg(test)]
mod headless_snapshot;
mod image_appearance;
mod launch;
mod memory_panel;
mod navigation;
mod portable;
mod preview;
mod scene_files;
mod scene_resume;
mod service_process;
mod session;
mod setup;
mod setup_storage;
mod speech_panel;
mod startup;
mod ui;
mod welcome;
mod worker;

use std::path::PathBuf;

use ai_ex_domain::AppError;
use navigation::Destination;

fn main() {
    if let Err(error) = run() {
        eprintln!("AIex desktop failed: {error}");
        if !std::env::args().any(|argument| argument == "--check-install") {
            let _ignored = welcome::show_error(error.to_string());
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), AppError> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let executable =
        std::env::current_exe().map_err(|error| AppError::unavailable(error.to_string()))?;
    if arguments == ["--check-install"] {
        let directory = executable
            .parent()
            .ok_or_else(|| AppError::configuration("executable has no directory"))?;
        portable::check(directory)?;
        println!(
            "{}",
            serde_json::json!({"ready": true, "version": env!("CARGO_PKG_VERSION")})
        );
        return Ok(());
    }
    let show_welcome = arguments.is_empty();
    let mut options = launch::parse_options(arguments.into_iter())?;
    let portable_root = portable::root(&executable).filter(|_| !options.config_explicit);
    if let Some(directory) = &portable_root {
        std::env::set_current_dir(directory).map_err(|error| {
            AppError::configuration(format!("cannot enter portable directory: {error}"))
        })?;
        options.config_path = PathBuf::from(portable::CONFIG);
    }
    let mut destination = if show_welcome {
        Destination::Home
    } else if options.preview {
        Destination::Preview
    } else if options.setup {
        Destination::Setup
    } else {
        Destination::Connect
    };
    let mut session = session::Session::default();
    loop {
        let next = match destination {
            Destination::Home => welcome::run(session.configured(&options.config_path))
                .map(|choice| choice.map(destination_for)),
            Destination::Preview => preview::run(options.appearance_pack.take()),
            Destination::Connect | Destination::Setup => session.connect(
                &options,
                portable_root.as_deref(),
                destination == Destination::Setup,
            ),
        };
        let next = match next {
            Ok(next) => next,
            Err(error) => welcome::recover(error.to_string())?.map(destination_for),
        };
        match next {
            Some(next) => destination = next,
            None => return Ok(()),
        }
    }
}

fn destination_for(choice: welcome::Choice) -> Destination {
    match choice {
        welcome::Choice::Preview => Destination::Preview,
        welcome::Choice::Connect => Destination::Connect,
        welcome::Choice::Setup => Destination::Setup,
    }
}
