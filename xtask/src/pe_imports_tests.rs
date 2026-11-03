use super::*;

fn put16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// A compact, non-executable PE32+ fixture with import names in one section.
pub(crate) fn fixture(normal: Option<&str>, delayed: Option<&str>) -> Vec<u8> {
    let mut data = vec![0; 0x600];
    data[0..2].copy_from_slice(b"MZ");
    put32(&mut data, 0x3c, 0x80);
    data[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut data, 0x84, 0x8664);
    put16(&mut data, 0x86, 1);
    put16(&mut data, 0x94, 0xf0);
    put16(&mut data, 0x98, 0x20b);
    put32(&mut data, 0x98 + 108, 16);
    let section = 0x188;
    data[section..section + 8].copy_from_slice(b".rdata\0\0");
    put32(&mut data, section + 8, 0x400);
    put32(&mut data, section + 12, 0x1000);
    put32(&mut data, section + 16, 0x400);
    put32(&mut data, section + 20, 0x200);
    if let Some(name) = normal {
        put32(&mut data, 0x98 + 112 + 8, 0x1000);
        put32(&mut data, 0x98 + 112 + 12, 40);
        put32(&mut data, 0x200 + 12, 0x1100);
        data[0x300..0x300 + name.len()].copy_from_slice(name.as_bytes());
    }
    if let Some(name) = delayed {
        put32(&mut data, 0x98 + 112 + 13 * 8, 0x1040);
        put32(&mut data, 0x98 + 112 + 13 * 8 + 4, 64);
        put32(&mut data, 0x240, 1);
        put32(&mut data, 0x244, 0x1140);
        data[0x340..0x340 + name.len()].copy_from_slice(name.as_bytes());
    }
    data
}

#[test]
fn detects_regular_and_delayed_runtime_imports() {
    let imports = inspect(&fixture(Some("PYTHON311.DLL"), Some("libpython3.11.dll"))).unwrap();
    assert_eq!(imports.len(), 2);
    assert!(imports[0].python_runtime);
    assert!(!imports[0].delayed);
    assert!(imports[1].python_runtime);
    assert!(imports[1].delayed);
}

#[test]
fn accepts_other_libraries_and_recognizes_debug_runtime_names() {
    let imports = inspect(&fixture(Some("KERNEL32.dll"), Some("python_adapter.dll"))).unwrap();
    assert!(imports.iter().all(|entry| !entry.python_runtime));
    for name in [
        "python3.dll",
        "python311_d.dll",
        "c:\\runtime\\Python312.dll",
    ] {
        assert!(python_dll(name), "{name}");
    }
    assert!(!python_dll("notpython311.dll"));
}

#[test]
fn rejects_invalid_and_unsupported_data() {
    assert!(inspect(b"not a PE").is_err());
    let mut truncated = fixture(Some("KERNEL32.dll"), None);
    truncated.truncate(0x190);
    assert!(inspect(&truncated).is_err());
    let mut bad_name = fixture(Some("KERNEL32.dll"), None);
    put32(&mut bad_name, 0x20c, u32::MAX);
    assert!(inspect(&bad_name).is_err());
    let mut old_delay = fixture(None, Some("python311.dll"));
    put32(&mut old_delay, 0x240, 0);
    assert!(inspect(&old_delay).is_err());
}
