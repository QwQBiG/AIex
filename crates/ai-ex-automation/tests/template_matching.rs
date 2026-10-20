use ai_ex_automation::{
    AutomationAction, DesktopBounds, FrameGeometry, GameProfile, MatchOptions, PixelPoint,
    PixelRect, ScreenFrame, TemplateMatch, TemplateMatcher,
};

const LEGACY_PROFILE: &str = include_str!("../../../assets/games/cookie-clicker/profile.json");

fn solid(width: u32, height: u32, value: u8) -> ScreenFrame {
    let mut rgba = Vec::new();
    for _ in 0..width * height {
        rgba.extend_from_slice(&[value, value, value, 255]);
    }
    ScreenFrame::new(width, height, rgba).unwrap()
}

fn texture(width: u32, height: u32) -> ScreenFrame {
    let mut frame = solid(width, height, 0);
    let mut state = 19_u32;
    for pixel in frame.rgba.chunks_exact_mut(4) {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let value = (state >> 24) as u8;
        pixel[..3].fill(value);
    }
    frame
}

fn paste(frame: &mut ScreenFrame, template: &ScreenFrame, x: u32, y: u32) {
    assert!(x + template.width <= frame.width);
    assert!(y + template.height <= frame.height);
    for row in 0..template.height {
        let from = (row * template.width * 4) as usize;
        let to = (((row + y) * frame.width + x) * 4) as usize;
        let bytes = (template.width * 4) as usize;
        frame.rgba[to..to + bytes].copy_from_slice(&template.rgba[from..from + bytes]);
    }
}

fn matcher(template: &ScreenFrame) -> TemplateMatcher {
    TemplateMatcher::new(template, MatchOptions::default()).unwrap()
}

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "actual={actual}, expected={expected}"
    );
}

#[test]
fn loads_the_actual_legacy_profile_without_requiring_a_new_schema() {
    let profile = GameProfile::parse(LEGACY_PROFILE).unwrap();
    assert_eq!(profile.display_name, "Cookie Clicker");
    assert!(!profile.description.is_empty());
    assert_eq!(profile.vlm_prompts.len(), 3);
    assert_eq!(profile.default_templates.len(), 3);
    assert_eq!(
        profile.template_file("primary_target").unwrap(),
        "big-cookie.png"
    );
    assert_eq!(
        profile.template_file("cursor_upgrade").unwrap(),
        "cursor-upgrade.png"
    );
    assert_eq!(
        profile.template_file("grandma_upgrade").unwrap(),
        "grandma-upgrade.png"
    );
    assert_eq!(profile.action_cooldowns["buy_upgrade"], 2.0);
    assert!(profile.template_file("big-cookie").is_err());
    assert!(profile.template_file("").is_err());
}

#[test]
fn profile_accepts_missing_optional_cooldowns_and_rejects_missing_required_fields() {
    let mut value: serde_json::Value = serde_json::from_str(LEGACY_PROFILE).unwrap();
    value.as_object_mut().unwrap().remove("action_cooldowns");
    let profile = GameProfile::parse(&value.to_string()).unwrap();
    assert!(profile.action_cooldowns.is_empty());
    value.as_object_mut().unwrap().remove("default_templates");
    assert!(GameProfile::parse(&value.to_string()).is_err());
    assert!(GameProfile::parse("{").is_err());
}

#[test]
fn profile_rejects_template_path_escapes_on_either_separator_convention() {
    for path in [
        "../cookie.png",
        "..\\cookie.png",
        "nested/cookie.png",
        "nested\\cookie.png",
        "/cookie.png",
        "\\\\server\\cookie.png",
        "C:\\cookie.png",
        "C:cookie.png",
        "cookie.png:stream",
        "cookie.png ",
        "cookie.png.",
        "cookie\n.png",
        "cookie.gif",
        ".png",
        "NUL.png",
        "COM1.png",
    ] {
        let mut profile = GameProfile::parse(LEGACY_PROFILE).unwrap();
        profile
            .default_templates
            .insert("primary_target".to_owned(), path.to_owned());
        assert!(profile.validate().is_err(), "path={path:?}");
        assert!(
            profile.template_file("primary_target").is_err(),
            "path={path:?}"
        );
        assert!(
            GameProfile::parse(&serde_json::to_string(&profile).unwrap()).is_err(),
            "path={path:?}"
        );
    }
}

#[test]
fn profile_revalidates_public_values_and_cooldowns() {
    let mut profile = GameProfile::parse(LEGACY_PROFILE).unwrap();
    profile
        .action_cooldowns
        .insert("buy_upgrade".to_owned(), -1.0);
    assert!(profile.validate().is_err());
    profile
        .action_cooldowns
        .insert("buy_upgrade".to_owned(), f64::NAN);
    assert!(profile.validate().is_err());
    profile.action_cooldowns.clear();
    profile.display_name = " ".to_owned();
    assert!(profile.validate().is_err());
}

#[test]
fn finds_a_known_rgba_template_and_integer_center() {
    let template = texture(20, 20);
    let mut frame = solid(41, 39, 7);
    paste(&mut frame, &template, 13, 11);
    let found = matcher(&template).find_match(&frame, None).unwrap();
    assert_eq!(found.score, 1.0);
    let bounds = found.bounds.unwrap();
    assert_eq!(
        bounds,
        PixelRect {
            x: 13,
            y: 11,
            width: 20,
            height: 20
        }
    );
    assert_eq!(bounds.center(), PixelPoint { x: 23, y: 21 });
}

#[test]
fn roi_offsets_are_included_once_and_oversized_edges_are_clipped() {
    let template = texture(20, 20);
    let mut frame = solid(31, 29, 7);
    paste(&mut frame, &template, 11, 9);
    let found = matcher(&template)
        .find_match(
            &frame,
            Some(PixelRect {
                x: 11,
                y: 9,
                width: u32::MAX,
                height: u32::MAX,
            }),
        )
        .unwrap();
    assert_eq!(
        found.bounds,
        Some(PixelRect {
            x: 11,
            y: 9,
            width: 20,
            height: 20
        })
    );
    assert_eq!(found.bounds.unwrap().center(), PixelPoint { x: 21, y: 19 });
}

#[test]
fn selects_the_best_nonperfect_candidate() {
    let template = texture(20, 20);
    let mut weak = template.clone();
    for pixel in weak.rgba.chunks_exact_mut(4).step_by(3) {
        pixel[..3].fill(127);
    }
    let mut strong = template.clone();
    let changed = strong.rgba[0] ^ 255;
    strong.rgba[..3].fill(changed);
    let mut frame = solid(43, 20, 7);
    paste(&mut frame, &weak, 0, 0);
    paste(&mut frame, &strong, 23, 0);
    let found = matcher(&template).find_match(&frame, None).unwrap();
    assert_eq!(found.bounds.unwrap().x, 23);
    assert!(found.score > 0.98 && found.score < 1.0);
}

#[test]
fn preserves_a_negative_best_score_without_fabricating_a_hit() {
    let template = texture(20, 20);
    let mut inverse = template.clone();
    for pixel in inverse.rgba.chunks_exact_mut(4) {
        let value = 255 - pixel[0];
        pixel[..3].fill(value);
    }
    let found = matcher(&template).find_match(&inverse, None).unwrap();
    near(found.score, -1.0);
    assert_eq!(found.bounds, None);
}

#[test]
fn normalized_correlation_recognizes_a_positive_affine_change() {
    let mut template = texture(20, 20);
    for pixel in template.rgba.chunks_exact_mut(4) {
        let value = pixel[0] % 100;
        pixel[..3].fill(value);
    }
    let mut changed = template.clone();
    for pixel in changed.rgba.chunks_exact_mut(4) {
        let value = pixel[0] * 2 + 10;
        pixel[..3].fill(value);
    }
    let found = matcher(&template).find_match(&changed, None).unwrap();
    assert_eq!(found.score, 1.0);
    assert!(found.bounds.is_some());
}

#[test]
fn rejects_constant_templates_and_skips_constant_windows_without_kernel_work() {
    assert!(TemplateMatcher::new(&solid(20, 20, 8), MatchOptions::default()).is_err());
    let template = texture(20, 20);
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.0,
            max_work: 1,
        },
    )
    .unwrap();
    let found = matcher.find_match(&solid(60, 60, 8), None).unwrap();
    assert_eq!(
        found,
        TemplateMatch {
            score: 0.0,
            bounds: None
        }
    );
}

#[test]
fn first_mathematical_perfect_match_ends_search_with_a_single_kernel_budget() {
    let template = texture(20, 20);
    let mut frame = solid(42, 20, 7);
    paste(&mut frame, &template, 0, 0);
    paste(&mut frame, &template, 22, 0);
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 1.0,
            max_work: 400,
        },
    )
    .unwrap();
    let found = matcher.find_match(&frame, None).unwrap();
    assert_eq!(found.bounds.unwrap().x, 0);
    assert_eq!(found.score, 1.0);
}

#[test]
fn exhausted_work_budget_returns_an_error_instead_of_a_partial_result() {
    let template = texture(20, 20);
    let matcher = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.7,
            max_work: 400,
        },
    )
    .unwrap();
    let frame = texture(21, 20);
    let error = matcher.find_match(&frame, None).unwrap_err();
    assert!(error.message.contains("work limit"));
    let too_small = TemplateMatcher::new(
        &template,
        MatchOptions {
            threshold: 0.7,
            max_work: 399,
        },
    )
    .unwrap();
    assert!(too_small.find_match(&template, None).is_err());
}

#[test]
fn revalidates_malformed_and_oversized_frame_values() {
    let template = texture(20, 20);
    let matcher = matcher(&template);
    for frame in [
        ScreenFrame {
            width: 20,
            height: 20,
            rgba: vec![0; 1_599],
        },
        ScreenFrame {
            width: 0,
            height: 20,
            rgba: vec![],
        },
        ScreenFrame {
            width: u32::MAX,
            height: u32::MAX,
            rgba: vec![],
        },
        ScreenFrame {
            width: 16_777_217,
            height: 1,
            rgba: vec![],
        },
    ] {
        assert!(matcher.find_match(&frame, None).is_err());
        assert!(TemplateMatcher::new(&frame, MatchOptions::default()).is_err());
    }
}

#[test]
fn validates_template_dimensions_and_match_options() {
    for (width, height) in [(19, 20), (20, 19), (201, 20), (20, 201)] {
        assert!(TemplateMatcher::new(&texture(width, height), MatchOptions::default()).is_err());
    }
    let template = texture(20, 20);
    for threshold in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        assert!(
            TemplateMatcher::new(
                &template,
                MatchOptions {
                    threshold,
                    max_work: 400
                },
            )
            .is_err()
        );
    }
    assert!(
        TemplateMatcher::new(
            &template,
            MatchOptions {
                threshold: 0.7,
                max_work: 0
            },
        )
        .is_err()
    );
}

#[test]
fn invalid_roi_is_an_error_and_an_roi_smaller_than_the_template_is_a_miss() {
    let template = texture(20, 20);
    let matcher = matcher(&template);
    for region in [
        PixelRect {
            x: 0,
            y: 0,
            width: 0,
            height: 20,
        },
        PixelRect {
            x: 20,
            y: 0,
            width: 20,
            height: 20,
        },
        PixelRect {
            x: 0,
            y: 20,
            width: 20,
            height: 20,
        },
    ] {
        assert!(matcher.find_match(&template, Some(region)).is_err());
    }
    let found = matcher
        .find_match(
            &template,
            Some(PixelRect {
                x: 0,
                y: 0,
                width: 19,
                height: 20,
            }),
        )
        .unwrap();
    assert_eq!(found.bounds, None);
    assert_eq!(found.score, 0.0);
    assert!(
        matcher
            .find_match(&solid(19, 20, 8), None)
            .unwrap()
            .bounds
            .is_none()
    );
}

#[test]
fn maps_capture_origin_and_scale_once_into_the_existing_pointer_domain() {
    let geometry = FrameGeometry::new(
        100,
        50,
        2.0,
        2.0,
        DesktopBounds::new(0, 0, 1_920, 1_080).unwrap(),
    )
    .unwrap();
    let point = PixelPoint { x: 70, y: 80 };
    assert_eq!(geometry.absolute(point).unwrap(), (240.0, 210.0));
    let (x, y) = geometry.normalize(point).unwrap();
    near(x, 240.0 / 1_919.0);
    near(y, 210.0 / 1_079.0);
    AutomationAction::MovePointer {
        x: x as f32,
        y: y as f32,
    }
    .validate()
    .unwrap();
}

#[test]
fn supports_negative_desktop_origins_and_fractional_capture_scale() {
    let geometry = FrameGeometry::new(
        -1_800,
        -900,
        1.5,
        2.0,
        DesktopBounds::new(-1_920, -1_080, 3_840, 2_160).unwrap(),
    )
    .unwrap();
    let point = PixelPoint { x: 80, y: 40 };
    assert_eq!(geometry.absolute(point).unwrap(), (-1_680.0, -820.0));
    let (x, y) = geometry.normalize(point).unwrap();
    near(x, 240.0 / 3_839.0);
    near(y, 260.0 / 2_159.0);
}

#[test]
fn geometry_accepts_last_pixels_and_rejects_outside_points() {
    let geometry = FrameGeometry::new(
        -100,
        -50,
        1.0,
        1.0,
        DesktopBounds::new(-100, -50, 101, 51).unwrap(),
    )
    .unwrap();
    assert_eq!(
        geometry.normalize(PixelPoint { x: 0, y: 0 }).unwrap(),
        (0.0, 0.0)
    );
    assert_eq!(
        geometry.normalize(PixelPoint { x: 100, y: 50 }).unwrap(),
        (1.0, 1.0)
    );
    assert!(geometry.absolute(PixelPoint { x: 101, y: 50 }).is_err());
    assert!(geometry.normalize(PixelPoint { x: 100, y: 51 }).is_err());
    let single =
        FrameGeometry::new(-1, -2, 1.0, 1.0, DesktopBounds::new(-1, -2, 1, 1).unwrap()).unwrap();
    assert_eq!(
        single.normalize(PixelPoint { x: 0, y: 0 }).unwrap(),
        (0.0, 0.0)
    );
    assert!(single.normalize(PixelPoint { x: 1, y: 0 }).is_err());
}

#[test]
fn geometry_revalidates_public_and_deserialized_values() {
    let desktop = DesktopBounds::new(0, 0, 1_920, 1_080).unwrap();
    assert!(DesktopBounds::new(0, 0, 0, 10).is_err());
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(FrameGeometry::new(0, 0, scale, 1.0, desktop).is_err());
        let geometry = FrameGeometry {
            origin_x: 0,
            origin_y: 0,
            scale_x: 1.0,
            scale_y: scale,
            desktop,
        };
        assert!(geometry.absolute(PixelPoint { x: 0, y: 0 }).is_err());
        assert!(geometry.normalize(PixelPoint { x: 0, y: 0 }).is_err());
    }
    let geometry: FrameGeometry = serde_json::from_str(
        r#"{"origin_x":0,"origin_y":0,"scale_x":1,"scale_y":1,"desktop":{"left":0,"top":0,"width":0,"height":1}}"#,
    )
    .unwrap();
    assert!(geometry.normalize(PixelPoint { x: 0, y: 0 }).is_err());
    let overflow = FrameGeometry::new(0, 0, f64::MAX, 1.0, desktop).unwrap();
    assert!(overflow.absolute(PixelPoint { x: 2, y: 0 }).is_err());
}

#[test]
fn report_and_manifest_value_types_round_trip_through_serde() {
    let geometry = FrameGeometry::new(
        10,
        20,
        1.5,
        2.0,
        DesktopBounds::new(-100, -100, 400, 400).unwrap(),
    )
    .unwrap();
    let encoded = serde_json::to_string(&geometry).unwrap();
    assert_eq!(
        serde_json::from_str::<FrameGeometry>(&encoded).unwrap(),
        geometry
    );
    let found = TemplateMatch {
        score: 0.9,
        bounds: Some(PixelRect {
            x: 1,
            y: 2,
            width: 20,
            height: 20,
        }),
    };
    let encoded = serde_json::to_string(&found).unwrap();
    assert_eq!(
        serde_json::from_str::<TemplateMatch>(&encoded).unwrap(),
        found
    );
    let point = PixelPoint { x: 2, y: 3 };
    assert_eq!(
        serde_json::from_str::<PixelPoint>(&serde_json::to_string(&point).unwrap()).unwrap(),
        point
    );
    assert_eq!(
        serde_json::from_str::<MatchOptions>("{}").unwrap(),
        MatchOptions::default()
    );
}
