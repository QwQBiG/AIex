use super::*;

#[test]
fn portable_setup_keeps_relative_data_paths_and_does_not_store_credentials() {
    let files = Files::new();
    let mut app = files.app(Some(
        include_str!("../../../config/ai-ex.portable.example.toml").to_owned(),
    ));
    app.api_key = "test-secret-never-persist".to_owned();
    let document = app.config_text(0).unwrap();
    let config = AppConfig::parse(&document).unwrap();
    assert_eq!(config.control.token_path, "data/control.token");
    assert_eq!(config.memory.path, "data/memory.jsonl");
    assert!(config.memory.enabled);
    assert!(config.desktop.auto_start_service);
    assert!(!config.tts.enabled);
    assert!(!config.duplex.enabled);
    assert!(!document.contains("test-secret-never-persist"));
}

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("aiex-setup-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn app(&self, original: Option<String>) -> SetupApp {
        SetupApp::new(
            self.0.join("profile.toml"),
            Arc::new(Mutex::new(None)),
            original,
        )
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        for name in ["profile.toml", "control.token"] {
            let _ignored = std::fs::remove_file(self.0.join(name));
        }
        let _ignored = std::fs::remove_dir(&self.0);
    }
}

#[test]
fn setup_round_trips_custom_text_and_persists_startup_without_rotating_token() {
    let files = Files::new();
    let mut app = files.app(None);
    app.provider = ProviderChoice::Ollama;
    app.provider_changed();
    app.persona_name = "小\"白\\夜\n猫".to_owned();
    let document = app.config_text(0).unwrap();
    crate::setup_storage::save(&app.config_path, &document, None).unwrap();
    let loaded = crate::startup::read_config(&app.config_path).unwrap();
    assert_eq!(loaded.persona.name, app.persona_name);
    assert!(app.memory_enabled);
    assert!(loaded.memory.enabled);
    assert!(loaded.desktop.auto_start_service);
    assert!(std::path::Path::new(&loaded.control.token_path).is_absolute());
    let token = crate::startup::read_token(&loaded).unwrap();
    let mut reopened = files.app(Some(document.clone()));
    assert_eq!(reopened.persona_name, app.persona_name);
    assert!(reopened.start_service);
    reopened.start_service = false;
    let updated = reopened.config_text(0).unwrap();
    crate::setup_storage::save(&app.config_path, &updated, Some(&document)).unwrap();
    assert!(
        !crate::startup::read_config(&app.config_path)
            .unwrap()
            .desktop
            .auto_start_service
    );
    assert_eq!(crate::startup::read_token(&loaded).unwrap(), token);
    assert!(crate::setup_storage::save(&app.config_path, &document, Some(&document)).is_err());
    assert_eq!(std::fs::read_to_string(&app.config_path).unwrap(), updated);
}

#[test]
fn setup_preserves_memory_persona_and_extension_fields() {
    let files = Files::new();
    let source = "[memory]\nenabled = true\npath = 'my-history.jsonl'\n[persona]\nprofile_id = 'friend'\nrevision = 9\nsystem_prompt = 'remember our adventures'\n[bilibili]\ncookie_env = 'OLD_COOKIE'\n[extensions]\ncustom_animation = 'wave'\n";
    let mut app = files.app(Some(source.to_owned()));
    assert!(app.memory_enabled);
    assert!(!app.start_service);
    app.bilibili_cookie_env.clear();
    let updated = app.config_text(0).unwrap();
    let config = AppConfig::parse(&updated).unwrap();
    assert!(config.memory.enabled);
    assert_eq!(config.memory.path, "my-history.jsonl");
    assert_eq!(config.persona.profile_id, "friend");
    assert_eq!(config.persona.revision, 9);
    assert_eq!(config.persona.system_prompt, "remember our adventures");
    assert!(config.bilibili.cookie_env.is_none());
    assert!(updated.contains("custom_animation = \"wave\""));
    app.persona_name = "new name".to_owned();
    assert_eq!(
        AppConfig::parse(&app.config_text(0).unwrap())
            .unwrap()
            .persona
            .revision,
        10
    );
}

#[test]
fn memory_switch_preserves_existing_records_paths_and_other_settings() {
    let files = Files::new();
    let memory_path = files.0.join("existing-memory.jsonl");
    let memory_content = "existing private history\n";
    std::fs::write(&memory_path, memory_content).unwrap();
    let mut initial = AppConfig::default();
    initial.memory.enabled = false;
    initial.memory.path = memory_path.to_string_lossy().into_owned();
    initial.control.token_path = files.0.join("control.token").to_string_lossy().into_owned();
    initial.persona.profile_id = "existing-character".to_owned();
    let original = format!(
        "{}\n[extensions]\ncustom_animation = 'wave'\n",
        initial.to_toml().unwrap()
    );
    let mut app = files.app(Some(original.clone()));
    std::fs::write(&app.config_path, &original).unwrap();
    assert!(!app.memory_enabled);
    app.memory_enabled = true;
    let enabled = app.config_text(0).unwrap();
    crate::setup_storage::save(&app.config_path, &enabled, Some(&original)).unwrap();
    let saved = crate::startup::read_config(&app.config_path).unwrap();
    assert!(saved.memory.enabled);
    assert_eq!(saved.memory.path, initial.memory.path);
    assert_eq!(saved.persona.profile_id, initial.persona.profile_id);
    assert_eq!(saved.persona.revision, initial.persona.revision);
    assert!(enabled.contains("custom_animation = \"wave\""));
    let token = crate::startup::read_token(&saved).unwrap();
    let mut reopened = files.app(Some(enabled.clone()));
    assert!(reopened.memory_enabled);
    reopened.memory_enabled = false;
    let disabled = reopened.config_text(0).unwrap();
    crate::setup_storage::save(&app.config_path, &disabled, Some(&enabled)).unwrap();
    let saved = crate::startup::read_config(&app.config_path).unwrap();
    assert!(!saved.memory.enabled);
    assert_eq!(saved.memory.path, initial.memory.path);
    assert_eq!(crate::startup::read_token(&saved).unwrap(), token);
    assert_eq!(
        std::fs::read_to_string(&memory_path).unwrap(),
        memory_content
    );
    assert!(!files.app(Some(disabled.clone())).memory_enabled);
    assert!(crate::setup_storage::save(&app.config_path, &enabled, Some(&enabled)).is_err());
    assert_eq!(std::fs::read_to_string(&app.config_path).unwrap(), disabled);
    std::fs::remove_file(memory_path).unwrap();
}

#[test]
fn memory_switch_is_visible_and_can_be_toggled_without_a_native_window() {
    use eframe::App;
    for size in [[640.0, 520.0], [760.0, 620.0]] {
        let files = Files::new();
        let mut app = files.app(None);
        let context = egui::Context::default();
        crate::ui::theme::configure_appearance(&context);
        let mut frame = eframe::Frame::_new_kittest();
        let mut render = |events| {
            context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size.into())),
                    events,
                    ..Default::default()
                },
                |ui| app.ui(ui, &mut frame),
            )
        };
        render(Vec::new());
        let output = render(Vec::new());
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == "启用长期记忆" => {
                    let center = text.pos + text.galley.size() * 0.5;
                    assert!(shape.clip_rect.contains(center));
                    Some(center)
                }
                _ => None,
            })
            .expect("memory switch is visible at the initial scroll position");
        for pressed in [true, false] {
            render(vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(!app.memory_enabled);
        assert!(
            !AppConfig::parse(&app.config_text(0).unwrap())
                .unwrap()
                .memory
                .enabled
        );
        assert!(app.result.lock().unwrap().is_none());
        assert!(!app.config_path.exists());
    }
}

#[test]
fn invalid_existing_token_is_preserved_and_configuration_is_not_created() {
    let files = Files::new();
    std::fs::write(files.0.join("control.token"), "invalid").unwrap();
    let app = files.app(None);
    assert!(
        crate::setup_storage::save(&app.config_path, &app.config_text(0).unwrap(), None).is_err()
    );
    assert!(!app.config_path.exists());
    assert_eq!(
        std::fs::read_to_string(files.0.join("control.token")).unwrap(),
        "invalid"
    );
}

#[cfg(windows)]
#[test]
fn occupied_configuration_keeps_original_and_cleans_temporary_file() {
    use std::os::windows::fs::OpenOptionsExt;
    let files = Files::new();
    let app = files.app(None);
    let original = app.config_text(0).unwrap();
    crate::setup_storage::save(&app.config_path, &original, None).unwrap();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&app.config_path)
        .unwrap();
    let mut changed = files.app(Some(original.clone()));
    changed.start_service = false;
    assert!(
        crate::setup_storage::save(
            &app.config_path,
            &changed.config_text(0).unwrap(),
            Some(&original)
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&app.config_path).unwrap(), original);
    assert_eq!(std::fs::read_dir(&files.0).unwrap().count(), 2);
    drop(locked);
}
