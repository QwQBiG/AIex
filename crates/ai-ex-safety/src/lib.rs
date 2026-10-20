#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use ai_ex_domain::{AppError, ComponentHealth};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Even values identify active sessions; odd values mark the preceding session
// stopped. A single atomic makes revocation and recovery one ordered state change.
const STOPPED_BIT: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ScreenRead,
    MouseInput,
    KeyboardInput,
    ProcessLaunch,
}

impl std::str::FromStr for Capability {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "screen_read" => Ok(Self::ScreenRead),
            "mouse_input" => Ok(Self::MouseInput),
            "keyboard_input" => Ok(Self::KeyboardInput),
            "process_launch" => Ok(Self::ProcessLaunch),
            _ => Err(AppError::configuration(format!(
                "unknown automation capability: {value}",
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRequest {
    pub capability: Capability,
    pub target: String,
    pub rationale: String,
}

#[derive(Debug, Clone)]
pub struct SafetyPolicy {
    pub automation_enabled: bool,
    pub allowed_capabilities: BTreeSet<Capability>,
    pub allowed_targets: Vec<String>,
}

#[derive(Debug)]
pub struct Permit {
    id: Uuid,
    capability: Capability,
    target: String,
    rationale: String,
    lease: u64,
    lease_state: Arc<AtomicU64>,
}

impl Permit {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn capability(&self) -> Capability {
        self.capability
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn rationale(&self) -> &str {
        &self.rationale
    }

    pub fn ensure_active(&self) -> Result<(), AppError> {
        let current = self.lease_state.load(Ordering::Acquire);
        if current & STOPPED_BIT != 0 || current != self.lease {
            return Err(AppError::safety("permit revoked by emergency stop"));
        }
        Ok(())
    }
}

pub struct SafetyGate {
    policy: SafetyPolicy,
    lease_state: Arc<AtomicU64>,
}

impl SafetyGate {
    pub fn new(policy: SafetyPolicy) -> Self {
        Self {
            policy,
            lease_state: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn authorize(&self, request: ActionRequest) -> Result<Permit, AppError> {
        let lease = self.lease_state.load(Ordering::Acquire);
        if lease & STOPPED_BIT != 0 {
            return Err(AppError::safety("automation emergency stop is active"));
        }
        if !self.policy.automation_enabled {
            return Err(AppError::safety("automation is disabled"));
        }
        if !self
            .policy
            .allowed_capabilities
            .contains(&request.capability)
        {
            return Err(AppError::safety("automation capability is not allowed"));
        }
        if request.rationale.trim().is_empty() {
            return Err(AppError::safety("automation action requires a rationale"));
        }
        let target = request.target.trim();
        if target.is_empty() || !self.target_allowed(target) {
            return Err(AppError::safety(
                "automation target is outside allowed scopes",
            ));
        }
        let permit = Permit {
            id: Uuid::new_v4(),
            capability: request.capability,
            target: target.to_owned(),
            rationale: request.rationale,
            lease,
            lease_state: Arc::clone(&self.lease_state),
        };
        // A stop/recovery during policy validation must not issue a permit for
        // the stale session. A later stop is caught again at execution.
        permit.ensure_active()?;
        Ok(permit)
    }

    pub fn trigger_emergency_stop(&self) {
        let _ = self.lease_state.fetch_or(STOPPED_BIT, Ordering::AcqRel);
    }

    pub fn clear_emergency_stop(&mut self) {
        let _ = self
            .lease_state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |lease| {
                if lease & STOPPED_BIT == 0 {
                    None
                } else {
                    // Exhaustion stays stopped, rather than wrapping around and
                    // potentially making an ancient permit valid again.
                    lease.checked_add(1)
                }
            });
    }

    pub fn emergency_stop_active(&self) -> bool {
        self.lease_state.load(Ordering::Acquire) & STOPPED_BIT != 0
    }

    pub fn health(&self) -> ComponentHealth {
        let detail = if self.emergency_stop_active() {
            "emergency stop active"
        } else if self.policy.automation_enabled {
            "automation policy active"
        } else {
            "automation disabled"
        };
        ComponentHealth {
            component: "safety".to_owned(),
            ready: true,
            detail: detail.to_owned(),
        }
    }

    fn target_allowed(&self, target: &str) -> bool {
        self.policy
            .allowed_targets
            .iter()
            .any(|allowed| allowed == target)
    }
}

#[cfg(test)]
mod tests {
    use ai_ex_domain::ErrorKind;

    use super::*;

    fn policy(enabled: bool) -> SafetyPolicy {
        SafetyPolicy {
            automation_enabled: enabled,
            allowed_capabilities: BTreeSet::from([Capability::ScreenRead]),
            allowed_targets: vec!["VTube Studio".to_owned()],
        }
    }

    #[test]
    fn disabled_automation_denies_every_request() {
        let gate = SafetyGate::new(policy(false));
        let result = gate.authorize(ActionRequest {
            capability: Capability::ScreenRead,
            target: "VTube Studio".to_owned(),
            rationale: "inspect avatar state".to_owned(),
        });

        assert_eq!(result.unwrap_err().kind, ErrorKind::Safety);
    }

    #[test]
    fn capability_and_target_must_both_be_allowed() {
        let gate = SafetyGate::new(policy(true));
        let result = gate.authorize(ActionRequest {
            capability: Capability::MouseInput,
            target: "VTube Studio".to_owned(),
            rationale: "click a control".to_owned(),
        });

        assert!(result.is_err());
    }

    #[test]
    fn emergency_stop_revokes_existing_permits() {
        let gate = SafetyGate::new(policy(true));
        let permit = gate
            .authorize(ActionRequest {
                capability: Capability::ScreenRead,
                target: "VTube Studio".to_owned(),
                rationale: "inspect avatar state".to_owned(),
            })
            .expect("authorized request");

        assert_eq!(permit.rationale(), "inspect avatar state");
        permit.ensure_active().expect("active permit");
        gate.trigger_emergency_stop();
        assert!(permit.ensure_active().is_err());
        assert!(gate.emergency_stop_active());
    }

    fn request() -> ActionRequest {
        ActionRequest {
            capability: Capability::ScreenRead,
            target: "VTube Studio".to_owned(),
            rationale: "inspect avatar state".to_owned(),
        }
    }

    #[test]
    fn clearing_emergency_stop_does_not_revive_old_permits() {
        let mut gate = SafetyGate::new(policy(true));
        let old_permit = gate.authorize(request()).unwrap();
        gate.trigger_emergency_stop();
        assert!(gate.authorize(request()).is_err());
        gate.clear_emergency_stop();

        assert!(!gate.emergency_stop_active());
        assert_eq!(
            old_permit.ensure_active().unwrap_err().kind,
            ErrorKind::Safety
        );
        let fresh_permit = gate.authorize(request()).unwrap();
        fresh_permit.ensure_active().unwrap();
        assert_ne!(fresh_permit.id(), old_permit.id());
        assert_eq!(fresh_permit.capability(), Capability::ScreenRead);
        assert_eq!(fresh_permit.target(), "VTube Studio");
        assert_eq!(fresh_permit.rationale(), "inspect avatar state");
    }

    #[test]
    fn active_clear_is_a_noop_and_repeated_recovery_keeps_all_old_permits_revoked() {
        let mut gate = SafetyGate::new(policy(true));
        let first = gate.authorize(request()).unwrap();
        gate.clear_emergency_stop();
        first.ensure_active().unwrap();
        let mut retired = vec![first];

        for _ in 0..4 {
            gate.trigger_emergency_stop();
            gate.trigger_emergency_stop();
            gate.clear_emergency_stop();
            gate.clear_emergency_stop();
            for permit in &retired {
                assert!(permit.ensure_active().is_err());
            }
            let fresh = gate.authorize(request()).unwrap();
            fresh.ensure_active().unwrap();
            retired.push(fresh);
        }
    }

    #[test]
    fn arc_gate_clones_share_cross_thread_revocation_without_recovery_reviving_permits() {
        let gate = Arc::new(SafetyGate::new(policy(true)));
        let clone = Arc::clone(&gate);
        let first = gate.authorize(request()).unwrap();
        let second = clone.authorize(request()).unwrap();
        std::thread::spawn(move || clone.trigger_emergency_stop())
            .join()
            .unwrap();

        // Joining establishes that the stop completed before either check.
        assert!(gate.emergency_stop_active());
        assert!(first.ensure_active().is_err());
        assert!(second.ensure_active().is_err());
        let mut gate = Arc::try_unwrap(gate).ok().expect("sole gate owner");
        gate.clear_emergency_stop();
        assert!(!gate.emergency_stop_active());
        assert!(first.ensure_active().is_err());
        assert!(second.ensure_active().is_err());
        gate.authorize(request()).unwrap().ensure_active().unwrap();
    }

    #[test]
    fn independent_gates_keep_independent_sessions() {
        let mut stopped = SafetyGate::new(policy(true));
        let other = SafetyGate::new(policy(true));
        let stopped_permit = stopped.authorize(request()).unwrap();
        let other_permit = other.authorize(request()).unwrap();

        stopped.trigger_emergency_stop();
        assert!(stopped_permit.ensure_active().is_err());
        other_permit.ensure_active().unwrap();
        stopped.clear_emergency_stop();
        assert!(stopped_permit.ensure_active().is_err());
        other_permit.ensure_active().unwrap();
        stopped
            .authorize(request())
            .unwrap()
            .ensure_active()
            .unwrap();
    }

    #[test]
    fn exhausted_session_counter_stays_stopped_instead_of_wrapping() {
        let mut gate = SafetyGate::new(policy(true));
        gate.lease_state.store(u64::MAX - 1, Ordering::Release);
        let final_permit = gate.authorize(request()).unwrap();
        final_permit.ensure_active().unwrap();

        gate.trigger_emergency_stop();
        gate.clear_emergency_stop();
        gate.clear_emergency_stop();
        assert!(gate.emergency_stop_active());
        assert!(final_permit.ensure_active().is_err());
        assert!(gate.authorize(request()).is_err());
        assert_eq!(gate.health().detail, "emergency stop active");
    }
}
