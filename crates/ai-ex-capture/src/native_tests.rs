use super::*;

#[test]
fn named_devices_are_listed_once_and_ambiguous_names_are_not_selected() {
    let devices = readable_items([
        (Ok("Microphone A".into()), 1),
        (Ok("microphone a".into()), 2),
        (Ok("合成麦克风".into()), 3),
    ])
    .unwrap();
    let names: Vec<_> = devices.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        catalog(names.iter().map(String::as_str)),
        vec![
            InputDeviceChoice {
                name: "Microphone A".into(),
                ambiguous: true,
            },
            InputDeviceChoice {
                name: "合成麦克风".into(),
                ambiguous: false,
            },
        ]
    );
    assert!(explicit_name_index(&names, "MICROPHONE A").is_err());
    assert_eq!(explicit_name_index(&names, "合成麦克风").unwrap(), 2);
    assert!(explicit_name_index(&names, "missing").is_err());
}

#[test]
fn device_discovery_rejects_unreadable_or_oversized_lists() {
    let empty: Vec<(Result<String, AppError>, ())> = Vec::new();
    assert!(readable_items(empty).unwrap().is_empty());
    assert!(readable_items([(Ok("\n".into()), ())]).is_err());
    assert!(readable_items([(Ok("a".repeat(257)), ())]).is_err());
    assert!(readable_items([(Err(AppError::unavailable("synthetic error")), ())]).is_err());
    assert!(
        readable_items((0..=MAX_INPUT_DEVICES).map(|i| (Ok(format!("Device {i}")), ()))).is_err()
    );
    assert!(readable_items((0..49).map(|_| (Ok("a".repeat(256)), ()))).is_err());
}
