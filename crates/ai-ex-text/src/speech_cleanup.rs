/// Speech preparation options matching the old cleaner's user-facing switches.
/// Inline code follows the existing Rust behavior: it is omitted from speech.
#[derive(Debug, Clone, Copy)]
pub struct SpeechCleanup {
    pub remove_emoji: bool,
    pub remove_markdown: bool,
    pub remove_parenthetical: bool,
}

impl Default for SpeechCleanup {
    fn default() -> Self {
        Self {
            remove_emoji: true,
            remove_markdown: true,
            remove_parenthetical: true,
        }
    }
}

impl SpeechCleanup {
    pub fn clean(self, input: &str) -> String {
        let mut filtered = String::with_capacity(input.len());
        let mut inside_code = false;
        for character in input.chars() {
            if self.remove_markdown && character == '`' {
                inside_code = !inside_code;
                continue;
            }
            if inside_code
                || (self.remove_markdown && matches!(character, '*' | '_' | '#' | '>' | '~'))
            {
                continue;
            }
            if self.remove_emoji && is_legacy_emoji(character) {
                continue;
            }
            if !character.is_control() || character.is_whitespace() {
                filtered.push(character);
            }
        }
        let mut ranges = Vec::new();
        if self.remove_parenthetical {
            let mut stack = Vec::new();
            for (index, character) in filtered.char_indices() {
                match character {
                    '(' | '（' => stack.push((index, if character == '(' { ')' } else { '）' })),
                    ')' | '）'
                        if stack
                            .last()
                            .is_some_and(|(_, closing)| *closing == character) =>
                    {
                        let (start, _) = stack.pop().expect("matching opening exists");
                        if stack.is_empty() {
                            ranges.push(start..index + character.len_utf8());
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut ranges = ranges.into_iter().peekable();
        let mut output = String::with_capacity(filtered.len());
        let mut pending_space = false;
        for (index, character) in filtered.char_indices() {
            while ranges.peek().is_some_and(|range| index >= range.end) {
                ranges.next();
            }
            if ranges.peek().is_some_and(|range| range.contains(&index)) {
                continue;
            }
            if character.is_whitespace() {
                pending_space = !output.is_empty();
            } else {
                if pending_space {
                    output.push(' ');
                }
                output.push(character);
                pending_space = false;
            }
        }
        output
    }
}

fn is_legacy_emoji(character: char) -> bool {
    matches!(character as u32,
        0x1f000..=0x1f02f | 0x1f0a0..=0x1f0ff | 0x1f1e0..=0x1f1ff |
        0x1f200..=0x1f251 | 0x1f300..=0x1f64f | 0x1f680..=0x1faff |
        0x2702..=0x27b0 | 0x2600..=0x26ff | 0xfe00..=0xfe0f
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_the_legacy_default_speech_fixtures() {
        for (input, expected) in [
            ("Hello 😀 World", "Hello World"),
            ("🎉 Party 🎊", "Party"),
            ("😀😁😂", ""),
            ("**bold text**", "bold text"),
            ("_italic text_", "italic text"),
            ("~~strikethrough~~", "strikethrough"),
            ("Hello (world) there", "Hello there"),
            ("你好（笑）世界", "你好世界"),
            ("**Hello** 😀 (aside) 你好！", "Hello 你好！"),
            ("你好，世界！再见。", "你好，世界！再见。"),
        ] {
            assert_eq!(SpeechCleanup::default().clean(input), expected);
        }
    }

    #[test]
    fn optional_flags_preserve_content_and_nested_directions_are_not_spoken() {
        let unchanged = SpeechCleanup {
            remove_emoji: false,
            remove_markdown: false,
            remove_parenthetical: false,
        };
        assert_eq!(
            unchanged.clean(" **Hi** 😀 （笑） `code` "),
            "**Hi** 😀 （笑） `code`"
        );
        assert_eq!(
            SpeechCleanup::default().clean("你好（轻声（微笑））！"),
            "你好！"
        );
        assert_eq!(
            SpeechCleanup::default().clean("未闭合（说明"),
            "未闭合（说明"
        );
        assert_eq!(
            SpeechCleanup::default().clean("**Hello** `ignored()` world"),
            "Hello world"
        );
    }
}
