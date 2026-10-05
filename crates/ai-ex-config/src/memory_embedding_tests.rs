use super::*;

const EMBEDDING: &str = r#"
[memory.embedding]
base_url = 'http://127.0.0.1:11434'
model = 'local-encoder:test'
space = 'local-encoder@revision-1'
timeout_seconds = 10
"#;

#[test]
fn embedding_is_opt_in_and_round_trips_without_network_access() {
    assert!(AppConfig::parse("").unwrap().memory.embedding.is_none());
    assert!(
        !AppConfig::default()
            .to_toml()
            .unwrap()
            .contains("[memory.embedding]")
    );
    let config = AppConfig::parse(EMBEDDING).unwrap();
    let encoded = config.to_toml().unwrap();
    let reloaded = AppConfig::parse(&encoded).unwrap();
    let embedding = reloaded.memory.embedding.unwrap();
    assert_eq!(embedding.model, "local-encoder:test");
    assert_eq!(embedding.space, "local-encoder@revision-1");
    assert_eq!(embedding.timeout_seconds, 10);
}

#[test]
fn rejects_missing_space_credentials_and_unbounded_timeouts() {
    for invalid in [
        EMBEDDING.replace("space = 'local-encoder@revision-1'", ""),
        EMBEDDING.replace("'local-encoder@revision-1'", "' '"),
        EMBEDDING.replace("'local-encoder:test'", "''"),
        EMBEDDING.replace("http://127.0.0.1:11434", "file:///model"),
        EMBEDDING.replace("http://127.0.0.1:11434", "http://user:secret@localhost"),
        EMBEDDING.replace("timeout_seconds = 10", "timeout_seconds = 0"),
        EMBEDDING.replace("timeout_seconds = 10", "timeout_seconds = 301"),
    ] {
        assert!(AppConfig::parse(&invalid).is_err());
    }
}

#[test]
fn desktop_config_merge_preserves_embedding_settings() {
    let mut config = AppConfig::parse(EMBEDDING).unwrap();
    config.persona.name = "Changed persona".to_owned();
    let merged = config.merge_toml(EMBEDDING).unwrap();
    assert_eq!(
        AppConfig::parse(&merged)
            .unwrap()
            .memory
            .embedding
            .unwrap()
            .space,
        "local-encoder@revision-1"
    );
}
