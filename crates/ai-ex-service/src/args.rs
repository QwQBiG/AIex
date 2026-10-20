use std::path::PathBuf;

use ai_ex_domain::AppError;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub config: PathBuf,
    pub check: bool,
    pub validate_config: bool,
    pub reindex_memory: bool,
    pub maintain_media: bool,
    pub apply_media_maintenance: bool,
    pub media_capacity: usize,
    pub archive_memory: Option<PathBuf>,
    pub serve: bool,
    pub managed: bool,
    pub prompt: Option<String>,
    pub replay_events: Option<PathBuf>,
    pub replay_report: Option<PathBuf>,
    pub replay_automation: Option<PathBuf>,
    pub replay_stage: Option<PathBuf>,
    pub template_game: Option<PathBuf>,
    pub vision_image: Option<PathBuf>,
    pub vision_prompt: Option<String>,
}

impl Args {
    pub fn parse(values: impl IntoIterator<Item = String>) -> Result<Self, AppError> {
        let mut config = PathBuf::from("config/ai-ex.example.toml");
        let mut check = false;
        let mut validate_config = false;
        let mut reindex_memory = false;
        let mut maintain_media = false;
        let mut apply_media_maintenance = false;
        let mut media_capacity = None;
        let mut archive_memory = None;
        let mut serve = false;
        let mut managed = false;
        let mut prompt = None;
        let mut replay_events = None;
        let mut replay_report = None;
        let mut replay_automation = None;
        let mut replay_stage = None;
        let mut template_game = None;
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
                "--reindex-memory" => reindex_memory = true,
                "--maintain-media" => maintain_media = true,
                "--apply-media-maintenance" => apply_media_maintenance = true,
                "--media-capacity" => {
                    let capacity = values
                        .next()
                        .ok_or_else(|| {
                            AppError::configuration("--media-capacity requires a positive integer")
                        })?
                        .parse::<usize>()
                        .map_err(|_| AppError::configuration("invalid media capacity"))?;
                    if capacity == 0 {
                        return Err(AppError::configuration("media capacity must be positive"));
                    }
                    media_capacity = Some(capacity);
                }
                "--archive-memory" => {
                    archive_memory = Some(PathBuf::from(values.next().ok_or_else(|| {
                        AppError::configuration("--archive-memory requires a new path")
                    })?));
                }
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
                "--template-game" => {
                    let path = values.next().ok_or_else(|| {
                        AppError::configuration("--template-game requires a manifest path")
                    })?;
                    template_game = Some(PathBuf::from(path));
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
                        "usage: ai-ex-service [--config PATH] [--validate-config | --reindex-memory | --maintain-media [--media-capacity N] [--apply-media-maintenance --archive-memory NEW.jsonl] | --serve | --managed | --check | --replay-events PATH [--replay-report PATH] | --replay-automation PATH | --replay-stage PATH | --template-game MANIFEST.json | --prompt TEXT | \
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
        if !maintain_media
            && (apply_media_maintenance || media_capacity.is_some() || archive_memory.is_some())
        {
            return Err(AppError::configuration(
                "media maintenance options require --maintain-media",
            ));
        }
        if archive_memory.is_some() && !apply_media_maintenance {
            return Err(AppError::configuration(
                "--archive-memory requires --apply-media-maintenance",
            ));
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
            + usize::from(reindex_memory)
            + usize::from(maintain_media)
            + usize::from(check)
            + usize::from(serve)
            + usize::from(managed)
            + usize::from(replay_events.is_some())
            + usize::from(replay_automation.is_some())
            + usize::from(replay_stage.is_some())
            + usize::from(template_game.is_some())
            + usize::from(prompt.is_some())
            + usize::from(vision_image.is_some());
        if selected_modes > 1 {
            return Err(AppError::configuration(
                "--validate-config, --reindex-memory, --maintain-media, --serve, --managed, --check, --replay-events, --replay-automation, --replay-stage, --template-game, --prompt, and vision analysis modes are mutually exclusive",
            ));
        }
        Ok(Self {
            config,
            check,
            validate_config,
            reindex_memory,
            maintain_media,
            apply_media_maintenance,
            media_capacity: media_capacity.unwrap_or(1000),
            archive_memory,
            serve,
            managed,
            prompt,
            replay_events,
            replay_report,
            replay_automation,
            replay_stage,
            template_game,
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
    fn reindex_memory_is_explicit_and_exclusive() {
        assert!(
            Args::parse(["--reindex-memory".to_owned()])
                .unwrap()
                .reindex_memory
        );
        for other in ["--check", "--serve", "--managed", "--validate-config"] {
            assert!(Args::parse(["--reindex-memory".to_owned(), other.to_owned()]).is_err());
        }
        for other in [
            "--replay-events",
            "--replay-automation",
            "--replay-stage",
            "--prompt",
        ] {
            assert!(
                Args::parse([
                    "--reindex-memory".to_owned(),
                    other.to_owned(),
                    "value".to_owned()
                ])
                .is_err()
            );
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

#[cfg(test)]
mod maintenance_args_tests {
    use super::*;
    #[test]
    fn maintenance_mode_and_apply_options_are_explicit() {
        let args = Args::parse(["--maintain-media".to_owned()]).unwrap();
        assert!(args.maintain_media);
        assert!(!args.apply_media_maintenance);
        assert_eq!(args.media_capacity, 1000);
        let args = Args::parse(
            [
                "--maintain-media",
                "--apply-media-maintenance",
                "--archive-memory",
                "new.jsonl",
                "--media-capacity",
                "10",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(args.media_capacity, 10);
        assert!(args.apply_media_maintenance);
        for values in [
            vec!["--media-capacity", "1"],
            vec!["--maintain-media", "--media-capacity", "0"],
            vec!["--maintain-media", "--archive-memory", "new"],
            vec!["--maintain-media", "--serve"],
            vec!["--maintain-media", "--media-capacity"],
            vec!["--archive-memory"],
        ] {
            assert!(Args::parse(values.into_iter().map(str::to_owned)).is_err());
        }
    }
}

#[cfg(test)]
mod template_game_args_tests {
    use super::*;
    #[test]
    fn template_game_requires_a_path_and_is_exclusive() {
        assert!(Args::parse(["--template-game".into()]).is_err());
        let args = Args::parse(["--template-game".into(), "fixture.json".into()]).unwrap();
        assert_eq!(args.template_game, Some(PathBuf::from("fixture.json")));
        for conflict in [
            vec!["--serve"],
            vec!["--managed"],
            vec!["--check"],
            vec!["--maintain-media"],
            vec!["--reindex-memory"],
            vec!["--validate-config"],
            vec!["--prompt", "hello"],
            vec!["--replay-events", "events.jsonl"],
            vec!["--replay-stage", "stage.jsonl"],
            vec!["--replay-automation", "actions.jsonl"],
            vec!["--vision-image", "image.png", "--vision-prompt", "describe"],
        ] {
            assert!(
                Args::parse(
                    ["--template-game", "fixture.json"]
                        .into_iter()
                        .chain(conflict)
                        .map(str::to_owned)
                )
                .is_err()
            );
        }
    }
}
