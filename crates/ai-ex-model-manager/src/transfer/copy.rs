use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;

use ai_ex_domain::AppError;

use crate::files::{self, io_error};
use crate::manager::{Package, TaskContext};

pub(crate) fn import(
    context: &TaskContext,
    package: &Package,
    source: &Path,
    data: &Path,
) -> Result<(), AppError> {
    let metadata = fs::symlink_metadata(source).map_err(io_error)?;
    if files::is_link(&metadata) {
        return Err(io_error("offline input cannot be a link"));
    }
    if metadata.is_dir() {
        let source = if source.join("manifest.json").is_file() && source.join("data").is_dir() {
            files::checked_path(source, "data")?
        } else {
            source.to_path_buf()
        };
        files::verify_inventory(package.artifacts(), &source)?;
        for artifact in package.artifacts() {
            context.cancel.check()?;
            let path = files::checked_path(&source, &artifact.path)?;
            let mut input = File::open(path).map_err(io_error)?;
            let metadata = input.metadata().map_err(io_error)?;
            if !metadata.is_file() || metadata.len() != artifact.bytes {
                return Err(io_error("offline artifact size mismatch"));
            }
            let mut output = files::new_output(data, &artifact.path)?;
            copy(context, &mut input, &mut output, artifact.bytes)?;
            output.sync_all().map_err(io_error)?;
        }
    } else if metadata.is_file()
        && source
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case("zip"))
    {
        super::archive::extract(context, package, source, data)?;
    } else {
        return Err(io_error("offline input must be a package directory or ZIP"));
    }
    Ok(())
}

pub(crate) fn copy(
    context: &TaskContext,
    input: &mut impl Read,
    output: &mut impl Write,
    expected: u64,
) -> Result<(), AppError> {
    let mut buffer = [0_u8; 65_536];
    let mut total = 0_u64;
    loop {
        context.cancel.check()?;
        let count = input.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| io_error("offline artifact size overflow"))?;
        if total > expected {
            return Err(io_error("offline artifact exceeds declared length"));
        }
        output.write_all(&buffer[..count]).map_err(io_error)?;
        context.progress(count as u64)?;
    }
    if total != expected {
        return Err(io_error("offline artifact is shorter than declared"));
    }
    Ok(())
}
