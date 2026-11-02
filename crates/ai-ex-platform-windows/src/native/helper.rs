use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::desktop::{PixelBounds, WindowIdentity, WindowSnapshot};

mod gdi;

pub(super) fn run(arguments: &[String]) -> io::Result<()> {
    match arguments {
        [argument] if argument == "--version" => {
            println!("ai-ex-platform-helper {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [argument] if argument == "--help" => {
            println!(
                "ai-ex-platform-helper [--test-window | --version | --help]\nNo arguments opens only the dedicated input-test canvas. --capture is service-owned and does not open a window."
            );
            Ok(())
        }
        [] => super::test_window::run(),
        [argument] if argument == "--test-window" => super::test_window::run(),
        [mode, values @ ..] if mode == "--capture" => {
            let result = capture(values);
            if let Err(error) = &result
                && let Some(output) = values.last().map(PathBuf::from)
                && output.is_absolute()
                && let Some(parent) = output.parent()
                && let Ok(mut report) = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(parent.join("error.txt"))
            {
                let mut detail = error.to_string();
                let mut length = detail.len().min(2048);
                while !detail.is_char_boundary(length) {
                    length -= 1;
                }
                detail.truncate(length);
                let _written = report.write_all(detail.as_bytes());
            }
            result
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unknown helper arguments",
        )),
    }
}

pub(super) fn capture_arguments(target: &WindowSnapshot) -> Vec<String> {
    let identity = target.identity;
    let client = target.client;
    let desktop = target.desktop;
    vec![
        "--capture".into(),
        identity.handle.to_string(),
        identity.process_id.to_string(),
        identity.process_started_ticks.to_string(),
        client.left.to_string(),
        client.top.to_string(),
        client.width.to_string(),
        client.height.to_string(),
        target.dpi.to_string(),
        desktop.left.to_string(),
        desktop.top.to_string(),
        desktop.width.to_string(),
        desktop.height.to_string(),
    ]
}

fn capture(arguments: &[String]) -> io::Result<()> {
    if arguments.len() != 13 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid capture worker request",
        ));
    }
    fn integer<T: std::str::FromStr>(value: &str) -> io::Result<T> {
        value
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid capture integer"))
    }
    let identity = WindowIdentity {
        handle: integer(&arguments[0])?,
        process_id: integer(&arguments[1])?,
        process_started_ticks: integer(&arguments[2])?,
    };
    let client = PixelBounds {
        left: integer(&arguments[3])?,
        top: integer(&arguments[4])?,
        width: integer(&arguments[5])?,
        height: integer(&arguments[6])?,
    };
    let dpi = integer(&arguments[7])?;
    let desktop = PixelBounds {
        left: integer(&arguments[8])?,
        top: integer(&arguments[9])?,
        width: integer(&arguments[10])?,
        height: integer(&arguments[11])?,
    };
    let output = PathBuf::from(&arguments[12]);
    if !output.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "absolute capture output required",
        ));
    }
    let expected = WindowSnapshot {
        identity,
        client,
        desktop,
        dpi,
        foreground: true,
        title: String::new(),
        executable: String::new(),
    };
    let confirm = || -> io::Result<()> {
        let observed = super::inspect::inspect(identity.handle)?;
        if !observed.as_ref().is_some_and(|current| {
            current.identity == identity
                && current.client == client
                && current.desktop == desktop
                && current.dpi == dpi
                && current.foreground
        }) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "capture target changed",
            ));
        }
        Ok(())
    };
    confirm()?;
    let frame = gdi::capture(&expected)?;
    confirm()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    file.write_all(b"AEXCAP01")?;
    file.write_all(&frame.width.to_le_bytes())?;
    file.write_all(&frame.height.to_le_bytes())?;
    file.write_all(&frame.rgba)
}
