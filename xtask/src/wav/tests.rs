use super::*;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("native wav 中文 {}-{unique}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("output.wav"), bytes).unwrap();
        Self(directory)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(self.0.join("output.wav")).unwrap();
        fs::remove_dir(&self.0).unwrap();
    }
}

#[test]
fn verifies_actual_pcm16_bytes_and_declared_rate() {
    let bytes = ai_ex_inference::encode_mono_pcm16_wav(&[0.0, 0.5, -0.5], 24000).unwrap();
    let fixture = Fixture::new(&bytes);
    assert_eq!(
        inspect(&fixture.0, "output.wav", 24000).unwrap()["samples"],
        3
    );
    assert!(inspect(&fixture.0, "output.wav", 16000).is_err());
    assert!(inspect(&fixture.0, "../output.wav", 24000).is_err());
}

#[test]
fn rejects_float_truncation_and_misaligned_samples() {
    let bytes = ai_ex_inference::encode_mono_pcm16_wav(&[0.0, 0.5], 24000).unwrap();
    let mut float = bytes.clone();
    float[20] = 3;
    assert!(inspect(&Fixture::new(&float).0, "output.wav", 24000).is_err());
    let mut truncated = bytes.clone();
    truncated.pop();
    assert!(inspect(&Fixture::new(&truncated).0, "output.wav", 24000).is_err());
    let mut odd = bytes;
    odd[40..44].copy_from_slice(&3_u32.to_le_bytes());
    assert!(inspect(&Fixture::new(&odd).0, "output.wav", 24000).is_err());
}
