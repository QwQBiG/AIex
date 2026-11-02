use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ring::digest::{Context, SHA256};
use uuid::Uuid;

use super::{ChromaCopyExportOptions, invalid, unavailable};
use ai_ex_domain::AppError;

pub(super) struct LockedFile {
    relative: PathBuf,
    handle: File,
    pub length: u64,
    modified: SystemTime,
    digest: [u8; 32],
}

pub(super) struct CheckedCopy {
    pub options: ChromaCopyExportOptions,
    pub staging: PathBuf,
    pub files: Vec<LockedFile>,
    paths: Vec<PathBuf>,
    // The verified server bytes cannot be replaced while the native process runs.
    _server: File,
}

fn linked(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn reject_links(path: &Path) -> Result<(), AppError> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|error| unavailable("cannot inspect snapshot path", error))?;
        if linked(&metadata) {
            return Err(invalid(
                "snapshot paths must not contain links or reparse points",
            ));
        }
    }
    Ok(())
}

fn new_destination(path: &Path) -> Result<PathBuf, AppError> {
    let absolute =
        std::path::absolute(path).map_err(|error| unavailable("cannot resolve output", error))?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(invalid("copy and snapshot destinations must be new")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unavailable("cannot inspect destination", error)),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| invalid("destination requires a parent directory"))?;
    reject_links(parent)?;
    let parent = parent
        .canonicalize()
        .map_err(|error| unavailable("destination parent must exist", error))?;
    Ok(parent.join(
        absolute
            .file_name()
            .ok_or_else(|| invalid("destination requires a file name"))?,
    ))
}

fn open_locked(path: &Path) -> Result<File, AppError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: reject writers and deletion for this lease.
    }
    let file = options.open(path).map_err(|error| {
        unavailable(
            "source is occupied or unreadable; close the legacy application first",
            error,
        )
    })?;
    if ai_ex_platform_windows::file_link_count(&file)
        .map_err(|error| unavailable("cannot inspect file identity", error))?
        != 1
    {
        return Err(invalid(
            "snapshot and native server files must not be hard linked",
        ));
    }
    Ok(file)
}

fn digest(file: &mut File) -> Result<[u8; 32], AppError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|e| unavailable("cannot rewind snapshot file", e))?;
    let mut context = Context::new(&SHA256);
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|e| unavailable("cannot read snapshot file", e))?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }
    Ok(context.finish().as_ref().try_into().expect("SHA256 length"))
}

fn inventory(root: &Path) -> Result<Vec<PathBuf>, AppError> {
    let mut pending = vec![PathBuf::new()];
    let mut entries = Vec::new();
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(root.join(&relative))
            .map_err(|e| unavailable("cannot enumerate source snapshot", e))?
        {
            let entry = entry.map_err(|e| unavailable("cannot enumerate source entry", e))?;
            let path = relative.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| unavailable("cannot inspect source entry", e))?;
            if linked(&metadata) || (!metadata.is_file() && !metadata.is_dir()) {
                return Err(invalid(
                    "source snapshot contains a link or unsupported file",
                ));
            }
            entries.push(path.clone());
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    entries.sort();
    Ok(entries)
}

pub(super) fn prepare(mut options: ChromaCopyExportOptions) -> Result<CheckedCopy, AppError> {
    if [
        &options.source,
        &options.working_copy,
        &options.native_server,
        &options.output,
    ]
    .into_iter()
    .any(|path| path.as_os_str().is_empty())
    {
        return Err(invalid(
            "source, working copy, native server and snapshot paths are required",
        ));
    }
    for name in [&options.collection, &options.tenant, &options.database]
        .into_iter()
        .chain(options.entity_collection.iter())
    {
        crate::chroma_bridge::validate_name(name)?;
    }
    if options.entity_collection.as_ref() == Some(&options.collection) {
        return Err(invalid("main and entity collections must differ"));
    }
    reject_links(
        &std::path::absolute(&options.source)
            .map_err(|e| unavailable("cannot resolve source", e))?,
    )?;
    options.source = options
        .source
        .canonicalize()
        .map_err(|e| unavailable("cannot open source snapshot", e))?;
    if !options.source.is_dir() {
        return Err(invalid("source snapshot must be a directory"));
    }
    if !options.source.join("chroma.sqlite3").is_file() {
        return Err(invalid(
            "closed Chroma snapshot must contain chroma.sqlite3",
        ));
    }
    options.working_copy = new_destination(&options.working_copy)?;
    options.output = new_destination(&options.output)?;
    if options.working_copy.starts_with(&options.source)
        || options.source.starts_with(&options.working_copy)
        || options.output.starts_with(&options.source)
        || options.output.starts_with(&options.working_copy)
        || options.output == options.working_copy
    {
        return Err(invalid(
            "source, working copy and snapshot destinations must be separate",
        ));
    }
    reject_links(
        &std::path::absolute(&options.native_server)
            .map_err(|e| unavailable("cannot resolve native server", e))?,
    )?;
    options.native_server = options
        .native_server
        .canonicalize()
        .map_err(|e| unavailable("cannot resolve native server", e))?;
    let mut native = open_locked(&options.native_server)?;
    let native_hash = digest(&mut native)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    const PINNED: &str = "71884ca430ae3118d46d51a10e733f77100aff368cc0cd64363a602d1ecdeb35";
    if !options.server_sha256.eq_ignore_ascii_case(PINNED) || native_hash != PINNED {
        return Err(invalid(
            "native server must match the pinned Chroma cli-1.3.0 Windows x64 SHA256",
        ));
    }
    options.server_sha256 = native_hash;
    let paths = inventory(&options.source)?;
    let mut files = Vec::new();
    for relative in &paths {
        let path = options.source.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| unavailable("source changed during inspection", e))?;
        if linked(&metadata) {
            return Err(invalid("source changed to a link during inspection"));
        }
        if metadata.is_dir() {
            continue;
        }
        let mut handle = open_locked(&path)?;
        let metadata = handle
            .metadata()
            .map_err(|e| unavailable("cannot inspect locked source", e))?;
        if linked(&metadata) {
            return Err(invalid("locked source is a reparse point"));
        }
        files.push(LockedFile {
            relative: relative.clone(),
            length: metadata.len(),
            modified: metadata
                .modified()
                .map_err(|e| unavailable("cannot inspect source timestamp", e))?,
            digest: digest(&mut handle)?,
            handle,
        });
    }
    let staging = options
        .output
        .with_file_name(format!(".chroma-snapshot-{}.json", Uuid::new_v4()));
    let mut checked = CheckedCopy {
        options,
        staging,
        files,
        paths,
        _server: native,
    };
    checked.verify_source()?;
    fs::create_dir(&checked.options.working_copy)
        .map_err(|e| unavailable("cannot create new working copy", e))?;
    for relative in &checked.paths {
        if checked.options.source.join(relative).is_dir() {
            fs::create_dir_all(checked.options.working_copy.join(relative))
                .map_err(|e| unavailable("cannot create copied directory", e))?;
        }
    }
    for source in &mut checked.files {
        let mut copy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(checked.options.working_copy.join(&source.relative))
            .map_err(|e| unavailable("cannot create working copy file", e))?;
        source
            .handle
            .seek(SeekFrom::Start(0))
            .map_err(|e| unavailable("cannot rewind source", e))?;
        std::io::copy(&mut source.handle, &mut copy)
            .map_err(|e| unavailable("cannot copy source snapshot", e))?;
        copy.flush()
            .map_err(|e| unavailable("cannot flush copied snapshot", e))?;
        copy.sync_data()
            .map_err(|e| unavailable("cannot sync copied snapshot", e))?;
        drop(copy);
        let mut copied = File::open(checked.options.working_copy.join(&source.relative))
            .map_err(|e| unavailable("cannot verify working copy", e))?;
        if digest(&mut copied)? != source.digest {
            return Err(invalid("working copy digest differs from source"));
        }
    }
    checked.verify_source()?;
    Ok(checked)
}

impl CheckedCopy {
    pub fn verify_source(&self) -> Result<(), AppError> {
        if inventory(&self.options.source)? != self.paths {
            return Err(invalid(
                "source snapshot directory changed; no consistent snapshot was published",
            ));
        }
        for source in &self.files {
            let metadata = source
                .handle
                .metadata()
                .map_err(|e| unavailable("cannot inspect source lease", e))?;
            if metadata.len() != source.length || metadata.modified().ok() != Some(source.modified)
            {
                return Err(invalid(
                    "source snapshot changed; no consistent snapshot was published",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("closed-snapshot-files-{}", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            assert_eq!(self.0.parent(), Some(std::env::temp_dir().as_path()));
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn source_leases_reject_existing_writers_and_hardlinks() {
        let fixture = Fixture::new();
        let path = fixture.0.join("chroma.sqlite3");
        fs::write(&path, b"synthetic closed snapshot").unwrap();
        let writer = OpenOptions::new().write(true).open(&path).unwrap();
        assert!(open_locked(&path).is_err());
        drop(writer);
        let lease = open_locked(&path).unwrap();
        assert!(OpenOptions::new().write(true).open(&path).is_err());
        assert!(fs::remove_file(&path).is_err());
        drop(lease);
        fs::hard_link(&path, fixture.0.join("alias.sqlite3")).unwrap();
        assert!(open_locked(&path).is_err());
    }

    #[test]
    fn inventory_change_refuses_a_consistent_snapshot_and_destinations_never_overwrite() {
        let fixture = Fixture::new();
        let path = fixture.0.join("chroma.sqlite3");
        fs::write(&path, b"synthetic closed snapshot").unwrap();
        assert!(new_destination(&path).is_err());
        let mut file = open_locked(&path).unwrap();
        let metadata = file.metadata().unwrap();
        let options = ChromaCopyExportOptions {
            source: fixture.0.clone(),
            working_copy: fixture.0.join("new-copy"),
            native_server: path.clone(),
            server_sha256: String::new(),
            output: fixture.0.join("new-snapshot.json"),
            collection: "main".into(),
            tenant: "tenant".into(),
            database: "database".into(),
            entity_collection: None,
        };
        let checked = CheckedCopy {
            options,
            staging: fixture.0.join("staging.json"),
            paths: inventory(&fixture.0).unwrap(),
            files: vec![LockedFile {
                relative: "chroma.sqlite3".into(),
                length: metadata.len(),
                modified: metadata.modified().unwrap(),
                digest: digest(&mut file).unwrap(),
                handle: file,
            }],
            _server: File::open(&path).unwrap(),
        };
        checked.verify_source().unwrap();
        fs::write(fixture.0.join("new-file"), b"changed directory").unwrap();
        assert!(checked.verify_source().is_err());
    }
}
