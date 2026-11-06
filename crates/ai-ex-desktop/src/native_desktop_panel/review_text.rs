//! Render model-proposed text for review without changing the submitted action.

use std::fmt::Write;
use unicode_general_category::{GeneralCategory, get_general_category};

pub(super) fn escaped(text: &str) -> String {
    let mut output = String::new();
    for character in text.chars() {
        let category = get_general_category(character);
        let visible_marker = matches!(
            category,
            GeneralCategory::Control
                | GeneralCategory::Format
                | GeneralCategory::SpaceSeparator
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
                | GeneralCategory::NonspacingMark
                | GeneralCategory::SpacingMark
                | GeneralCategory::EnclosingMark
                | GeneralCategory::PrivateUse
                | GeneralCategory::Unassigned
        );
        if visible_marker {
            let description = match character {
                '\n' => "换行",
                '\r' => "回车",
                '\t' => "制表",
                ' ' => "空格",
                _ => "字符",
            };
            let _ = write!(output, "⟦{description} U+{:04X}⟧", u32::from(character));
            if character == '\n' {
                output.push('\n');
            }
        } else {
            output.push(character);
        }
    }
    output
}

pub(super) fn codepoints(text: &str) -> String {
    let mut output = String::new();
    for (index, character) in text.chars().enumerate() {
        if index > 0 {
            output.push(' ');
        }
        let _ = write!(output, "U+{:04X}", u32::from(character));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_review_shows_text_and_marks_invisible_characters() {
        let sample = "中文 A\t\n\u{202e}\u{2066}\u{200b}\u{200d}\u{fe0f}\u{3000}\u{0301}\u{093e}😀";
        let shown = escaped(sample);
        assert!(shown.starts_with("中文⟦空格 U+0020⟧A"));
        for code in [
            "U+0009", "U+000A", "U+202E", "U+2066", "U+200B", "U+200D", "U+FE0F", "U+3000",
            "U+0301", "U+093E",
        ] {
            assert!(shown.contains(code), "missing {code}");
        }
        assert!(shown.ends_with('😀'));
        let points = codepoints(sample);
        let expected: Vec<_> = sample
            .chars()
            .map(|ch| format!("U+{:04X}", u32::from(ch)))
            .collect();
        assert_eq!(points.split(' ').collect::<Vec<_>>(), expected);
    }

    #[test]
    fn review_does_not_truncate_maximum_length() {
        let sample = format!("{}终", "文".repeat(4095));
        assert_eq!(escaped(&sample), sample);
        assert_eq!(codepoints(&sample).split(' ').count(), 4096);
    }
}
