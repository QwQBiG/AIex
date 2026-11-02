<p align="center">
  <a href="https://github.com/wavekat/wavekat-core">
    <img src="https://github.com/wavekat/wavekat-brand/raw/main/assets/banners/wavekat-core-narrow.svg" alt="WaveKat Core">
  </a>
</p>

[![Crates.io](https://img.shields.io/crates/v/wavekat-core.svg)](https://crates.io/crates/wavekat-core)
[![docs.rs](https://docs.rs/wavekat-core/badge.svg)](https://docs.rs/wavekat-core)
[![codecov](https://codecov.io/gh/wavekat/wavekat-core/branch/main/graph/badge.svg)](https://codecov.io/gh/wavekat/wavekat-core)

Shared types for the WaveKat audio processing ecosystem.

> [!WARNING]
> Early development. API may change.

## What's Inside

| Type | Description |
|------|-------------|
| `AudioFrame` | Audio samples with sample rate, accepts `i16` and `f32` in slice, Vec, or array form |
| `IntoSamples` | Trait for transparent sample format conversion |

## Quick Start

```sh
cargo add wavekat-core
```

```rust
use wavekat_core::AudioFrame;

// From f32 — zero-copy (slice, &Vec<f32>, or array)
let frame = AudioFrame::new(&f32_samples, 16000);

// From i16 — normalizes to f32 [-1.0, 1.0] automatically
let frame = AudioFrame::new(&i16_samples, 16000);

// From an owned Vec — zero-copy, produces AudioFrame<'static>
let frame = AudioFrame::from_vec(vec![0.0f32; 160], 16000);

// Inspect the frame
let samples: &[f32] = frame.samples();
let rate: u32 = frame.sample_rate();
let n: usize = frame.len();
let empty: bool = frame.is_empty();
let secs: f64 = frame.duration_secs();

// Convert a borrowed frame to owned
let owned: AudioFrame<'static> = frame.into_owned();
```

## Audio Format Standard

The WaveKat ecosystem standardizes on **16 kHz, mono, f32 `[-1.0, 1.0]`**.
`AudioFrame` handles the conversion so downstream crates don't have to.

```
Your audio (any format)
        |
        v
   AudioFrame::new(samples, sample_rate)
        |
        +---> wavekat-vad
        +---> wavekat-turn
        +---> wavekat-asr (future)
```

## Optional Features

### `wav`

Adds WAV file I/O via [`hound`](https://crates.io/crates/hound).

```sh
cargo add wavekat-core --features wav
```

```rust
use wavekat_core::AudioFrame;

// Read a WAV file (f32 or i16, normalized automatically)
let frame = AudioFrame::from_wav("input.wav")?;
println!("{} Hz, {} samples", frame.sample_rate(), frame.len());

// Write a frame to a WAV file (mono f32 PCM)
frame.write_wav("output.wav")?;
```

### `resample`

Adds sample-rate conversion via [`rubato`](https://crates.io/crates/rubato) (high-quality sinc interpolation).

```sh
cargo add wavekat-core --features resample
```

```rust
use wavekat_core::AudioFrame;

// Resample a 44.1 kHz frame to 24 kHz for TTS
let frame = AudioFrame::from_vec(samples, 44100);
let frame = frame.resample(24000)?;
assert_eq!(frame.sample_rate(), 24000);

// No-op if already at the target rate
let same = frame.resample(24000)?;
```

## License

Licensed under [Apache 2.0](LICENSE).

Copyright 2026 WaveKat.
