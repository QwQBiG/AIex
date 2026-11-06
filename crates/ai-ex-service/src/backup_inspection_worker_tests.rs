use super::*;
use std::cell::Cell;

#[test]
fn invalid_options_are_rejected_before_source_read() {
    let path = std::env::current_dir()
        .unwrap()
        .join("synthetic-unused.jsonl");
    let path = path.to_string_lossy().into_owned();
    for (profile, comparison_space) in [
        (String::new(), None),
        ("x".repeat(129), None),
        ("valid".into(), Some("".into())),
        ("valid".into(), Some("space\nmarker".into())),
        ("valid".into(), Some("x".repeat(257))),
    ] {
        let called = Cell::new(false);
        let request = Request {
            path: path.clone(),
            profile,
            comparison_space,
            legacy: false,
        };
        assert!(
            inspect_with(request, |_| {
                called.set(true);
                None
            })
            .is_none()
        );
        assert!(!called.get());
    }
}
