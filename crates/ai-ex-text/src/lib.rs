#![forbid(unsafe_code)]

mod preamble;
#[cfg(test)]
mod soft_split_tests;
mod speech_cleanup;

pub use preamble::{PreambleOutput, ResponsePreamble};
pub use speech_cleanup::SpeechCleanup;

#[derive(Debug, Default)]
pub struct SentenceBuffer {
    buffer: String,
    scanned_bytes: usize,
    soft_split_min_chars: Option<usize>,
    segment_chars: usize,
    segment_started: bool,
    inside_backticks: bool,
    parentheses: Vec<char>,
}

impl SentenceBuffer {
    /// Opt in to splitting long segments at Chinese commas and enumeration marks.
    pub fn with_soft_split(min_chars: usize) -> Result<Self, ai_ex_domain::AppError> {
        if !(ai_ex_domain::SOFT_SPLIT_MIN_CHARS..=ai_ex_domain::SOFT_SPLIT_MAX_CHARS)
            .contains(&min_chars)
        {
            return Err(ai_ex_domain::AppError::configuration(
                "soft split minimum is outside supported bounds",
            ));
        }
        Ok(Self {
            soft_split_min_chars: Some(min_chars),
            ..Self::default()
        })
    }

    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut result = Vec::new();
        let mut start = 0;
        let mut consumed = 0;
        // Only the appended suffix can contain a new boundary. Offsets are UTF-8
        // byte boundaries, and a retained suffix has already been scanned.
        for (offset, character) in self.buffer[self.scanned_bytes..].char_indices() {
            let index = self.scanned_bytes + offset;
            let soft_boundary = self.soft_split_min_chars.is_some_and(|minimum| {
                self.segment_chars >= minimum
                    && matches!(character, '，' | '、')
                    && !self.inside_backticks
                    && self.parentheses.is_empty()
            });
            if is_sentence_boundary(character) || soft_boundary {
                let end = index + character.len_utf8();
                let sentence = self.buffer[start..end].trim();
                if !sentence.is_empty() {
                    result.push(sentence.to_owned());
                }
                start = end;
                consumed = end;
                self.segment_chars = 0;
                self.segment_started = false;
                self.inside_backticks = false;
                self.parentheses.clear();
            } else if self.soft_split_min_chars.is_some() {
                if !character.is_whitespace() || self.segment_started {
                    self.segment_started = true;
                    self.segment_chars = self.segment_chars.saturating_add(1);
                }
                if character == '`' {
                    self.inside_backticks = !self.inside_backticks;
                } else if !self.inside_backticks {
                    match character {
                        '(' => self.parentheses.push(')'),
                        '（' => self.parentheses.push('）'),
                        ')' | '）' if self.parentheses.last() == Some(&character) => {
                            self.parentheses.pop();
                        }
                        _ => {}
                    }
                }
            }
        }
        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        self.scanned_bytes = self.buffer.len();
        result
    }

    pub fn finish(&mut self) -> Option<String> {
        let trailing = self.buffer.trim().to_owned();
        self.clear();
        if trailing.is_empty() {
            None
        } else {
            Some(trailing)
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.scanned_bytes = 0;
        self.segment_chars = 0;
        self.segment_started = false;
        self.inside_backticks = false;
        self.parentheses.clear();
    }
}

pub fn clean_for_speech(input: &str) -> String {
    SpeechCleanup::default().clean(input)
}

fn is_sentence_boundary(character: char) -> bool {
    matches!(character, '。' | '！' | '？' | '.' | '!' | '?' | '\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_partial_utf8_and_emits_sentences_once() {
        let mut buffer = SentenceBuffer::default();
        assert!(buffer.push("你好").is_empty());
        assert_eq!(buffer.push("。Next!"), vec!["你好。", "Next!"]);
    }

    #[test]
    fn removes_markdown_and_inline_code() {
        assert_eq!(
            clean_for_speech("**Hello** `ignored()` world"),
            "Hello world"
        );
    }
}

#[cfg(test)]
mod incremental_tests {
    use super::*;

    #[test]
    fn output_is_independent_of_unicode_chunk_boundaries() {
        for input in [
            "你好。Next!尾巴",
            "\n 。！？\n",
            "长句，没有句号🙂尾巴",
            "A。B!C?D\n后面",
        ] {
            let mut whole = SentenceBuffer::default();
            let mut expected = whole.push(input);
            expected.extend(whole.finish());
            let boundaries: Vec<_> = input
                .char_indices()
                .map(|(index, _)| index)
                .chain([input.len()])
                .collect();
            for width in 1..boundaries.len() {
                let mut buffer = SentenceBuffer::default();
                let mut actual = Vec::new();
                let mut start = 0;
                while start + 1 < boundaries.len() {
                    let end = (start + width).min(boundaries.len() - 1);
                    actual.extend(buffer.push(&input[boundaries[start]..boundaries[end]]));
                    start = end;
                }
                actual.extend(buffer.finish());
                assert_eq!(actual, expected, "input={input:?}; chunk width={width}");
            }
        }
    }

    #[test]
    fn finish_and_clear_reset_the_scan_offset() {
        let mut buffer = SentenceBuffer::default();
        assert!(buffer.push("尚未完成").is_empty());
        buffer.clear();
        assert_eq!(buffer.push("新。尾"), ["新。"]);
        assert_eq!(buffer.finish().as_deref(), Some("尾"));
        assert!(buffer.push("").is_empty());
        assert_eq!(buffer.push("再见!"), ["再见!"]);
        assert!(buffer.finish().is_none());
    }
}
