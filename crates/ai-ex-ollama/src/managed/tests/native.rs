use super::*;
use std::time::Instant;

async fn answer(model: &mut ManagedOllama, text: &str) -> String {
    let mut receiver = model
        .stream(ModelRequest {
            turn_id: TurnId::new(),
            messages: vec![Message::new(Role::User, text)],
        })
        .await
        .unwrap();
    let mut output = String::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(180), receiver.recv())
        .await
        .unwrap()
    {
        output.push_str(&chunk.unwrap());
    }
    output
}

#[tokio::test]
#[ignore = "requires explicit verified public model/runtime paths; no devices are opened"]
async fn verified_native_instance_answers_cancels_restarts_and_releases_private_model() {
    let executable =
        std::env::var_os("AIEX_TEST_MANAGED_OLLAMA_EXE").expect("explicit runtime path");
    let source =
        std::env::var_os("AIEX_TEST_MANAGED_MODEL_SOURCE").expect("explicit model source path");
    let private =
        std::env::var_os("AIEX_TEST_MANAGED_STATE_DIR").expect("explicit private state path");
    let settings = ManagedOllamaSettings {
        executable: executable.into(),
        model_source: source.into(),
        private_directory: private.into(),
        startup_timeout: Duration::from_secs(120),
        request_timeout: Duration::from_secs(120),
    };
    let source = settings.model_source.clone();
    let private = settings.private_directory.clone();
    let mut model = ManagedOllama::new(settings).unwrap();
    let cold_turn = TurnId::new();
    let cold_receiver = model
        .stream(ModelRequest {
            turn_id: cold_turn,
            messages: vec![Message::new(Role::User, "本轮将在准备期间取消。")],
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let cold_cancel = Instant::now();
    model.cancel(cold_turn).await.unwrap();
    let cold_cancel_ms = cold_cancel.elapsed().as_millis();
    assert!(model.state.lock().await.preparation_absent());
    assert!(model.state.lock().await.instance_absent());
    model.clear_prepared_model().await.unwrap();
    drop(cold_receiver);
    let began = Instant::now();
    assert_eq!(
        answer(&mut model, "请只回复：原生运行验证通过。")
            .await
            .trim(),
        "原生运行验证通过。"
    );
    let cold_ms = began.elapsed().as_millis();
    assert!(model.health().await.ready);
    let turn = TurnId::new();
    let mut receiver = model
        .stream(ModelRequest {
            turn_id: turn,
            messages: vec![Message::new(
                Role::User,
                "请直接写一万字中文冒险小说，持续写正文，不要总结。",
            )],
        })
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(120), receiver.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!first.is_empty());
    let cancellation = Instant::now();
    model.cancel(turn).await.unwrap();
    let cancellation_ms = cancellation.elapsed().as_millis();
    assert!(cancellation_ms < 2000);
    assert!(model.state.lock().await.instance_absent());
    while tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .is_some()
    {}
    assert!(!model.health().await.ready);
    assert_eq!(
        answer(&mut model, "请只回复：第二轮完成。").await.trim(),
        "第二轮完成。"
    );
    assert!(model.health().await.ready);
    model.clear_prepared_model().await.unwrap();
    assert!(model.state.lock().await.instance_absent());
    assert!(
        !private
            .join("models/blobs")
            .join(format!("sha256-{}", MANAGED_MODEL_FILES[0].sha256))
            .exists()
    );
    verify_managed_model_source(&source).unwrap();
    println!(
        "{}",
        json!({"native_cpu":true,"cold_answer_ms":cold_ms,"cancel_ms":cancellation_ms,
        "cold_prepare_cancel_ms":cold_cancel_ms,"preparation_drained":true,
        "restart":true,"private_model_cleared":true,"source_preserved":true})
    );
}
