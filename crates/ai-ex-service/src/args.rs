use std::path::PathBuf;

use ai_ex_domain::AppError;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub config: PathBuf,
    pub check: bool,
    pub validate_config: bool,
    pub serve: bool,
    pub managed: bool,
    pub prompt: Option<String>,
    pub replay_events: Option<PathBuf>,
    pub replay_report: Option<PathBuf>,
    pub replay_automation: Option<PathBuf>,
    pub replay_stage: Option<PathBuf>,
    pub vision_image: Option<PathBuf>,
    pub vision_prompt: Option<String>,
}

impl Args {
    pub fn parse(values: impl IntoIterator<Item = String>) -> Result<Self, AppError> {
        let mut config = PathBuf::from("config/ai-ex.example.toml");
        let mut check = false;
        let mut validate_config = false;
        let mut serve = false;
        let mut managed = false;
        let mut prompt = None;
        let mut replay_events = None;
        let mut replay_report = None;
        let mut replay_automation = None;
        let mut replay_stage = None;
        let mut vision_image = None;
        let mut vision_prompt = None;
        let mut values = values.into_iter();
        while let Some(value) = values.next() {
            match value.as_str() {
                "--config" => {
                    let path = values
                        .next()
                        .ok_or_else(|| AppError::configuration("--config requires a path"))?;
                    config = PathBuf::from(path);
                }
                "--check" => check = true,
                "--validate-config" => validate_config = true,
                "--serve" => serve = true,
                "--managed" => managed = true,
                "--replay-events" => {
                    let path = values.next().ok_or_else(|| {
                        AppError::configuration("--replay-events requires a JSONL path")
                    })?;
                    replay_events = Some(PathBuf::from(path));
                }
                "--replay-report" => {
                    let path = values.next().ok_or_else(|| {
                        AppError::configuration("--replay-report requires a JSONL path")
                    })?;
                    replay_report = Some(PathBuf::from(path));
                }
                "--replay-automation" => {
                    let path = values.next().ok_or_else(|| {
                        AppError::configuration("--replay-automation requires a JSONL path")
                    })?;
                    replay_automation = Some(PathBuf::from(path));
                }
                "--replay-stage" => {
                    let path = values.next().ok_or_else(|| {
                        AppError::configuration("--replay-stage requires a JSONL path")
                    })?;
                    replay_stage = Some(PathBuf::from(path));
                }
                "--prompt" => {
                    prompt = Some(
                        values
                            .next()
                            .ok_or_else(|| AppError::configuration("--prompt requires text"))?,
                    );
                }
                "--vision-image" => {
                    let path = values
                        .next()
                        .ok_or_else(|| AppError::configuration("--vision-image requires a path"))?;
                    vision_image = Some(PathBuf::from(path));
                }
                "--vision-prompt" => {
                    vision_prompt =
                        Some(values.next().ok_or_else(|| {
                            AppError::configuration("--vision-prompt requires text")
                        })?);
                }
                "--help" | "-h" => {
                    return Err(AppError::configuration(
                        "usage: ai-ex-service [--config PATH] [--validate-config | --serve | --managed | --check | --replay-events PATH [--replay-report PATH] | --replay-automation PATH | --replay-stage PATH | --prompt TEXT | \
                         --vision-image PATH --vision-prompt TEXT]",
                    ));
                }
                _ => {
                    return Err(AppError::configuration(format!(
                        "unknown argument: {value}"
                    )));
                }
            }
        }
        if replay_report.is_some() && replay_events.is_none() {
            return Err(AppError::configuration(
                "--replay-report requires --replay-events",
            ));
        }
        if vision_image.is_some() != vision_prompt.is_some() {
            return Err(AppError::configuration(
                "--vision-image and --vision-prompt must be used together",
            ));
        }
        let selected_modes = usize::from(validate_config)
            + usize::from(check)
            + usize::from(serve)
            + usize::from(managed)
            + usize::from(replay_events.is_some())
            + usize::from(replay_automation.is_some())
            + usize::from(replay_stage.is_some())
            + usize::from(prompt.is_some())
            + usize::from(vision_image.is_some());
        if selected_modes > 1 {
            return Err(AppError::configuration(
                "--validate-config, --serve, --managed, --check, --replay-events, --replay-automation, --replay-stage, --prompt, and vision analysis modes are mutually exclusive",
            ));
        }
        Ok(Self {
            config,
            check,
            validate_config,
            serve,
            managed,
            prompt,
            replay_events,
            replay_report,
            replay_automation,
            replay_stage,
            vision_image,
            vision_prompt,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_modes_are_explicit_and_exclusive() {
        assert!(Args::parse(["--serve".to_owned()]).unwrap().serve);
        assert!(Args::parse(["--managed".to_owned()]).unwrap().managed);
        for conflict in ["--managed", "--check", "--serve"] {
            let first = if conflict == "--serve" {
                "--managed"
            } else {
                "--serve"
            };
            assert!(Args::parse([first.to_owned(), conflict.to_owned()]).is_err());
        }
    }

    #[test]
    fn rejects_missing_prompt_value() {
        assert!(Args::parse(["--prompt".to_owned()]).is_err());
    }

    #[test]
    fn accepts_config_and_check() {
        let args = Args::parse([
            "--config".to_owned(),
            "custom.toml".to_owned(),
            "--check".to_owned(),
        ])
        .expect("arguments parse");
        assert_eq!(args.config, PathBuf::from("custom.toml"));
        assert!(args.check);
    }

    #[test]
    fn accepts_event_replay_mode() {
        let args = Args::parse(["--replay-events".to_owned(), "events.jsonl".to_owned()])
            .expect("replay arguments parse");
        assert_eq!(args.replay_events, Some(PathBuf::from("events.jsonl")));
    }
    #[test]
    fn accepts_event_replay_report() {
        let args = Args::parse([
            "--replay-events".to_owned(),
            "events.jsonl".to_owned(),
            "--replay-report".to_owned(),
            "report.jsonl".to_owned(),
        ])
        .expect("replay report arguments parse");
        assert_eq!(args.replay_report, Some(PathBuf::from("report.jsonl")));
    }

    #[test]
    fn accepts_automation_replay_mode() {
        let args = Args::parse([
            "--replay-automation".to_owned(),
            "automation.jsonl".to_owned(),
        ])
        .expect("automation replay arguments parse");
        assert_eq!(
            args.replay_automation,
            Some(PathBuf::from("automation.jsonl"))
        );
    }

    #[test]
    fn accepts_stage_replay_mode() {
        let args = Args::parse(["--replay-stage".to_owned(), "stage.jsonl".to_owned()])
            .expect("stage replay arguments parse");
        assert_eq!(args.replay_stage, Some(PathBuf::from("stage.jsonl")));
    }

    #[test]
    fn accepts_complete_vision_mode() {
        let args = Args::parse([
            "--vision-image".to_owned(),
            "screen.png".to_owned(),
            "--vision-prompt".to_owned(),
            "Describe the UI".to_owned(),
        ])
        .expect("vision arguments parse");

        assert_eq!(args.vision_image, Some(PathBuf::from("screen.png")));
        assert_eq!(args.vision_prompt.as_deref(), Some("Describe the UI"));
    }

    #[test]
    fn rejects_partial_or_conflicting_modes() {
        assert!(Args::parse(["--vision-image".to_owned(), "screen.png".to_owned(),]).is_err());
        assert!(
            Args::parse([
                "--check".to_owned(),
                "--prompt".to_owned(),
                "hello".to_owned(),
            ])
            .is_err()
        );
    }
}

#[cfg(test)]
mod offline_validation_tests {
    use super::*;

    #[test]
    fn offline_validation_is_an_exclusive_mode() {
        assert!(
            Args::parse(["--validate-config".to_owned()])
                .unwrap()
                .validate_config
        );
        for other in [
            vec!["--check"],
            vec!["--serve"],
            vec!["--managed"],
            vec!["--prompt", "hello"],
            vec!["--replay-events", "events.jsonl"],
            vec!["--replay-automation", "automation.jsonl"],
            vec!["--replay-stage", "stage.jsonl"],
            vec!["--vision-image", "image.png", "--vision-prompt", "describe"],
        ] {
            let values = std::iter::once("--validate-config")
                .chain(other)
                .map(str::to_owned);
            assert!(Args::parse(values).is_err());
        }
    }
}
