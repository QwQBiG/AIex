use super::*;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("portable-test-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).expect("create own test directory");
    path
}

#[test]
fn metadata_requires_a_unique_named_version() {
    let metadata = json!({"packages": [{"name": "service", "version": "1.0.0-beta.1"}]});
    assert_eq!(
        package_version(&metadata, "service").unwrap(),
        "1.0.0-beta.1"
    );
    assert!(package_version(&metadata, "missing").is_err());
    assert!(
        package_version(
            &json!({"packages": [{"name":"x","version":"1"},{"name":"x","version":"2"}]}),
            "x"
        )
        .is_err()
    );
}

#[test]
fn checks_pe_architecture_and_gui_subsystem() {
    let dir = directory();
    let path = dir.join("program.exe");
    let mut bytes = vec![0; 256];
    bytes[0x3c..0x40].copy_from_slice(&64_u32.to_le_bytes());
    bytes[64..70].copy_from_slice(&[b'P', b'E', 0, 0, 0x64, 0x86]);
    bytes[156..158].copy_from_slice(&2_u16.to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    assert!(check_pe(&path, true).is_ok());
    bytes[156] = 3;
    fs::write(&path, &bytes).unwrap();
    assert!(check_pe(&path, true).is_err());
    assert!(check_pe(&path, false).is_ok());
    bytes[68] = 0x4c;
    fs::write(&path, &bytes).unwrap();
    assert!(check_pe(&path, false).is_err());
    fs::write(&path, [1, 2]).unwrap();
    assert!(check_pe(&path, false).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn version_checks_refuse_wrong_or_unready_programs() {
    let root = Path::new("unused");
    let check = |path: &Path, arg: &str| -> Result<Vec<u8>, String> {
        if arg == "--check-install" {
            return Ok(br#"{"ready":true,"version":"1.0.0"}"#.to_vec());
        }
        Ok(format!("{} 1.0.0\n", path.file_stem().unwrap().to_string_lossy()).into_bytes())
    };
    assert!(verify_with(root, "1.0.0", check).is_ok());
    assert!(verify_with(root, "1.0.1", check).is_err());
    assert!(
        verify_with(root, "1.0.0", |_, _| Ok(
            br#"{"ready":false,"version":"1.0.0"}"#.to_vec()
        ))
        .is_err()
    );
    assert!(verify_with(root, "1.0.0", |_, _| Err("program failed".into())).is_err());
    assert!(
        verify_with(root, "1.0.0", |_, arg| {
            if arg == "--check-install" {
                Ok(br#"{"ready":true,"version":"1.0.0"}"#.to_vec())
            } else {
                Ok(b"incorrect-version".to_vec())
            }
        })
        .is_err()
    );
}

#[test]
fn archive_allowlist_hash_and_existing_output_are_verified() {
    let dir = directory();
    let package = dir.join("package");
    fs::create_dir(&package).unwrap();
    fs::create_dir(package.join("data")).unwrap();
    let contents: Vec<_> = required_files().collect();
    assert_eq!(contents.len(), 10);
    for name in &contents {
        fs::write(package.join(name), format!("fixture {name}")).unwrap();
    }
    fs::write(package.join("private.txt"), "must not be included").unwrap();
    let archive = dir.join("out.zip");
    let hash = write_archive(&package, &archive, "bundle").unwrap();
    let original = fs::read(&archive).unwrap();
    let expected: String = ring::digest::digest(&SHA256, &original)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(hash, expected);
    let mut zip = zip::ZipArchive::new(File::open(&archive).unwrap()).unwrap();
    assert_eq!(zip.len(), contents.len());
    for name in contents {
        let mut entry = zip.by_name(&format!("bundle/{name}")).unwrap();
        let mut data = String::new();
        entry.read_to_string(&mut data).unwrap();
        assert_eq!(data, format!("fixture {name}"));
    }
    assert!(zip.by_name("bundle/private.txt").is_err());
    drop(zip);
    assert!(write_archive(&package, &archive, "bundle").is_err());
    assert!(reject_existing(&[&archive]).is_err());
    assert_eq!(fs::read(&archive).unwrap(), original);
    assert!(write_new(&archive, b"replace").is_err());
    fs::remove_dir_all(dir).unwrap();
}
