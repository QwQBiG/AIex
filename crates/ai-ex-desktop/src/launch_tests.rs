use super::*;

fn parse(arguments: &[&str]) -> Result<LaunchOptions, AppError> {
    parse_options(arguments.iter().map(|argument| (*argument).to_owned()))
}

#[test]
fn source_config_defaults_and_explicit_paths_remain_distinct() {
    let defaults = parse(&[]).unwrap();
    assert_eq!(
        defaults.config_path,
        PathBuf::from("config/ai-ex.local.toml")
    );
    assert!(!defaults.config_explicit);
    assert!(!defaults.setup);
    assert!(!defaults.developer);
    assert!(!defaults.preview);
    assert!(!defaults.start_service);
    assert!(!defaults.connect_only);
    assert!(defaults.appearance_pack.is_none());

    for path in ["config/ai-ex.local.toml", "自定义目录/connection file.toml"] {
        let explicit = parse(&["--config", path]).unwrap();
        assert_eq!(explicit.config_path, PathBuf::from(path));
        assert!(explicit.config_explicit);
    }
}

#[test]
fn service_overrides_are_accepted_separately_and_rejected_together() {
    let start = parse(&["--start-service"]).unwrap();
    assert!(start.start_service);
    assert!(!start.connect_only);
    let connect = parse(&["--connect-only"]).unwrap();
    assert!(connect.connect_only);
    assert!(!connect.start_service);
    let error = parse(&["--connect-only", "--start-service"]).err().unwrap();
    assert!(
        error
            .to_string()
            .contains("--start-service and --connect-only are mutually exclusive")
    );
}

#[test]
fn offline_preview_accepts_a_pack_and_rejects_connection_modes() {
    let path = "自定义目录/portrait pack/appearance.toml";
    let preview = parse(&["--preview", "--appearance-pack", path]).unwrap();
    assert!(preview.preview);
    assert_eq!(preview.appearance_pack, Some(PathBuf::from(path)));
    assert!(parse(&["--appearance-pack", path]).is_err());
    for argument in ["--setup", "--start-service", "--connect-only"] {
        let error = parse(&["--preview", argument]).err().unwrap();
        assert!(error.to_string().contains(
            "--preview cannot be combined with --setup, --start-service, or --connect-only"
        ));
    }
}

#[test]
fn setup_and_developer_aliases_keep_their_existing_meaning() {
    for argument in ["--developer", "--dev"] {
        let options = parse(&["--setup", argument]).unwrap();
        assert!(options.setup);
        assert!(options.developer);
        assert!(!options.preview);
    }
}

#[test]
fn missing_values_and_unknown_arguments_report_the_existing_error() {
    let cases: &[(&[&str], &str)] = &[
        (&["--config"], "--config requires a path"),
        (
            &["--preview", "--appearance-pack"],
            "--appearance-pack requires a manifest path",
        ),
        (&["--unknown"], "unknown desktop argument: --unknown"),
    ];
    for &(arguments, expected) in cases {
        let error = parse(arguments).err().unwrap();
        assert!(error.to_string().contains(expected));
    }
}
