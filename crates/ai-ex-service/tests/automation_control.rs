//! Actual managed-service/control-channel coverage using only supplied PNGs and local model fixtures.
use std::time::Duration;

use ai_ex_control::{ControlCommand, ControlPayload};
use ai_ex_domain::{
    AutomationControlMode as Mode, AutomationPauseReason as PauseReason,
    AutomationResumeEligibility as Eligibility, AutomationSessionPhase as Phase,
    AutomationStepOutcome, ErrorKind,
};

#[path = "support/automation_control.rs"]
mod support;
use support::{Model, Service, pause, resume, session, stop};

#[tokio::test]
async fn template_control_retries_are_idempotent_and_stale_mutations_are_rejected() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let model = Model::start().await;
        let mut service = Service::start(&model.url);
        service.ready().await;
        let (start, active) = service.start_replay().await;
        let retried = service.automation(start).await;
        assert_eq!(session(&retried).session_id, session(&active).session_id);
        assert_eq!(
            session(&retried).operation_id,
            session(&active).operation_id
        );
        assert!(retried.projection_sequence >= active.projection_sequence);

        let paused = service.automation(pause(&retried)).await;
        assert_eq!(session(&paused).control_mode, Mode::PausedForChat);
        assert_eq!(session(&paused).pause_reason, Some(PauseReason::Manual));
        assert!(paused.revision > retried.revision);

        let mut old_instance = paused.clone();
        old_instance.instance_id = "a7b0f925-1101-4001-8001-000000000099".parse().unwrap();
        let mut old_session = paused.clone();
        old_session.session.as_mut().unwrap().session_id =
            "a7b0f925-1101-4001-8001-000000000098".parse().unwrap();
        for request in [pause(&old_instance), pause(&old_session), resume(&retried)] {
            let error = service
                .client
                .send(ControlCommand::Automation { request })
                .await
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidTransition);
        }
        assert_eq!(
            session(&service.status().await).session_id,
            session(&paused).session_id
        );

        let paused_again = service.automation(pause(&paused)).await;
        assert!(
            paused_again.revision > paused.revision,
            "manual pause revokes prior ownership even when already paused"
        );
        let resumed = service.automation(resume(&paused_again)).await;
        assert_eq!(session(&resumed).control_mode, Mode::Running);
        // Stop remains useful after a concurrent pause/resume advances this session's revision.
        service.automation(stop(&active)).await;
        let finished = service
            .until(|snapshot| session(snapshot).phase == Phase::Finished)
            .await;
        assert_eq!(session(&finished).control_mode, Mode::Stopped);
        assert_eq!(
            session(&finished).latest_outcome,
            Some(AutomationStepOutcome::Stopped)
        );
        assert!(
            service
                .client
                .send(ControlCommand::Automation {
                    request: resume(&finished)
                })
                .await
                .is_err()
        );
        service.close_owner().await;
    })
    .await
    .expect("control lifecycle completes within 20 seconds");
}

#[tokio::test]
async fn queued_chat_holds_replay_and_manual_pause_survives_interruption() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut model = Model::start().await;
        let mut service = Service::start(&model.url);
        service.ready().await;
        service.start_replay().await;

        service.submit("hold this synthetic generation").await;
        let first = model.next().await;
        assert_eq!(
            first.request["messages"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["content"],
            "hold this synthetic generation"
        );
        let first_pause = service.status().await;
        assert_eq!(session(&first_pause).control_mode, Mode::PausedForChat);
        assert_eq!(session(&first_pause).pause_reason, Some(PauseReason::Chat));
        assert_eq!(session(&first_pause).resume_eligibility, Eligibility::Busy);
        assert!(session(&first_pause).chat_deadline_ms.is_some());

        service.submit("hold this synthetic generation").await;
        model.assert_no_queued_generation_started();
        let queued = service.status().await;
        assert_eq!(session(&queued).control_mode, Mode::PausedForChat);
        assert_eq!(session(&queued).resume_eligibility, Eligibility::Busy);
        assert_eq!(
            session(&queued).control_generation,
            session(&first_pause).control_generation
        );
        assert!(session(&queued).chat_deadline_ms >= session(&first_pause).chat_deadline_ms);
        let error = service
            .client
            .send(ControlCommand::Automation {
                request: resume(&queued),
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidTransition);

        first.complete();
        let second = model.next().await;
        let generating = service.status().await;
        assert_eq!(session(&generating).control_mode, Mode::PausedForChat);
        assert_eq!(session(&generating).resume_eligibility, Eligibility::Busy);
        assert_eq!(
            session(&generating).frames,
            session(&queued).frames,
            "finishing one generation cannot resume while another admitted turn remains"
        );
        let manual = service.automation(pause(&generating)).await;
        assert_eq!(session(&manual).pause_reason, Some(PauseReason::Manual));
        assert!(manual.revision > generating.revision);
        assert!(matches!(
            service
                .client
                .send(ControlCommand::Interrupt {
                    reason: "synthetic test interruption".to_owned(),
                })
                .await
                .unwrap(),
            ControlPayload::Accepted
        ));
        second.cancelled().await;

        let idle = service
            .until(|snapshot| session(snapshot).resume_eligibility == Eligibility::Idle)
            .await;
        assert_eq!(session(&idle).control_mode, Mode::PausedForChat);
        assert_eq!(session(&idle).pause_reason, Some(PauseReason::Manual));
        assert!(
            session(&idle).chat_deadline_ms.is_none(),
            "manual pause leaves no automatic resume claim"
        );
        assert_eq!(session(&idle).frames, session(&manual).frames);
        // No 30-second sleep or production clock override: explicit resume is allowed once idle.
        let resumed = service.automation(resume(&idle)).await;
        assert_eq!(session(&resumed).control_mode, Mode::Running);
        service.automation(stop(&resumed)).await;
        service
            .until(|snapshot| session(snapshot).phase == Phase::Finished)
            .await;
        service.close_owner().await;
    })
    .await
    .expect("queued generation arbitration completes within 20 seconds");
}

#[tokio::test]
async fn emergency_stop_is_terminal_even_when_chat_finishes_or_is_interrupted() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut model = Model::start().await;
        let mut service = Service::start(&model.url);
        service.ready().await;
        service.start_replay().await;
        service.submit("pending at emergency stop").await;
        let generation = model.next().await;
        let before = service.status().await;
        assert_eq!(session(&before).resume_eligibility, Eligibility::Busy);
        assert!(matches!(
            service
                .client
                .send(ControlCommand::EmergencyStop)
                .await
                .unwrap(),
            ControlPayload::Accepted
        ));
        generation.cancelled().await;
        let finished = service
            .until(|snapshot| session(snapshot).phase == Phase::Finished)
            .await;
        assert_eq!(session(&finished).session_id, session(&before).session_id);
        assert_eq!(session(&finished).control_mode, Mode::Stopped);
        assert_eq!(
            session(&finished).latest_outcome,
            Some(AutomationStepOutcome::Stopped)
        );
        assert!(
            service
                .client
                .send(ControlCommand::Automation {
                    request: resume(&finished)
                })
                .await
                .is_err()
        );
        let refreshed = service.status().await;
        assert_eq!(session(&refreshed).phase, Phase::Finished);
        assert_eq!(session(&refreshed).control_mode, Mode::Stopped);
        assert_eq!(session(&refreshed).frames, session(&finished).frames);
        assert_eq!(session(&refreshed).actions, session(&finished).actions);
        service.close_owner().await;
    })
    .await
    .expect("emergency stop completes within 20 seconds");
}

#[tokio::test]
async fn owner_disconnect_drains_active_replay_and_cancels_pending_generation() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut model = Model::start().await;
        let mut service = Service::start(&model.url);
        service.ready().await;
        service.start_replay().await;
        service
            .submit("owner will disconnect before this model replies")
            .await;
        let generation = model.next().await;
        let paused = service.status().await;
        assert_eq!(session(&paused).phase, Phase::Active);
        assert_eq!(session(&paused).control_mode, Mode::PausedForChat);
        assert_eq!(session(&paused).resume_eligibility, Eligibility::Busy);
        service.close_owner().await;
        generation.cancelled().await;
    })
    .await
    .expect("owner shutdown completes within 20 seconds");
}

#[tokio::test]
async fn held_visual_strategy_does_not_block_fast_frames_and_applies_through_control() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut model = Model::start().await;
        let mut vision = Model::start().await;
        let mut service = Service::start_with_strategy(&model.url, &vision.url);
        service.ready().await;
        service.start_replay().await;

        // This is the real configured OllamaVisionClient request from an audited replay frame.
        let observation = vision.next().await;
        assert_eq!(observation.request["model"], "synthetic-vision");
        assert_eq!(observation.request["stream"], false);
        let user = observation.request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "user")
            .unwrap();
        assert!(
            user["content"]
                .as_str()
                .unwrap()
                .contains(support::STRATEGY_PROMPT)
        );
        let images = user["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert!(
            images[0].as_str().unwrap().starts_with("iVBORw0KGgo"),
            "strategy observes the synthetic PNG"
        );
        model.assert_no_queued_generation_started();

        let held = service.status().await;
        assert_eq!(session(&held).phase, Phase::Active);
        assert_eq!(session(&held).control_mode, Mode::Running);
        let advancing = service
            .until(|snapshot| session(snapshot).frames >= session(&held).frames + 2)
            .await;
        assert_eq!(session(&advancing).phase, Phase::Active);
        assert_eq!(session(&advancing).control_mode, Mode::Running);
        assert_eq!(session(&advancing).resume_eligibility, Eligibility::Idle);
        assert_eq!(
            session(&advancing).control_generation,
            session(&held).control_generation
        );
        assert!(
            session(&advancing).actions > session(&held).actions,
            "audited dry-run actions continue while the VLM response is held"
        );

        observation.complete_with(
            serde_json::json!({
                "type": "focus_region", "region": {"x": 2, "y": 2, "width": 20, "height": 20}
            })
            .to_string(),
        );
        let focused = service
            .until(|snapshot| {
                session(snapshot).control_generation > session(&advancing).control_generation
            })
            .await;
        assert_eq!(session(&focused).phase, Phase::Active);
        assert_eq!(session(&focused).control_mode, Mode::Running);
        assert!(session(&focused).last_error.is_none());
        assert!(session(&focused).frames >= session(&advancing).frames);
        model.assert_no_queued_generation_started();

        // Stop reaches the same owner after its slow worker changed the fast-loop plan.
        service.automation(stop(&focused)).await;
        let finished = service
            .until(|snapshot| session(snapshot).phase == Phase::Finished)
            .await;
        assert_eq!(session(&finished).control_mode, Mode::Stopped);
        assert_eq!(
            session(&finished).latest_outcome,
            Some(AutomationStepOutcome::Stopped)
        );
        let refreshed = service.status().await;
        assert_eq!(session(&refreshed).frames, session(&finished).frames);
        assert_eq!(session(&refreshed).actions, session(&finished).actions);
        service.close_owner().await;
    })
    .await
    .expect("local visual strategy/control integration completes within 20 seconds");
}
#[tokio::test]
async fn incomplete_search_is_terminal_typed_and_never_executes_pointer_actions() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let mut model = Model::start().await;
        let mut service = Service::start(&model.url);
        service.ready().await;
        service.set_search_budget(1);
        let before = service.status().await;
        let request = ai_ex_domain::AutomationRequest::StartTemplate {
            instance_id: before.instance_id,
            expected_revision: before.revision,
            operation_id: "a7b0f925-1101-4001-8001-000000000002".parse().unwrap(),
            manifest: "manifest.json".into(),
        };
        service.automation(request.clone()).await;
        let finished = service
            .until(|snapshot| session(snapshot).phase == Phase::Finished)
            .await;
        let current = session(&finished);
        assert_eq!(
            current.latest_outcome,
            Some(AutomationStepOutcome::SearchIncomplete)
        );
        assert_eq!(current.control_mode, Mode::Stopped);
        assert_eq!(current.frames, 1);
        assert_eq!(current.actions, 0);
        let search = current
            .search
            .as_ref()
            .expect("typed incomplete search diagnostics");
        assert_eq!(
            search.reason,
            ai_ex_domain::AutomationSearchIncompleteReason::WorkBudgetExhausted
        );
        assert_eq!(
            (
                search.region.x,
                search.region.y,
                search.region.width,
                search.region.height
            ),
            (0, 0, 24, 24)
        );
        assert_eq!(search.candidates, 25);
        assert_eq!(search.verified, 0);
        assert_eq!(search.max_work, 1);
        assert!(search.work <= search.max_work);
        assert_eq!(search.work, search.exact_pixel_pairs + search.bound_work);
        assert_eq!(search.exact_evaluations, 0);
        finished.validate().unwrap();
        // Stable operation retry and later polls cannot borrow a fresh search budget.
        let retried = service.automation(request).await;
        assert_eq!(session(&retried).search, current.search);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let refreshed = service.status().await;
        assert_eq!(session(&refreshed).frames, 1);
        assert_eq!(session(&refreshed).actions, 0);
        assert_eq!(session(&refreshed).search, current.search);
        assert_eq!(
            session(&refreshed).latest_outcome,
            Some(AutomationStepOutcome::SearchIncomplete)
        );
        assert!(
            service
                .client
                .send(ControlCommand::Automation {
                    request: resume(&refreshed)
                })
                .await
                .is_err()
        );
        model.assert_no_queued_generation_started();
        service.close_owner().await;
    })
    .await
    .expect("typed incomplete search settles without retrying or model calls");
}
