/// Speech preparation options matching the old cleaner's user-facing switches.
/// Inline code is omitted only when the fragment has paired backticks.
/// An odd number of backticks is discarded without treating text as code.
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
        let paired_backticks = self.remove_markdown
            && input
                .bytes()
                .filter(|byte| *byte == b'`')
                .count()
                .is_multiple_of(2);
        for character in input.chars() {
            if self.remove_markdown && character == '`' {
                if paired_backticks {
                    inside_code = !inside_code;
                }
                continue;
            }
            if inside_code {
                continue;
            }
            if self.remove_emoji && is_legacy_emoji(character) {
                continue;
            }
            if !character.is_control() || character.is_whitespace() {
                filtered.push(character);
            }
        }
        let filtered = if self.remove_markdown {
            strip_markdown(&filtered)
        } else {
            filtered
        };
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

/// Remove paired inline formatting and line prefixes without guessing at symbols
/// embedded in identifiers or expressions. Unmatched marks remain audible text.
/// This is a limited heuristic; ambiguous mixed formatting can be interpreted
/// as markup and is not a complete Markdown parser.
fn strip_markdown(input: &str) -> String {
    let characters: Vec<_> = input.chars().collect();
    let mut remove = vec![false; characters.len()];
    let mut stack: Vec<(char, usize, usize)> = Vec::new();
    let mut index = 0;
    let mut line_prefix = true;
    while index < characters.len() {
        let current = characters[index];
        if current == '\n' || current == '\r' {
            line_prefix = true;
            index += 1;
            continue;
        }
        if line_prefix && current.is_whitespace() {
            index += 1;
            continue;
        }
        if line_prefix && current == '>' && quote_prefix(&characters, index) {
            remove[index] = true;
            index += 1;
            continue;
        }
        if line_prefix && current == '#' {
            let start = index;
            while index < characters.len() && characters[index] == '#' {
                index += 1;
            }
            if index - start <= 6
                && characters
                    .get(index)
                    .is_some_and(|next| next.is_whitespace())
            {
                remove[start..index].fill(true);
            }
            line_prefix = false;
            continue;
        }
        line_prefix = false;
        if !matches!(current, '*' | '_' | '~') {
            index += 1;
            continue;
        }
        let start = index;
        while index < characters.len() && characters[index] == current {
            index += 1;
        }
        let width = index - start;
        if (current == '~' && width != 2) || width > 3 {
            continue;
        }
        let escapes = characters[..start]
            .iter()
            .rev()
            .take_while(|character| **character == '\\')
            .count();
        if escapes % 2 != 0 {
            continue;
        }
        let before = start
            .checked_sub(1)
            .and_then(|i| characters.get(i))
            .copied();
        let after = characters.get(index).copied();
        let within_ascii_word = before.is_some_and(|c| c.is_ascii_alphanumeric())
            && after.is_some_and(|c| c.is_ascii_alphanumeric());
        let matching_open = stack
            .last()
            .is_some_and(|(mark, count, _)| *mark == current && *count == width);
        if within_ascii_word && (current == '_' || width == 1 || !matching_open) {
            continue;
        }
        let can_close = before.is_some_and(|c| !c.is_whitespace());
        let can_open = after.is_some_and(|c| !c.is_whitespace());
        if can_close && matching_open {
            let (_, _, opening) = stack.pop().expect("matching mark exists");
            remove[opening..opening + width].fill(true);
            remove[start..index].fill(true);
        } else if can_open {
            stack.push((current, width, start));
        }
    }
    characters
        .into_iter()
        .zip(remove)
        .filter_map(|(c, remove)| (!remove).then_some(c))
        .collect()
}

fn quote_prefix(characters: &[char], index: usize) -> bool {
    let Some(next) = characters.get(index + 1) else {
        return false;
    };
    if !next.is_whitespace() {
        return false;
    }
    let mut content = characters[index + 1..]
        .iter()
        .copied()
        .skip_while(|c| *c == ' ' || *c == '\t');
    let first = content.next();
    let numeric = first.is_some_and(|c| c.is_ascii_digit())
        || first.is_some_and(|c| matches!(c, '+' | '-' | '.'))
            && content.next().is_some_and(|c| c.is_ascii_digit());
    !numeric
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

    #[test]
    fn unpaired_backticks_do_not_silence_following_speech() {
        for (input, expected) in [
            ("这是 `未闭合的说明。", "这是 未闭合的说明。"),
            ("片段` 后面的正文。", "片段 后面的正文。"),
            ("`前面的正文。", "前面的正文。"),
            (
                "请看 `secret` 然后 `继续阅读。",
                "请看 secret 然后 继续阅读。",
            ),
            (
                "bar` 的函数，查看 `code` 后继续。",
                "bar 的函数，查看 code 后继续。",
            ),
            ("`隐藏` 后面的正文。", "后面的正文。"),
        ] {
            assert_eq!(SpeechCleanup::default().clean(input), expected);
        }
    }

    #[test]
    fn sentence_fragments_do_not_lose_speech_after_a_split_inline_code() {
        let mut buffer = crate::SentenceBuffer::default();
        let sentences = buffer.push("这是 `foo.bar` 的函数。");
        assert_eq!(sentences, ["这是 `foo.", "bar` 的函数。"]);
        let speech: Vec<_> = sentences
            .iter()
            .map(|sentence| crate::clean_for_speech(sentence))
            .collect();
        assert_eq!(speech, ["这是 foo.", "bar 的函数。"]);
    }

    #[test]
    fn preserves_identifiers_arithmetic_and_unmatched_symbols() {
        for input in [
            "版本 x_1，2*3=6，5 > 3。",
            "foo_bar_baz、A*B*C、C#、~5",
            "x**2 + y**3，x**y + a**b",
            ">5，>=5，5 > 3",
            ">=5",
            "> 5",
            "> -5",
            "> .5",
            "未闭合 *重点，未闭合 _名称，未闭合 ~~删除。",
            "转义 \\*literal* 保留，标签 #topic",
        ] {
            assert_eq!(SpeechCleanup::default().clean(input), input);
        }
    }

    #[test]
    fn removes_paired_formatting_and_line_prefixes_without_changing_words() {
        for (input, expected) in [
            ("**重点 _内容_**、~~旧词~~和*轻声*", "重点 内容、旧词和轻声"),
            ("我**喜欢**茶，猫_很_可爱", "我喜欢茶，猫很可爱"),
            ("# 标题\n> 引用\n5 > 3\n#topic", "标题 引用 5 > 3 #topic"),
            (
                "**保留 foo_bar**，以及 2**3**4",
                "保留 foo_bar，以及 2**3**4",
            ),
            ("**Hello**world 和 *a*b*c*", "Helloworld 和 a*b*c"),
            ("**v2**next 和 **3**x", "v2next 和 3x"),
        ] {
            assert_eq!(SpeechCleanup::default().clean(input), expected);
        }
    }

    #[test]
    fn split_formatting_never_removes_the_unpaired_words() {
        let mut buffer = crate::SentenceBuffer::default();
        let sentences = buffer.push("**第一句。第二句**以及 x_1。");
        assert_eq!(sentences, ["**第一句。", "第二句**以及 x_1。"]);
        let speech: Vec<_> = sentences
            .iter()
            .map(|sentence| crate::clean_for_speech(sentence))
            .collect();
        assert_eq!(speech, ["**第一句。", "第二句**以及 x_1。"]);
    }
}
