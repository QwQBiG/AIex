use std::collections::HashSet;
use std::fs::File;
use std::path::Path;

use ai_ex_domain::AppError;
use ai_ex_model_catalog::validate_relative_path;
use zip::ZipArchive;

use crate::files::{self, io_error};
use crate::manager::{Package, TaskContext};

pub(super) fn extract(
    context: &TaskContext,
    package: &Package,
    archive: &Path,
    data: &Path,
) -> Result<(), AppError> {
    let input = File::open(archive).map_err(io_error)?;
    let expected: u64 = package.artifacts().iter().map(|file| file.bytes).sum();
    if input.metadata().map_err(io_error)?.len()
        > expected.saturating_mul(2).saturating_add(16 * 1024 * 1024)
    {
        return Err(io_error("offline ZIP exceeds its package size allowance"));
    }
    let mut archive = ZipArchive::new(input).map_err(io_error)?;
    if archive.len() > package.artifacts().len() + 10_000 {
        return Err(io_error("ZIP contains too many entries"));
    }
    let mut seen = HashSet::new();
    for index in 0..archive.len() {
        context.cancel.check()?;
        let mut entry = archive.by_index(index).map_err(io_error)?;
        let name = entry.name().to_owned();
        let relative = name.strip_suffix('/').unwrap_or(&name);
        validate_relative_path(relative)?;
        if !seen.insert(relative.to_uppercase()) {
            return Err(io_error("ZIP has duplicate or case-colliding paths"));
        }
        let kind = entry.unix_mode().map(|mode| mode & 0o170000).unwrap_or(0);
        if !matches!(kind, 0 | 0o100000 | 0o040000) {
            return Err(io_error("ZIP links and special files are not permitted"));
        }
        if entry.is_dir() {
            if !package
                .artifacts()
                .iter()
                .any(|artifact| artifact.path.starts_with(&format!("{relative}/")))
            {
                return Err(io_error("ZIP contains an undeclared directory"));
            }
            files::create_directory(data, relative)?;
            continue;
        }
        let artifact = package
            .artifacts()
            .iter()
            .find(|artifact| artifact.path == relative)
            .ok_or_else(|| io_error("ZIP contains an undeclared file"))?;
        if entry.size() != artifact.bytes {
            return Err(io_error("ZIP entry size differs from the catalog"));
        }
        let mut output = files::new_output(data, relative)?;
        super::copy::copy(context, &mut entry, &mut output, artifact.bytes)?;
        output.sync_all().map_err(io_error)?;
    }
    for artifact in package.artifacts() {
        if !seen.contains(&artifact.path.to_uppercase()) {
            return Err(io_error("ZIP is missing a required file"));
        }
    }
    Ok(())
}
