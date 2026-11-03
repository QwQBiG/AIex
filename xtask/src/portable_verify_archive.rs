//! Inspect portable archive names and entries before any extraction.

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ArchiveLayout {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) requires_migrator: bool,
    pub(crate) requires_license: bool,
    pub(crate) requires_platform: bool,
    pub(crate) file_count: usize,
}

pub(crate) fn inspect_path(path: &Path) -> Result<(Value, bool), String> {
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("invalid archive filename")?;
    let file =
        std::fs::File::open(path).map_err(|error| format!("cannot open archive: {error}"))?;
    let layout = inspect(file, filename)?;
    Ok((
        json!({"status": "passed", "scope": "archive structure only", "name": layout.name, "version": layout.version, "file_count": layout.file_count, "requires_migrator": layout.requires_migrator, "requires_license": layout.requires_license, "requires_platform": layout.requires_platform}),
        true,
    ))
}

fn version_parts(filename: &str) -> Result<(String, [u32; 3]), String> {
    let version = filename
        .strip_prefix("AIex-Windows-x64-")
        .and_then(|value| value.strip_suffix(".zip"))
        .ok_or("Archive name must be AIex-Windows-x64-VERSION.zip")?;
    let (without_build, build) = match version.split_once('+') {
        Some((base, suffix)) => (base, Some(suffix)),
        None => (version, None),
    };
    let (numbers, prerelease) = match without_build.split_once('-') {
        Some((base, suffix)) => (base, Some(suffix)),
        None => (without_build, None),
    };
    for suffix in [prerelease, build].into_iter().flatten() {
        if suffix.is_empty()
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte))
        {
            return Err("Invalid archive version suffix".into());
        }
    }
    let components: Vec<_> = numbers.split('.').collect();
    if components.len() != 3 {
        return Err("Archive version must have three numeric components".into());
    }
    let mut result = [0; 3];
    for (target, part) in result.iter_mut().zip(components) {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Archive version must have three numeric components".into());
        }
        *target = part
            .parse::<i32>()
            .map_err(|_| "Archive version is out of range")? as u32;
    }
    Ok((version.to_owned(), result))
}

fn expected_files(version: [u32; 3]) -> BTreeSet<&'static str> {
    let mut files = BTreeSet::from([
        "AIex.exe",
        "ai-ex-service.exe",
        "AIex.portable",
        "开始使用.txt",
        "data/ai-ex.local.toml",
    ]);
    if version >= [0, 8, 0] {
        files.insert("ai-ex-migrate.exe");
    }
    if version >= [0, 9, 0] {
        files.extend(["LICENSE.txt", "THIRD-PARTY-LICENSES.txt"]);
    }
    if version >= [0, 10, 0] {
        files.extend(["ai-ex-platform-helper.exe", "AIex-Test-Window.cmd"]);
    }
    files
}

fn check_directory_entries<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    count: usize,
) -> Result<(), String> {
    reader
        .seek(SeekFrom::Start(start))
        .map_err(|e| format!("Cannot read ZIP directory: {e}"))?;
    let mut seen = 0usize;
    loop {
        let mut signature = [0; 4];
        reader
            .read_exact(&mut signature)
            .map_err(|e| format!("Invalid ZIP directory: {e}"))?;
        if signature != [0x50, 0x4b, 0x01, 0x02] {
            break;
        }
        seen += 1;
        if seen > count {
            return Err("Archive contains duplicate entries".into());
        }
        let mut header = [0u8; 42];
        reader
            .read_exact(&mut header)
            .map_err(|e| format!("Invalid ZIP directory entry: {e}"))?;
        let length = [24, 26, 28]
            .into_iter()
            .map(|i| u16::from_le_bytes([header[i], header[i + 1]]) as i64)
            .sum();
        reader
            .seek(SeekFrom::Current(length))
            .map_err(|e| format!("Invalid ZIP directory entry: {e}"))?;
    }
    if seen != count {
        return Err("ZIP directory entry count differs".into());
    }
    Ok(())
}

pub(crate) fn inspect<R: Read + Seek>(
    mut reader: R,
    filename: &str,
) -> Result<ArchiveLayout, String> {
    let (version, numeric) = version_parts(filename)?;
    let name = filename
        .strip_suffix(".zip")
        .ok_or("Invalid archive filename")?;
    let expected = expected_files(numeric);
    let mut files = BTreeSet::new();
    let archive =
        zip::ZipArchive::new(&mut reader).map_err(|error| format!("Invalid ZIP: {error}"))?;
    let directory_start = archive.central_directory_start();
    let entry_count = archive.len();
    drop(archive);
    // ZipArchive indexes by name, so identical names otherwise disappear.
    check_directory_entries(&mut reader, directory_start, entry_count)?;
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|error| format!("Invalid ZIP: {error}"))?;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| format!("Invalid ZIP entry: {error}"))?;
        if entry.is_symlink() {
            return Err("Archive contains a symbolic link".into());
        }
        let path = entry.name().replace('\\', "/");
        if path.contains('\0')
            || !path.starts_with(&format!("{name}/"))
            || path.split('/').any(|part| part == "." || part == "..")
        {
            return Err("Archive entry is outside the expected package directory".into());
        }
        if entry.is_dir() || path.ends_with('/') {
            if path != format!("{name}/") && path != format!("{name}/data/") {
                return Err("Archive contains an unexpected directory".into());
            }
            continue;
        }
        let relative = &path[name.len() + 1..];
        if !expected.contains(relative) || !files.insert(relative.to_owned()) {
            return Err("Archive contains an unexpected or duplicate file".into());
        }
    }
    if files.len() != expected.len() {
        return Err("Archive is missing required package files".into());
    }
    Ok(ArchiveLayout {
        name: name.to_owned(),
        version,
        requires_migrator: numeric >= [0, 8, 0],
        requires_license: numeric >= [0, 9, 0],
        requires_platform: numeric >= [0, 10, 0],
        file_count: files.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn archive(entries: &[String]) -> Cursor<Vec<u8>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in entries {
            writer
                .start_file(name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"fixture").unwrap();
        }
        let mut data = writer.finish().unwrap();
        data.set_position(0);
        data
    }

    fn entries(filename: &str, version: [u32; 3]) -> Vec<String> {
        let root = filename.strip_suffix(".zip").unwrap();
        expected_files(version)
            .into_iter()
            .map(|file| format!("{root}/{file}"))
            .collect()
    }

    #[test]
    fn historical_layouts_follow_numeric_release_line() {
        for (filename, numeric, count, migrator, licenses, platform) in [
            (
                "AIex-Windows-x64-0.7.1-alpha.1.zip",
                [0, 7, 1],
                5,
                false,
                false,
                false,
            ),
            (
                "AIex-Windows-x64-0.8.0-alpha.1.zip",
                [0, 8, 0],
                6,
                true,
                false,
                false,
            ),
            (
                "AIex-Windows-x64-0.9.0+build.1.zip",
                [0, 9, 0],
                8,
                true,
                true,
                false,
            ),
            (
                "AIex-Windows-x64-0.10.0-beta.1.zip",
                [0, 10, 0],
                10,
                true,
                true,
                true,
            ),
        ] {
            let result = inspect(archive(&entries(filename, numeric)), filename).unwrap();
            assert_eq!(result.file_count, count);
            assert_eq!(result.requires_migrator, migrator);
            assert_eq!(result.requires_license, licenses);
            assert_eq!(result.requires_platform, platform);
        }
    }

    #[test]
    fn malformed_versions_and_archive_names_are_rejected() {
        for name in [
            "AIex-Windows-x64-0.10.zip",
            "AIex-Windows-x64-0.10.0-.zip",
            "AIex-Windows-x64-0.10.0+build+extra.zip",
            "AIex-Windows-x64-0.10.0/a.zip",
            "AIex-Windows-x64-2147483648.0.0.zip",
            "other-0.10.0.zip",
        ] {
            assert!(version_parts(name).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_duplicates_missing_and_undeclared_files() {
        let filename = "AIex-Windows-x64-0.10.0-alpha.1.zip";
        let baseline = entries(filename, [0, 10, 0]);
        let mut missing = baseline.clone();
        missing.pop();
        assert!(
            inspect(archive(&missing), filename)
                .unwrap_err()
                .contains("missing")
        );
        let mut duplicate = baseline.clone();
        duplicate.push("AIex-Windows-x64-0.10.0-alpha.1\\AIex.exe".into());
        assert!(
            inspect(archive(&duplicate), filename)
                .unwrap_err()
                .contains("duplicate")
        );
        let mut extra = baseline;
        extra.push("AIex-Windows-x64-0.10.0-alpha.1/data/private.json".into());
        assert!(
            inspect(archive(&extra), filename)
                .unwrap_err()
                .contains("unexpected")
        );
    }

    #[test]
    fn rejects_identical_names_even_if_zip_reader_deduplicates_them() {
        let filename = "AIex-Windows-x64-0.10.0-alpha.1.zip";
        let mut files = entries(filename, [0, 10, 0]);
        files.push("AIex-Windows-x64-0.10.0-alpha.1/BIex.exe".into());
        let mut bytes = archive(&files).into_inner();
        let old = b"BIex.exe";
        let new = b"AIex.exe";
        let mut replacements = 0;
        for index in 0..=bytes.len() - old.len() {
            if &bytes[index..index + old.len()] == old {
                bytes[index..index + old.len()].copy_from_slice(new);
                replacements += 1;
            }
        }
        assert_eq!(replacements, 2);
        assert!(
            inspect(Cursor::new(bytes), filename)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn rejects_traversal_and_wrong_root_before_extraction() {
        let filename = "AIex-Windows-x64-0.10.0-alpha.1.zip";
        let root = filename.strip_suffix(".zip").unwrap();
        for path in [
            format!("{root}/../outside"),
            format!("{root}\\..\\outside"),
            format!("{root}/data/./ai-ex.local.toml"),
            "different-root/AIex.exe".into(),
        ] {
            let mut data = entries(filename, [0, 10, 0]);
            data.push(path);
            assert!(
                inspect(archive(&data), filename)
                    .unwrap_err()
                    .contains("outside")
            );
        }
    }
}
