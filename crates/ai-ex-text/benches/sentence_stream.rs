use std::hint::black_box;
use std::time::{Duration, Instant};

use ai_ex_text::SentenceBuffer;

// Frozen pre-change Rust algorithm, for an apples-to-apples local comparison.
#[derive(Default)]
struct RescanningBuffer(String);

impl RescanningBuffer {
    fn push(&mut self, chunk: &str) -> Vec<String> {
        self.0.push_str(chunk);
        let mut output = Vec::new();
        let mut start = 0;
        let mut consumed = 0;
        for (index, character) in self.0.char_indices() {
            if matches!(character, '。' | '！' | '？' | '.' | '!' | '?' | '\n') {
                let end = index + character.len_utf8();
                let sentence = self.0[start..end].trim();
                if !sentence.is_empty() {
                    output.push(sentence.to_owned());
                }
                start = end;
                consumed = end;
            }
        }
        self.0.drain(..consumed);
        output
    }
}

fn measure(mut run: impl FnMut()) -> Duration {
    run();
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        run();
        samples.push(start.elapsed());
    }
    samples.sort();
    samples[3]
}

fn main() {
    println!(
        "sentence_stream_benchmark version={} os={} arch={}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("characters,chunk_characters,baseline_median_us,incremental_median_us,speedup");
    for characters in [1_024, 4_096, 16_384] {
        for chunk_characters in [1, 16] {
            let chunk = "汉".repeat(chunk_characters);
            let iterations = characters / chunk_characters;
            let baseline = measure(|| {
                let mut buffer = RescanningBuffer::default();
                for _ in 0..iterations {
                    black_box(buffer.push(black_box(&chunk)));
                }
                let sentences = buffer.push("。");
                assert_eq!(sentences[0].chars().count(), characters + 1);
                black_box(sentences);
            });
            let incremental = measure(|| {
                let mut buffer = SentenceBuffer::default();
                for _ in 0..iterations {
                    black_box(buffer.push(black_box(&chunk)));
                }
                let sentences = buffer.push("。");
                assert_eq!(sentences[0].chars().count(), characters + 1);
                assert!(buffer.finish().is_none());
                black_box(sentences);
            });
            println!(
                "{characters},{chunk_characters},{:.3},{:.3},{:.2}",
                baseline.as_secs_f64() * 1e6,
                incremental.as_secs_f64() * 1e6,
                baseline.as_secs_f64() / incremental.as_secs_f64()
            );
        }
    }
}
