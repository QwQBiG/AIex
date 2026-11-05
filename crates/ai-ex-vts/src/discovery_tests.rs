use super::*;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;

type FakeSocket = WebSocketStream<TcpStream>;

async fn connected() -> (VtsClient, FakeSocket) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let path = std::env::temp_dir().join(format!("vts-discovery-{}.json", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, br#"{"token":"synthetic-token"}"#)
        .await
        .unwrap();
    let settings = VtsSettings {
        host: "127.0.0.1".into(),
        port: listener.local_addr().unwrap().port(),
        token_path: path.clone(),
        plugin_name: "Fixture".into(),
        developer: "Fixture".into(),
        expression_hotkeys: BTreeMap::new(),
    };
    let (client, server) = tokio::join!(VtsClient::connect(settings), async {
        let (socket, _) = listener.accept().await.unwrap();
        let mut server = accept_async(socket).await.unwrap();
        let authentication = request(&mut server).await;
        assert_eq!(authentication["messageType"], "AuthenticationRequest");
        assert_eq!(
            authentication["data"]["authenticationToken"],
            "synthetic-token"
        );
        server
            .send(Message::Text(
                json!({"messageType":"AuthenticationResponse","data":{"authenticated":true}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        server
    });
    tokio::fs::remove_file(path).await.unwrap();
    (client.unwrap(), server)
}

async fn request(server: &mut FakeSocket) -> Value {
    let message = tokio::time::timeout(std::time::Duration::from_secs(2), server.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&message.into_text().unwrap()).unwrap()
}

async fn response(server: &mut FakeSocket, id: &Value, hotkeys: Value) {
    server
        .send(Message::Text(
            json!({"apiName":"VTubeStudioPublicAPI","apiVersion":"1.0",
        "messageType":"HotkeysInCurrentModelResponse","requestID":id,
        "data":{"modelLoaded":true,"availableHotkeys":hotkeys}})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn discovery_uses_current_model_and_keeps_mouth_and_hotkey_output_working() {
    let (client, mut server) = connected().await;
    let (result, ()) = tokio::join!(client.discover_hotkeys(), async {
        let discovery = request(&mut server).await;
        assert_eq!(discovery["messageType"], "HotkeysInCurrentModelRequest");
        assert_eq!(discovery["data"], json!({}));
        response(&mut server, &json!("unrelated"), json!([])).await;
        client.send(Command::Mouth(0.5)).await.unwrap();
        let mouth = request(&mut server).await;
        assert_eq!(mouth["messageType"], "InjectParameterDataRequest");
        assert_eq!(mouth["data"]["parameterValues"][0]["value"], 0.5);
        client.clone().trigger_hotkey("existing").await.unwrap();
        assert_eq!(request(&mut server).await["data"]["hotkeyID"], "existing");
        response(
            &mut server,
            &discovery["requestID"],
            json!([
            {"hotkeyID":"expression-1","name":"微笑","file":"ignored.exp3.json"},
            {"hotkeyID":"expression-2","name":""}]),
        )
        .await;
    });
    assert_eq!(
        result.unwrap(),
        vec![
            VtsHotkey {
                id: "expression-1".into(),
                name: "微笑".into()
            },
            VtsHotkey {
                id: "expression-2".into(),
                name: String::new()
            }
        ]
    );
}

#[tokio::test]
async fn discovery_matches_concurrent_responses_by_request_id_and_ignores_cancelled_calls() {
    let (client, mut server) = connected().await;
    let cancelled = {
        let discovery = client.discover_hotkeys();
        tokio::pin!(discovery);
        tokio::select! {
            request = request(&mut server) => request,
            result = &mut discovery => panic!("unexpected response before the fake server replied: {result:?}"),
        }
    };
    let (first, second, ()) = tokio::join!(
        client.discover_hotkeys(),
        client.discover_hotkeys(),
        async {
            let first = request(&mut server).await;
            let second = request(&mut server).await;
            response(
                &mut server,
                &cancelled["requestID"],
                json!([{ "hotkeyID":"late","name":"late"}]),
            )
            .await;
            response(
                &mut server,
                &second["requestID"],
                json!([{ "hotkeyID":"second","name":"第二"}]),
            )
            .await;
            response(
                &mut server,
                &first["requestID"],
                json!([{ "hotkeyID":"first","name":"第一"}]),
            )
            .await;
        }
    );
    assert_eq!(first.unwrap()[0].id, "first");
    assert_eq!(second.unwrap()[0].id, "second");
    assert!(VtsClient::disabled().discover_hotkeys().await.is_err());
}

#[tokio::test]
async fn rejected_requests_and_closed_connections_return_errors_without_fabricated_hotkeys() {
    let (client, mut server) = connected().await;
    let (result, ()) = tokio::join!(client.discover_hotkeys(), async {
        let request = request(&mut server).await;
        server
            .send(Message::Text(
                json!({"messageType":"APIError","requestID":request["requestID"],
            "data":{"errorID":1,"message":"synthetic rejected"}})
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
    });
    assert!(result.is_err());
    let (result, ()) = tokio::join!(client.discover_hotkeys(), async {
        request(&mut server).await;
        server.close(None).await.unwrap();
    });
    assert!(result.is_err());
}

#[tokio::test]
async fn parameter_and_hotkey_discovery_correlate_out_of_order_responses() {
    let (client, mut server) = connected().await;
    let (parameters, hotkeys, ()) = tokio::join!(
        client.discover_parameters(),
        client.discover_hotkeys(),
        async {
            let first = request(&mut server).await;
            let second = request(&mut server).await;
            assert_eq!(first["messageType"], "InputParameterListRequest");
            assert_eq!(first["data"], json!({}));
            assert_eq!(second["messageType"], "HotkeysInCurrentModelRequest");
            response(
                &mut server,
                &second["requestID"],
                json!([{ "hotkeyID":"smile","name":"微笑"}]),
            )
            .await;
            server.send(Message::Text(json!({
                "messageType":"InputParameterListResponse", "requestID": first["requestID"],
                "data": {"modelLoaded":true,"defaultParameters":[{"name":"MouthOpen","addedBy":"VTube Studio","value":0.5,"min":0,"max":1,"defaultValue":0}],"customParameters":[]}
            }).to_string().into())).await.unwrap();
        }
    );
    assert_eq!(hotkeys.unwrap()[0].id, "smile");
    assert_eq!(parameters.unwrap().parameters[0].name, "MouthOpen");
    assert!(VtsClient::disabled().discover_parameters().await.is_err());
}

#[tokio::test]
async fn rejected_parameter_discovery_does_not_fabricate_values() {
    let (client, mut server) = connected().await;
    let (result, ()) = tokio::join!(client.discover_parameters(), async {
        let request = request(&mut server).await;
        server.send(Message::Text(json!({"messageType":"APIError","requestID":request["requestID"],"data":{"errorID":1}}).to_string().into())).await.unwrap();
    });
    assert!(result.is_err());
}

#[tokio::test]
async fn mapping_check_uses_active_exact_keys_and_never_triggers_actions() {
    let (mut client, mut server) = connected().await;
    client.expression_hotkeys = BTreeMap::from([
        ("happy".into(), "smile".into()),
        ("angry".into(), "missing".into()),
        ("sad".into(), "z".repeat(300)),
        (" SURPRISED ".into(), "alias-not-active".into()),
    ]);
    let (result, ()) = tokio::join!(client.check_mappings(), async {
        let request = request(&mut server).await;
        assert_eq!(request["messageType"], "HotkeysInCurrentModelRequest");
        response(
            &mut server,
            &request["requestID"],
            json!([
                {"hotkeyID":"smile","name":"微笑"},
                {"hotkeyID":"alias-not-active","name":"alias"}
            ]),
        )
        .await;
    });
    let result = result.unwrap();
    assert!(result.model_loaded);
    assert_eq!(
        result.mappings.iter().map(|m| m.status).collect::<Vec<_>>(),
        vec![
            VtsMappingState::Unconfigured,
            VtsMappingState::Matched,
            VtsMappingState::Missing,
            VtsMappingState::Missing,
            VtsMappingState::Unconfigured,
        ]
    );
    assert_eq!(result.mappings[1].hotkey_id.as_deref(), Some("smile"));
    assert_eq!(result.mappings[3].hotkey_id.as_ref().unwrap().len(), 256);
    assert!(result.mappings[3].id_truncated);
    assert!(result.mappings[4].hotkey_id.is_none());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), server.next())
            .await
            .is_err()
    );

    let (unloaded, ()) =
        tokio::join!(client.check_mappings(), async {
            let request = request(&mut server).await;
            server.send(Message::Text(json!({
            "messageType":"HotkeysInCurrentModelResponse", "requestID":request["requestID"],
            "data":{"modelLoaded":false,"availableHotkeys":[]}
        }).to_string().into())).await.unwrap();
        });
    let unloaded = unloaded.unwrap();
    assert!(!unloaded.model_loaded);
    assert_eq!(unloaded.mappings[0].status, VtsMappingState::Unconfigured);
    assert_eq!(unloaded.mappings[1].status, VtsMappingState::Unknown);

    let (empty, ()) = tokio::join!(client.check_mappings(), async {
        let request = request(&mut server).await;
        response(&mut server, &request["requestID"], json!([])).await;
    });
    assert_eq!(empty.unwrap().mappings[1].status, VtsMappingState::Missing);
    assert!(VtsClient::disabled().check_mappings().await.is_err());
}
