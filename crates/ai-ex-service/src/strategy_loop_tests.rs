use super::*;
use ai_ex_automation::{DesktopBounds, ReflexControlMode};
use ai_ex_domain::ErrorKind;
use ai_ex_vision::ImageMediaType;
use serde_json::{Value, json};

fn options(value: Value) -> StrategyOptions {
    serde_json::from_value(value).expect("synthetic strategy options deserialize")
}

fn ticket(width: u32, height: u32) -> StrategyTicket {
    StrategyTicket {
        context: StrategyFrameContext {
            instance_id: Uuid::from_u128(1),
            session_id: Uuid::from_u128(2),
            operation_id: Uuid::from_u128(3),
            control: ReflexControlSnapshot {
                session_id: Uuid::from_u128(4),
                generation: 7,
                mode: ReflexControlMode::Running,
            },
            frame_sequence: 8,
            captured_ms: 500,
            geometry: FrameGeometry::new(
                -100,
                80,
                2.0,
                2.0,
                DesktopBounds {
                    left: -200,
                    top: -200,
                    width: 5000,
                    height: 5000,
                },
            )
            .unwrap(),
        },
        request_id: 9,
        max_age_ms: 5000,
        width,
        height,
    }
}

fn parse(text: &str, ticket: &StrategyTicket) -> Result<StrategyDecision, AppError> {
    parse_decision(
        VisionObservation {
            model: "synthetic-observation".into(),
            text: text.into(),
        },
        ticket,
    )
}

fn assert_protocol_rejection(text: &str, ticket: &StrategyTicket) {
    let error = parse(text, ticket).expect_err("untrusted strategy response must be rejected");
    assert_eq!(error.kind, ErrorKind::Protocol, "response: {text}");
}

#[test]
fn options_json_applies_defaults_and_rejects_missing_prompt_or_unknown_fields() {
    let decoded = options(json!({"prompt": "observe the configured target"}));
    decoded.validate().unwrap();
    assert_eq!(decoded.prompt, "observe the configured target");
    assert_eq!(decoded.interval_ms, 1000);
    assert_eq!(decoded.timeout_ms, 5000);
    assert_eq!(decoded.max_age_ms, 5000);
    assert_eq!(decoded.max_requests, 16);
    assert!(serde_json::from_value::<StrategyOptions>(json!({})).is_err());
    for unknown in [
        json!({"prompt": "observe", "action": "click"}),
        json!({"prompt": "observe", "unknown": true}),
        json!({"prompt": "observe", "control": {"resume": true}}),
    ] {
        assert!(serde_json::from_value::<StrategyOptions>(unknown).is_err());
    }
}

#[test]
fn options_validate_exact_duration_request_and_prompt_boundaries() {
    for (field, minimum, maximum) in [
        ("interval_ms", 100u64, 60_000u64),
        ("timeout_ms", 1, 30_000),
        ("max_age_ms", 1, 60_000),
        ("max_requests", 1, 64),
    ] {
        for accepted in [minimum, maximum] {
            let mut value = json!({"prompt": "observe"});
            value[field] = json!(accepted);
            options(value).validate().unwrap();
        }
        for rejected in [minimum - 1, maximum + 1] {
            let mut value = json!({"prompt": "observe"});
            value[field] = json!(rejected);
            assert_eq!(
                options(value).validate().unwrap_err().kind,
                ErrorKind::Configuration,
                "{field}={rejected}"
            );
        }
        for malformed in [json!(-1), json!(1.5), json!("100")] {
            let mut value = json!({"prompt": "observe"});
            value[field] = malformed;
            assert!(
                serde_json::from_value::<StrategyOptions>(value).is_err(),
                "{field} must remain an unsigned integer"
            );
        }
    }
    assert!(
        serde_json::from_value::<StrategyOptions>(
            json!({"prompt":"observe","max_requests":65_536})
        )
        .is_err()
    );
    for prompt in ["".to_owned(), " \t\r\n ".to_owned(), "界".repeat(2049)] {
        assert_eq!(
            options(json!({"prompt": prompt}))
                .validate()
                .unwrap_err()
                .kind,
            ErrorKind::Configuration
        );
    }
    options(json!({"prompt": "界".repeat(2048)}))
        .validate()
        .unwrap();
}

#[test]
fn decisions_accept_only_keep_or_a_complete_original_frame_local_rectangle() {
    let source = ticket(1280, 720);
    assert_eq!(
        parse(r#"{"type":"keep"}"#, &source).unwrap(),
        StrategyDecision::Keep
    );
    assert_eq!(
        parse(" \n { \"type\" : \"keep\" } \t", &source).unwrap(),
        StrategyDecision::Keep
    );
    let expected = PixelRect {
        x: 960,
        y: 400,
        width: 320,
        height: 320,
    };
    assert_eq!(
        parse(
            r#"{"region":{"height":320,"width":320,"y":400,"x":960},"type":"focus_region"}"#,
            &source
        )
        .unwrap(),
        StrategyDecision::FocusRegion(expected)
    );
    assert_eq!(
        parse(
            r#"{"type":"focus_region","region":{"x":1279,"y":719,"width":1,"height":1}}"#,
            &source
        )
        .unwrap(),
        StrategyDecision::FocusRegion(PixelRect {
            x: 1279,
            y: 719,
            width: 1,
            height: 1
        })
    );
    // Coordinates stay image-local even with a negative desktop origin and scale 2.
    assert_eq!(expected.x, 960);
    assert_eq!(expected.y, 400);
}

#[test]
fn decisions_reject_action_authority_and_extra_fields_on_both_variants() {
    let source = ticket(640, 480);
    for text in [
        r#"{"type":"keep","action":"click"}"#,
        r#"{"type":"keep","unknown":true}"#,
        r#"{"type":"keep","region":{"x":0,"y":0,"width":20,"height":20}}"#,
        r#"{"type":"focus_region","region":{"x":0,"y":0,"width":20,"height":20},"action":"click"}"#,
        r#"{"type":"focus_region","region":{"x":0,"y":0,"width":20,"height":20},"unknown":true}"#,
        r#"{"type":"focus_region","region":{"x":0,"y":0,"width":20,"height":20,"action":"click"}}"#,
        r#"{"type":"focus_region","region":{"x":0,"y":0,"width":20,"height":20,"confidence":1}}"#,
        r#"{"type":"click","button":"left"}"#,
        r#"{"type":"resume"}"#,
        r#"{"type":"Keep"}"#,
    ] {
        assert_protocol_rejection(text, &source);
    }
}

#[test]
fn decisions_reject_non_json_wrappers_multiple_values_and_missing_or_duplicate_fields() {
    let source = ticket(640, 480);
    for text in [
        "",
        "keep",
        "null",
        "true",
        r#"[{"type":"keep"}]"#,
        r#""{\"type\":\"keep\"}""#,
        "```json\n{\"type\":\"keep\"}\n```",
        r#"Result: {"type":"keep"}"#,
        r#"{"type":"keep"} extra"#,
        r#"{"type":"keep"}{"type":"keep"}"#,
        r#"{"type":"keep",}"#,
        r#"{"type":"keep","type":"keep"}"#,
        r#"{"type":"focus_region"}"#,
        r#"{"type":"focus_region","region":{"x":0,"y":0,"width":20}}"#,
        r#"{"type":"focus_region","region":{"x":0,"x":1,"y":0,"width":20,"height":20}}"#,
    ] {
        assert_protocol_rejection(text, &source);
    }
}

#[test]
fn focus_requires_unsigned_integer_nonzero_nonoverflowing_bounds_inside_original_frame() {
    let source = ticket(640, 480);
    let valid = json!({"type":"focus_region","region":{"x":10,"y":10,"width":20,"height":20}});
    for field in ["x", "y", "width", "height"] {
        for malformed in [
            json!(-1),
            json!(1.0),
            json!(1.5),
            json!(4_294_967_296u64),
            json!("20"),
        ] {
            let mut response = valid.clone();
            response["region"][field] = malformed;
            assert_protocol_rejection(&response.to_string(), &source);
        }
    }
    for region in [
        json!({"x":0,"y":0,"width":0,"height":20}),
        json!({"x":0,"y":0,"width":20,"height":0}),
        json!({"x":4_294_967_295u64,"y":0,"width":1,"height":20}),
        json!({"x":0,"y":4_294_967_295u64,"width":20,"height":1}),
        json!({"x":640,"y":0,"width":1,"height":20}),
        json!({"x":0,"y":480,"width":20,"height":1}),
        json!({"x":621,"y":0,"width":20,"height":20}),
        json!({"x":0,"y":461,"width":20,"height":20}),
    ] {
        assert_protocol_rejection(
            &json!({"type":"focus_region","region":region}).to_string(),
            &source,
        );
    }
    assert_eq!(
        parse(
            r#"{"type":"focus_region","region":{"x":620,"y":460,"width":20,"height":20}}"#,
            &source
        )
        .unwrap(),
        StrategyDecision::FocusRegion(PixelRect {
            x: 620,
            y: 460,
            width: 20,
            height: 20
        })
    );
}

#[test]
fn response_limit_counts_utf8_bytes_and_accepts_only_one_bounded_json_value() {
    let source = ticket(640, 480);
    let keep = r#"{"type":"keep"}"#;
    let boundary = format!("{keep}{}", " ".repeat(MAX_RESPONSE_BYTES - keep.len()));
    assert_eq!(boundary.len(), 16 * 1024);
    assert_eq!(parse(&boundary, &source).unwrap(), StrategyDecision::Keep);
    let oversized = format!("{boundary} ");
    let error = parse(&oversized, &source).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message.contains("byte limit"));
    let multibyte = "界".repeat(MAX_RESPONSE_BYTES / 3 + 1);
    assert!(multibyte.chars().count() < MAX_RESPONSE_BYTES);
    assert!(
        parse(&multibyte, &source)
            .unwrap_err()
            .message
            .contains("byte limit")
    );
}

#[test]
fn png_sampling_caps_the_long_edge_without_rescaling_decision_coordinates() {
    for (width, height, expected) in [
        (1280, 720, (640, 360)),
        (720, 1280, (360, 640)),
        (64, 32, (64, 32)),
    ] {
        let rgba = vec![127; width as usize * height as usize * 4];
        let encoded = encode_frame(ScreenFrame::new(width, height, rgba).unwrap()).unwrap();
        assert_eq!(encoded.media_type, ImageMediaType::Png);
        assert!(encoded.bytes.len() <= MAX_ENCODED_BYTES);
        let decoded =
            image::load_from_memory_with_format(&encoded.bytes, image::ImageFormat::Png).unwrap();
        assert_eq!((decoded.width(), decoded.height()), expected);
        let source = ticket(width, height);
        let original = PixelRect {
            x: width - 20,
            y: height - 20,
            width: 20,
            height: 20,
        };
        let response = json!({"type":"focus_region","region":{"x":original.x,"y":original.y,"width":20,"height":20}});
        assert_eq!(
            parse(&response.to_string(), &source).unwrap(),
            StrategyDecision::FocusRegion(original)
        );
    }
    let oversized = ScreenFrame {
        width: 2049,
        height: 2048,
        rgba: Vec::new(),
    };
    assert_eq!(
        encode_frame(oversized).unwrap_err().kind,
        ErrorKind::Configuration
    );
    let malformed = ScreenFrame {
        width: 1,
        height: 1,
        rgba: vec![0; 3],
    };
    assert_eq!(
        encode_frame(malformed).unwrap_err().kind,
        ErrorKind::Protocol
    );
}
