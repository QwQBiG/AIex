#![forbid(unsafe_code)]

use std::collections::VecDeque;

use ai_ex_domain::AppError;
use ai_ex_safety::Permit;
use async_trait::async_trait;

use crate::{ActionResult, AutomationAction, AutomationPort, ScreenFrame};

pub struct DryRunAutomationPort {
    actions: VecDeque<AutomationAction>,
    capacity: usize,
    screen: ScreenSource,
}

enum ScreenSource {
    Repeat(ScreenFrame),
    Sequence(VecDeque<ScreenFrame>),
}

impl DryRunAutomationPort {
    pub fn new(capacity: usize, width: u32, height: u32) -> Result<Self, AppError> {
        let pixel_count = usize::try_from(width)
            .ok()
            .zip(usize::try_from(height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
            .ok_or_else(|| AppError::configuration("dry-run screen dimensions overflow"))?;
        let byte_count = pixel_count
            .checked_mul(4)
            .ok_or_else(|| AppError::configuration("dry-run screen byte size overflow"))?;
        let screen = ScreenFrame::new(width, height, vec![0; byte_count])?;
        Self::with_screen(capacity, screen)
    }

    pub fn with_screen(capacity: usize, screen: ScreenFrame) -> Result<Self, AppError> {
        if capacity == 0 {
            return Err(AppError::configuration(
                "automation dry-run capacity must be positive",
            ));
        }
        let screen = ScreenFrame::new(screen.width, screen.height, screen.rgba)?;
        Ok(Self {
            actions: VecDeque::with_capacity(capacity),
            capacity,
            screen: ScreenSource::Repeat(screen),
        })
    }

    /// Consume one owned frame per successful capture; exhaustion is an adapter error.
    pub fn with_frames(capacity: usize, frames: Vec<ScreenFrame>) -> Result<Self, AppError> {
        if capacity == 0 {
            return Err(AppError::configuration(
                "automation dry-run capacity must be positive",
            ));
        }
        let frames = frames
            .into_iter()
            .map(|frame| ScreenFrame::new(frame.width, frame.height, frame.rgba))
            .collect::<Result<VecDeque<_>, _>>()?;
        Ok(Self {
            actions: VecDeque::with_capacity(capacity),
            capacity,
            screen: ScreenSource::Sequence(frames),
        })
    }

    pub fn actions(&self) -> impl Iterator<Item = &AutomationAction> {
        self.actions.iter()
    }

    pub fn len(&self) -> usize {
        self.actions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub fn clear(&mut self) {
        self.actions.clear();
    }
}

#[async_trait]
impl AutomationPort for DryRunAutomationPort {
    async fn execute(
        &mut self,
        permit: &Permit,
        action: &AutomationAction,
    ) -> Result<ActionResult, AppError> {
        permit.ensure_active()?;
        action.validate()?;
        if permit.capability() != action.required_capability() {
            return Err(AppError::safety(
                "automation permit capability does not match action",
            ));
        }
        if self.actions.len() >= self.capacity {
            return Err(AppError::unavailable("automation dry-run queue is full"));
        }
        let result = match action {
            AutomationAction::CaptureScreen => {
                let frame = match &mut self.screen {
                    ScreenSource::Repeat(frame) => frame.clone(),
                    ScreenSource::Sequence(frames) => frames.pop_front().ok_or_else(|| {
                        AppError::unavailable("automation dry-run frame sequence is exhausted")
                    })?,
                };
                ActionResult::ScreenCaptured(frame)
            }
            _ => ActionResult::Completed,
        };
        self.actions.push_back(action.clone());
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ai_ex_safety::{ActionRequest, Capability, SafetyGate, SafetyPolicy};

    use super::*;

    fn permit(capability: Capability) -> (SafetyGate, Permit) {
        let gate = SafetyGate::new(SafetyPolicy {
            automation_enabled: true,
            allowed_capabilities: BTreeSet::from([Capability::ScreenRead, Capability::MouseInput]),
            allowed_targets: vec!["game".to_owned()],
        });
        let permit = gate
            .authorize(ActionRequest {
                capability,
                target: "game".to_owned(),
                rationale: "dry-run test".to_owned(),
            })
            .expect("permit authorizes");
        (gate, permit)
    }

    #[tokio::test]
    async fn captures_deterministic_frame_and_records_action() {
        let (_gate, permit) = permit(Capability::ScreenRead);
        let mut port = DryRunAutomationPort::new(4, 2, 1).expect("dry-run creates");
        let result = port
            .execute(&permit, &AutomationAction::CaptureScreen)
            .await
            .expect("capture succeeds");
        let ActionResult::ScreenCaptured(frame) = result else {
            panic!("capture returns a frame");
        };
        assert_eq!((frame.width, frame.height), (2, 1));
        assert_eq!(frame.rgba, vec![0; 8]);
        assert_eq!(port.len(), 1);
    }

    #[tokio::test]
    async fn rejects_capability_mismatch_and_revoked_permit() {
        let (gate, permit) = permit(Capability::ScreenRead);
        let mut port = DryRunAutomationPort::new(2, 1, 1).expect("dry-run creates");
        let mismatch = port
            .execute(&permit, &AutomationAction::MovePointer { x: 0.5, y: 0.5 })
            .await
            .expect_err("screen permit cannot move pointer");
        assert_eq!(mismatch.kind, ai_ex_domain::ErrorKind::Safety);
        gate.trigger_emergency_stop();
        let revoked = port
            .execute(&permit, &AutomationAction::CaptureScreen)
            .await
            .expect_err("revoked permit is rejected");
        assert_eq!(revoked.kind, ai_ex_domain::ErrorKind::Safety);
        assert!(port.is_empty());
    }

    #[test]
    fn rejects_zero_capacity() {
        assert!(DryRunAutomationPort::new(0, 1, 1).is_err());
    }
}
