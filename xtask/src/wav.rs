use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Value, json};

pub fn inspect(job: &Path, relative: &str, sample_rate: u32) -> Result<Value, String> {
    ai_ex_model_catalog::validate_relative_path(relative).map_err(|error| error.to_string())?;
    let mut path = job.to_path_buf();
    check_link(&path)?;
    for component in relative.split('/') {
        path.push(component);
        check_link(&path)?;
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    inspect_file(file, sample_rate)
}

fn check_link(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let mut linked = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        linked |= metadata.file_attributes() & 0x400 != 0;
    }
    if linked {
        Err("audio output cannot use links or reparse points".into())
    } else {
        Ok(())
    }
}

fn inspect_file(mut file: File, sample_rate: u32) -> Result<Value, String> {
    let bytes = file.metadata().map_err(|error| error.to_string())?.len();
    let mut header = [0_u8; 12];
    read_exact(&mut file, &mut header)?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WAVE"
        || u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8 != bytes
    {
        return Err("invalid RIFF/WAVE size or signature".into());
    }
    let mut position = 12_u64;
    let mut format = false;
    let mut data_bytes = None;
    while position < bytes {
        let mut chunk = [0_u8; 8];
        read_exact(&mut file, &mut chunk)?;
        let length = u32::from_le_bytes(chunk[4..].try_into().unwrap());
        let end = position + 8 + u64::from(length) + u64::from(length % 2);
        if end > bytes {
            return Err("truncated WAV chunk".into());
        }
        match &chunk[..4] {
            b"fmt " => {
                if format || length != 16 {
                    return Err("WAV requires one standard PCM fmt chunk".into());
                }
                let mut fmt = [0_u8; 16];
                read_exact(&mut file, &mut fmt)?;
                let expected_rate = sample_rate.checked_mul(2).ok_or("WAV rate overflow")?;
                if fmt[..4] != [1, 0, 1, 0]
                    || u32::from_le_bytes(fmt[4..8].try_into().unwrap()) != sample_rate
                    || u32::from_le_bytes(fmt[8..12].try_into().unwrap()) != expected_rate
                    || fmt[12..] != [2, 0, 16, 0]
                {
                    return Err("WAV is not declared mono PCM16".into());
                }
                format = true;
            }
            b"data" => {
                if data_bytes.is_some() || length == 0 || length % 2 != 0 {
                    return Err("invalid PCM16 sample data".into());
                }
                data_bytes = Some(length);
            }
            _ => {}
        }
        file.seek(SeekFrom::Start(end))
            .map_err(|error| error.to_string())?;
        position = end;
    }
    if !format || sample_rate == 0 {
        return Err("WAV lacks a valid PCM format".into());
    }
    let samples = data_bytes.ok_or("WAV lacks sample data")? / 2;
    Ok(
        json!({"codec": "pcm_s16le", "channels": 1, "sample_rate": sample_rate,
        "samples": samples, "bytes": bytes}),
    )
}

fn read_exact(file: &mut File, bytes: &mut [u8]) -> Result<(), String> {
    file.read_exact(bytes)
        .map_err(|error| format!("cannot read WAV: {error}"))
}

#[cfg(test)]
mod tests;
