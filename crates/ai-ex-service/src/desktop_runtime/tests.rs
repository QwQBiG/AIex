use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct Actions {
    proactive: AtomicUsize,
    cancelled: AtomicUsize,
    emergency: AtomicUsize,
    minute: AtomicUsize,
}

#[async_trait]
impl DesktopRuntimeActions for Actions {
    async fn interrupt(&self) -> Result<(), AppError> {
        Ok(())
    }
    async fn emergency_stop(&self) -> Result<(), AppError> {
        self.emergency.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn pause_native(&self) -> Result<(), AppError> {
        Ok(())
    }
    fn resume_native(&self) -> Result<tokio::task::JoinHandle<Result<(), AppError>>, AppError> {
        Ok(tokio::spawn(async { Ok(()) }))
    }
    fn native_step(&self) -> Result<tokio::task::JoinHandle<Result<(), AppError>>, AppError> {
        Ok(tokio::spawn(async { std::future::pending().await }))
    }
    fn revoke_native(&self) -> Result<(), AppError> {
        Ok(())
    }
    async fn proactive(&self) -> Result<(), AppError> {
        self.proactive.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
    async fn cancel_proactive(&self) -> Result<(), AppError> {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn local_minute(&self) -> Option<u16> {
        Some(self.minute.load(Ordering::SeqCst) as u16)
    }
}

struct Fixture(PathBuf);

struct Parameters;

struct FilterParameters;

#[async_trait]
impl VtsDiscoveryPort for FilterParameters {
    async fn discover(&self) -> Result<Vec<DiscoveredVtsHotkey>, AppError> {
        Ok(Vec::new())
    }

    async fn check_mappings(&self) -> Result<VtsMappingCheck, AppError> {
        Err(AppError::unavailable("not used by this fixture"))
    }

    async fn discover_parameters(&self) -> Result<ai_ex_vts::VtsParameterList, AppError> {
        let parameter = |name: String, is_custom| ai_ex_vts::VtsParameter {
            name,
            added_by: "fixture".into(),
            value: 0.5,
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            is_custom,
        };
        let mut parameters = (0..20)
            .map(|index| parameter(format!("other-{index}"), false))
            .collect::<Vec<_>>();
        parameters.extend((0..17).map(|index| parameter(format!("Mouth-{index}"), false)));
        parameters.push(parameter("LIP-VOICE".into(), true));
        parameters.push(parameter("VoIcE-level".into(), true));
        Ok(ai_ex_vts::VtsParameterList {
            model_loaded: false,
            parameters,
        })
    }
}

#[async_trait]
impl VtsDiscoveryPort for Parameters {
    async fn discover(&self) -> Result<Vec<DiscoveredVtsHotkey>, AppError> {
        Ok(Vec::new())
    }

    async fn check_mappings(&self) -> Result<VtsMappingCheck, AppError> {
        Ok(VtsMappingCheck {
            model_loaded: true,
            mappings: vec![ai_ex_domain::VtsMappingStatus {
                emotion: ai_ex_domain::Emotion::Happy,
                status: ai_ex_domain::VtsMappingState::Matched,
                hotkey_id: Some("smile".into()),
                id_truncated: false,
            }],
        })
    }

    async fn discover_parameters(&self) -> Result<ai_ex_vts::VtsParameterList, AppError> {
        Ok(ai_ex_vts::VtsParameterList {
            model_loaded: true,
            parameters: (0..18)
                .map(|index| ai_ex_vts::VtsParameter {
                    name: format!("parameter-{index}"),
                    added_by: "fixture".into(),
                    value: 1.5,
                    min: 0.0,
                    max: 1.0,
                    default_value: 0.5,
                    is_custom: index > 0,
                })
                .collect(),
        })
    }
}

#[tokio::test]
async fn mapping_check_forwards_the_port_result_and_reports_unavailable() {
    let fixture = Fixture::new();
    let runtime = DesktopRuntime::new(
        Uuid::new_v4(),
        fixture.path(),
        Arc::new(Actions::default()),
        Some(Arc::new(Parameters)),
    )
    .unwrap();
    let DesktopRuntimeReply::VtsMappingCheck(check) = runtime
        .handle(DesktopRuntimeRequest::CheckVtsMappings)
        .await
        .unwrap()
    else {
        panic!("unexpected VTS response")
    };
    assert!(check.model_loaded);
    assert_eq!(
        check.mappings[0].status,
        ai_ex_domain::VtsMappingState::Matched
    );
    runtime.stop().await.unwrap();

    let disabled = DesktopRuntime::new(
        Uuid::new_v4(),
        fixture.path(),
        Arc::new(Actions::default()),
        None,
    )
    .unwrap();
    assert!(
        disabled
            .handle(DesktopRuntimeRequest::CheckVtsMappings)
            .await
            .is_err()
    );
    disabled.stop().await.unwrap();
}

#[tokio::test]
async fn vts_parameter_discovery_is_bounded_and_preserves_out_of_range_values() {
    let fixture = Fixture::new();
    let runtime = DesktopRuntime::new(
        Uuid::new_v4(),
        fixture.path(),
        Arc::new(Actions::default()),
        Some(Arc::new(Parameters)),
    )
    .unwrap();
    let DesktopRuntimeReply::VtsParameters(page) = runtime
        .handle(DesktopRuntimeRequest::DiscoverVtsParameters { offset: 16 })
        .await
        .unwrap()
    else {
        panic!("unexpected VTS response")
    };
    assert_eq!(
        (page.offset, page.total, page.parameters.len()),
        (16, 18, 2)
    );
    assert_eq!(page.parameters[0].value.as_f64(), Some(1.5));
    assert!(
        runtime
            .handle(DesktopRuntimeRequest::DiscoverVtsParameters { offset: 1025 })
            .await
            .is_err()
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn vts_parameter_filter_runs_before_pagination_and_preserves_sources() {
    let fixture = Fixture::new();
    let runtime = DesktopRuntime::new(
        Uuid::new_v4(),
        fixture.path(),
        Arc::new(Actions::default()),
        Some(Arc::new(FilterParameters)),
    )
    .unwrap();
    let request =
        |filter, offset| DesktopRuntimeRequest::DiscoverVtsParametersFiltered { filter, offset };
    let DesktopRuntimeReply::VtsFilteredParameters { filter, page } = runtime
        .handle(request(VtsParameterFilter::Mouth, 16))
        .await
        .unwrap()
    else {
        panic!("unexpected VTS response")
    };
    assert_eq!(filter, VtsParameterFilter::Mouth);
    assert!(!page.model_loaded);
    assert_eq!(
        (page.offset, page.total, page.parameters.len()),
        (16, 18, 2)
    );
    assert_eq!(page.parameters[0].name, "Mouth-16");
    assert!(!page.parameters[0].is_custom);
    assert_eq!(page.parameters[1].name, "LIP-VOICE");
    assert!(page.parameters[1].is_custom);
    let DesktopRuntimeReply::VtsFilteredParameters { page, .. } = runtime
        .handle(request(VtsParameterFilter::Voice, 0))
        .await
        .unwrap()
    else {
        panic!("unexpected VTS response")
    };
    assert_eq!(page.total, 2);
    assert_eq!(page.parameters[0].name, "LIP-VOICE");
    assert_eq!(page.parameters[1].name, "VoIcE-level");
    let DesktopRuntimeReply::VtsFilteredParameters { page, .. } = runtime
        .handle(request(VtsParameterFilter::Voice, 16))
        .await
        .unwrap()
    else {
        panic!("unexpected VTS response")
    };
    assert_eq!((page.offset, page.total, page.parameters.len()), (2, 2, 0));
    assert!(
        runtime
            .handle(request(VtsParameterFilter::Mouth, 1025))
            .await
            .is_err()
    );
    runtime.stop().await.unwrap();
}

impl Fixture {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("desktop-behavior-test-{}", Uuid::new_v4())))
    }
    fn path(&self) -> PathBuf {
        self.0.join("settings.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _file = std::fs::remove_file(self.path());
        let _directory = std::fs::remove_dir(&self.0);
    }
}

#[tokio::test]
async fn loading_saved_hotkeys_does_not_register_and_apply_is_cas() {
    let fixture = Fixture::new();
    let saved = DesktopRuntimeSettings {
        hotkeys_enabled: true,
        ..Default::default()
    };
    persistence::save(&fixture.path(), &saved).unwrap();
    let id = Uuid::new_v4();
    let runtime =
        DesktopRuntime::new(id, fixture.path(), Arc::new(Actions::default()), None).unwrap();
    assert!(!runtime.snapshot().await.hotkeys_registered);
    assert!(runtime.state.lock().await.hotkeys.is_none());
    assert!(
        runtime
            .apply(Uuid::new_v4(), 0, Default::default())
            .await
            .is_err()
    );
    runtime.apply(id, 0, Default::default()).await.unwrap();
    assert_eq!(runtime.snapshot().await.revision, 1);
    assert!(!persistence::load(&fixture.path()).unwrap().hotkeys_enabled);
    assert!(runtime.apply(id, 0, saved).await.is_err());
    assert!(!runtime.snapshot().await.settings.hotkeys_enabled);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn failed_save_keeps_confirmed_state_and_proactive_disable_cancels_only_its_port() {
    let fixture = Fixture::new();
    let actions = Arc::new(Actions::default());
    let id = Uuid::new_v4();
    let runtime = DesktopRuntime::new(id, fixture.path(), actions.clone(), None).unwrap();
    let enabled = DesktopRuntimeSettings {
        proactive_enabled: true,
        cooldown_seconds: 10,
        ..Default::default()
    };
    runtime.apply(id, 0, enabled).await.unwrap();
    runtime.state.lock().await.last_proactive = Instant::now() - Duration::from_secs(11);
    assert!(runtime.tick().await);
    tokio::task::yield_now().await;
    assert_eq!(actions.proactive.load(Ordering::SeqCst), 1);
    runtime.tick().await;
    assert_eq!(actions.proactive.load(Ordering::SeqCst), 1);
    runtime.apply(id, 1, Default::default()).await.unwrap();
    assert_eq!(actions.cancelled.load(Ordering::SeqCst), 1);
    assert!(runtime.state.lock().await.proactive_task.is_none());
    let before = runtime.snapshot().await;
    std::fs::remove_file(fixture.path()).unwrap();
    std::fs::create_dir(fixture.path()).unwrap();
    assert!(
        runtime
            .apply(id, before.revision, Default::default())
            .await
            .is_err()
    );
    assert_eq!(runtime.snapshot().await, before);
    std::fs::remove_dir(fixture.path()).unwrap();
    runtime.stop().await.unwrap();
    assert!(!runtime.tick().await);
}

#[test]
fn quiet_hours_cross_midnight_and_diagnostics_have_no_freeform_data() {
    let mut settings = DesktopRuntimeSettings {
        proactive_enabled: true,
        quiet_hours: Some(ai_ex_domain::QuietHours {
            start_minute: 1320,
            end_minute: 480,
        }),
        ..Default::default()
    };
    assert!(!settings.allows_proactive(Some(60)));
    assert!(!settings.allows_proactive(None));
    assert!(settings.allows_proactive(Some(600)));
    settings.do_not_disturb = true;
    assert!(!settings.allows_proactive(Some(600)));
    settings.quiet_hours.as_mut().unwrap().end_minute = 1320;
    assert!(settings.validate().is_err());
}

#[derive(Default)]
struct KeyState {
    registrations: usize,
    shutdowns: usize,
    reject: bool,
    events: Vec<(i32, u64)>,
}
struct FakeKeys(Arc<std::sync::Mutex<KeyState>>);
impl hotkeys::OwnedHotkeys for FakeKeys {
    fn poll(&mut self) -> Result<Vec<(i32, u64)>, AppError> {
        Ok(std::mem::take(&mut self.0.lock().unwrap().events))
    }
    fn shutdown(&mut self) -> Result<(), AppError> {
        self.0.lock().unwrap().shutdowns += 1;
        Ok(())
    }
}

#[tokio::test]
async fn registration_failure_is_a_new_confirmed_revision_and_retired_events_are_ignored() {
    let fixture = Fixture::new();
    let keys = Arc::new(std::sync::Mutex::new(KeyState::default()));
    let id = Uuid::new_v4();
    let mut runtime =
        DesktopRuntime::new(id, fixture.path(), Arc::new(Actions::default()), None).unwrap();
    let factory_keys = keys.clone();
    runtime.hotkey_factory = Arc::new(move |_| {
        let mut state = factory_keys.lock().unwrap();
        state.registrations += 1;
        if state.reject {
            return Err(AppError::unavailable("occupied fake binding"));
        }
        Ok(Box::new(FakeKeys(factory_keys.clone())))
    });
    let settings = DesktopRuntimeSettings {
        hotkeys_enabled: true,
        ..Default::default()
    };
    runtime.apply(id, 0, settings.clone()).await.unwrap();
    assert!(runtime.snapshot().await.hotkeys_registered);
    assert!(runtime.execute_hotkey(5, 0).await.is_err());
    assert!(!runtime.snapshot().await.settings.captions_visible);
    keys.lock().unwrap().events = vec![(5, 0)];
    runtime.tick().await;
    assert!(!runtime.snapshot().await.settings.captions_visible);
    keys.lock().unwrap().events = vec![(5, 1)];
    runtime.tick().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.snapshot().await.revision < 2 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let toggled = runtime.snapshot().await;
    assert!(toggled.settings.captions_visible);
    assert_eq!(toggled.revision, 2);
    assert!(persistence::load(&fixture.path()).unwrap().captions_visible);
    runtime.apply(id, 2, Default::default()).await.unwrap();
    assert_eq!(keys.lock().unwrap().shutdowns, 1);
    keys.lock().unwrap().reject = true;
    runtime.apply(id, 3, settings.clone()).await.unwrap();
    let failed = runtime.snapshot().await;
    assert_eq!(failed.revision, 4);
    assert!(failed.settings.hotkeys_enabled && failed.hotkeys_error);
    assert!(!failed.hotkeys_registered);
    assert_eq!(keys.lock().unwrap().shutdowns, 1);
    keys.lock().unwrap().reject = false;
    runtime.apply(id, 4, settings).await.unwrap();
    assert!(runtime.snapshot().await.hotkeys_registered);
    runtime.stop().await.unwrap();
    assert_eq!(keys.lock().unwrap().shutdowns, 2);
}

#[tokio::test]
async fn quiet_transition_cancels_finished_generation_audio_once() {
    let fixture = Fixture::new();
    let actions = Arc::new(Actions::default());
    actions.minute.store(60, Ordering::SeqCst);
    let id = Uuid::new_v4();
    let runtime = DesktopRuntime::new(id, fixture.path(), actions.clone(), None).unwrap();
    let settings = DesktopRuntimeSettings {
        proactive_enabled: true,
        quiet_hours: Some(ai_ex_domain::QuietHours {
            start_minute: 120,
            end_minute: 240,
        }),
        ..Default::default()
    };
    runtime.apply(id, 0, settings).await.unwrap();
    assert!(runtime.state.lock().await.proactive_task.is_none());
    actions.minute.store(180, Ordering::SeqCst);
    runtime.tick().await;
    assert_eq!(actions.cancelled.load(Ordering::SeqCst), 1);
    runtime.tick().await;
    assert_eq!(actions.cancelled.load(Ordering::SeqCst), 1);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn a_pending_native_step_never_blocks_polling_the_emergency_shortcut() {
    let fixture = Fixture::new();
    let keys = Arc::new(std::sync::Mutex::new(KeyState::default()));
    let actions = Arc::new(Actions::default());
    let id = Uuid::new_v4();
    let mut runtime = DesktopRuntime::new(id, fixture.path(), actions.clone(), None).unwrap();
    let factory_keys = keys.clone();
    runtime.hotkey_factory = Arc::new(move |_| Ok(Box::new(FakeKeys(factory_keys.clone()))));
    runtime
        .apply(
            id,
            0,
            DesktopRuntimeSettings {
                hotkeys_enabled: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    keys.lock().unwrap().events = vec![(6, 1)];
    runtime.tick().await;
    assert!(
        runtime.state.lock().await.native_tasks[1]
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    );
    keys.lock().unwrap().events = vec![(2, 1), (6, 1)];
    runtime.tick().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while actions.emergency.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert!(runtime.state.lock().await.native_tasks[1].is_none());
    runtime.stop().await.unwrap();
}
