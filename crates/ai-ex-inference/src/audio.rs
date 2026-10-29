use ai_ex_domain::AppError;

const HEADER_SIZE: usize = 44;

/// Encode native mono samples as standard little-endian PCM16 RIFF/WAVE.
/// Finite samples outside [-1, 1] saturate; both full-scale endpoints are kept.
pub fn encode_mono_pcm16_wav(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, AppError> {
    if samples.is_empty() || sample_rate == 0 {
        return Err(AppError::protocol(
            "WAV requires samples and a positive sample rate",
        ));
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(AppError::protocol("WAV samples must be finite"));
    }
    let (data_size, riff_size) = encoded_sizes(samples.len())?;
    let byte_rate = sample_rate
        .checked_mul(2)
        .ok_or_else(|| AppError::protocol("WAV byte rate overflow"))?;
    let capacity = HEADER_SIZE
        .checked_add(data_size as usize)
        .ok_or_else(|| AppError::protocol("WAV allocation size overflow"))?;
    let mut output = Vec::with_capacity(capacity);
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&riff_size.to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16_u32.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&sample_rate.to_le_bytes());
    output.extend_from_slice(&byte_rate.to_le_bytes());
    output.extend_from_slice(&2_u16.to_le_bytes());
    output.extend_from_slice(&16_u16.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_size.to_le_bytes());
    for &sample in samples {
        let scale = if sample < 0.0 { 32768.0 } else { 32767.0 };
        let pcm = (sample.clamp(-1.0, 1.0) * scale).round() as i16;
        output.extend_from_slice(&pcm.to_le_bytes());
    }
    Ok(output)
}

fn encoded_sizes(sample_count: usize) -> Result<(u32, u32), AppError> {
    let data_size = sample_count
        .checked_mul(2)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| AppError::protocol("audio is too large for WAV"))?;
    let riff_size = data_size
        .checked_add(36)
        .ok_or_else(|| AppError::protocol("WAV RIFF size overflow"))?;
    Ok((data_size, riff_size))
}

#[cfg(test)]
mod tests {
    use super::encoded_sizes;

    #[test]
    fn rejects_data_and_riff_size_overflow_without_allocating_audio() {
        assert!(encoded_sizes(usize::MAX).is_err());
        assert!(encoded_sizes((u32::MAX / 2) as usize).is_err());
    }
}
