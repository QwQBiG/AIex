#![forbid(unsafe_code)]

use std::io::Cursor;

use ai_ex_domain::ErrorKind;
use ai_ex_inference::encode_mono_pcm16_wav;

#[test]
fn hound_reads_standard_mono_pcm16_instead_of_extensible_native_output() {
    let samples = [-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0];
    let wav = encode_mono_pcm16_wav(&samples, 24000).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u32::from_le_bytes(wav[16..20].try_into().unwrap()), 16);
    assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
    assert_eq!(
        u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize,
        wav.len() - 8
    );
    let mut reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
    let spec = reader.spec();
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.sample_rate, 24000);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    let decoded: Vec<_> = reader.samples::<i16>().collect::<Result<_, _>>().unwrap();
    assert_eq!(
        decoded,
        [i16::MIN, i16::MIN, -16384, 0, 16384, i16::MAX, i16::MAX]
    );
}

#[test]
fn preserves_small_signed_samples_in_little_endian_pcm() {
    let wav = encode_mono_pcm16_wav(&[-1.0 / 32768.0, 1.0 / 32767.0], 16000).unwrap();
    assert_eq!(&wav[44..], &[0xff, 0xff, 0x01, 0x00]);
    let mut reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
    assert_eq!(reader.spec().sample_rate, 16000);
    assert_eq!(
        reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        [-1, 1]
    );
}

#[test]
fn canonicalizes_samples_from_an_extensible_float_wav() {
    let mut native = Cursor::new(Vec::new());
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut writer = hound::WavWriter::new(&mut native, spec).unwrap();
        for sample in [-1.0_f32, 0.0, 1.0] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
    }
    assert_eq!(
        u16::from_le_bytes(native.get_ref()[20..22].try_into().unwrap()),
        0xfffe
    );
    native.set_position(0);
    let mut reader = hound::WavReader::new(native).unwrap();
    let floats = reader
        .samples::<f32>()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let canonical = encode_mono_pcm16_wav(&floats, reader.spec().sample_rate).unwrap();
    assert_eq!(u16::from_le_bytes(canonical[20..22].try_into().unwrap()), 1);
    let mut reader = hound::WavReader::new(Cursor::new(canonical)).unwrap();
    assert_eq!(
        reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        [i16::MIN, 0, i16::MAX]
    );
}

#[test]
fn rejects_empty_invalid_rate_and_nonfinite_samples() {
    for (samples, rate) in [
        (vec![], 24000),
        (vec![0.0], 0),
        (vec![0.0], u32::MAX),
        (vec![f32::NAN], 24000),
        (vec![f32::INFINITY], 24000),
        (vec![f32::NEG_INFINITY], 24000),
    ] {
        assert_eq!(
            encode_mono_pcm16_wav(&samples, rate).unwrap_err().kind,
            ErrorKind::Protocol
        );
    }
}
