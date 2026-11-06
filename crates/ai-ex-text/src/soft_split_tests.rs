use super::*;

#[test]
fn soft_split_is_opt_in_and_uses_the_preceding_character_threshold() {
    let input = "一二三，四五六七，后面。";
    let mut original = SentenceBuffer::default();
    assert_eq!(original.push(input), [input]);
    let mut enabled = SentenceBuffer::with_soft_split(4).unwrap();
    assert_eq!(enabled.push(input), ["一二三，四五六七，", "后面。"]);
    assert!(SentenceBuffer::with_soft_split(3).is_err());
    assert!(SentenceBuffer::with_soft_split(513).is_err());
}

#[test]
fn soft_split_is_independent_of_stream_chunks_and_preserves_order() {
    for (input, expected) in [
        (
            "甲乙丙丁，戊己庚辛、末尾。",
            vec!["甲乙丙丁，", "戊己庚辛、", "末尾。"],
        ),
        (
            "甲乙丙丁（里面，说明），结束。",
            vec!["甲乙丙丁（里面，说明），", "结束。"],
        ),
        (
            "甲乙丙丁`代码，内容`，结束。",
            vec!["甲乙丙丁`代码，内容`，", "结束。"],
        ),
        ("1234,5678，尾巴。", vec!["1234,5678，", "尾巴。"]),
    ] {
        let boundaries: Vec<_> = input
            .char_indices()
            .map(|(index, _)| index)
            .chain([input.len()])
            .collect();
        for width in 1..boundaries.len() {
            let mut buffer = SentenceBuffer::with_soft_split(4).unwrap();
            let mut output = Vec::new();
            let mut start = 0;
            while start + 1 < boundaries.len() {
                let end = (start + width).min(boundaries.len() - 1);
                output.extend(buffer.push(&input[boundaries[start]..boundaries[end]]));
                start = end;
            }
            output.extend(buffer.finish());
            assert_eq!(output, expected, "input={input:?}, width={width}");
        }
    }
}

#[test]
fn soft_split_context_resets_on_hard_boundaries_finish_and_clear() {
    let mut buffer = SentenceBuffer::with_soft_split(4).unwrap();
    assert_eq!(buffer.push("abcd（还没结束。"), ["abcd（还没结束。"]);
    assert_eq!(buffer.push("甲乙丙丁，"), ["甲乙丙丁，"]);
    assert!(buffer.push("abcdefgh").is_empty());
    buffer.clear();
    assert!(buffer.push("一，").is_empty());
    assert_eq!(buffer.finish().as_deref(), Some("一，"));
    assert!(buffer.push("二，").is_empty());
    assert_eq!(buffer.finish().as_deref(), Some("二，"));
    assert_eq!(buffer.push("    ，"), Vec::<String>::new());
    assert_eq!(buffer.finish().as_deref(), Some("，"));
}
