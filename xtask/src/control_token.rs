//! Create or explicitly rotate a local control token.

use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};

pub(crate) fn create(path: &Path, rotate: bool) -> Result<(Value, bool), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create token directory: {error}"))?;
    }
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "system random number generator failed".to_owned())?;
    let token = STANDARD.encode(bytes);
    let mut options = OpenOptions::new();
    options.write(true);
    if rotate {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        if !rotate && error.kind() == ErrorKind::AlreadyExists {
            format!(
                "Token already exists at {}. Specify rotation only when intentional.",
                path.display()
            )
        } else {
            format!("cannot open token {}: {error}", path.display())
        }
    })?;
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    file.write_all(token.as_bytes())
        .and_then(|_| file.write_all(newline.as_bytes()))
        .map_err(|error| format!("cannot write token {}: {error}", path.display()))?;
    Ok((
        json!({"status": "passed", "path": path, "allow_rotation": rotate}),
        true,
    ))
}

#[cfg(test)]
#[path = "control_token_tests.rs"]
mod tests;
