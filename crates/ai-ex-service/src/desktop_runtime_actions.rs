//! Bridges confirmed behavior intents to service owners, without device authority.
use crate::desktop_runtime::{DesktopRuntime, DesktopRuntimeActions, VtsDiscoveryPort};
use crate::{
    conversation_activity::ConversationActivity, native_desktop::NativeDesktopHost,
    native_runtime::NativeRuntime, template_host::TemplateHost,
};
use ai_ex_config::AppConfig;
use ai_ex_domain::{AppError, ErrorKind};
use ai_ex_safety::SafetyGate;
use ai_ex_vts::VtsClient;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

struct Actions {
    runtime: ConversationActivity,
    safety: Arc<SafetyGate>,
    automation: TemplateHost,
    desktop: NativeDesktopHost,
    native: Option<Arc<NativeRuntime>>,
}

#[async_trait]
impl DesktopRuntimeActions for Actions {
    async fn interrupt(&self) -> Result<(), AppError> {
        let preview = match &self.native {
            Some(native) => native
                .request(ai_ex_domain::NativeRequest::StopPreview)
                .await
                .map(|_| ()),
            None => Ok(()),
        };
        let conversation = interrupt(&self.runtime, "global shortcut").await;
        preview.and(conversation)
    }
    async fn emergency_stop(&self) -> Result<(), AppError> {
        self.safety.trigger_emergency_stop();
        let desktop = self.desktop.stop_current("global emergency stop", true);
        let automation = self.automation.emergency_stop();
        let conversation = self.interrupt().await;
        desktop.and(automation).and(conversation)
    }
    async fn pause_native(&self) -> Result<(), AppError> {
        self.desktop.stop_current("global pause", false)
    }
    fn resume_native(&self) -> Result<tokio::task::JoinHandle<Result<(), AppError>>, AppError> {
        self.desktop.start_resume_current()
    }
    fn native_step(&self) -> Result<tokio::task::JoinHandle<Result<(), AppError>>, AppError> {
        self.desktop.start_current_step()
    }
    fn revoke_native(&self) -> Result<(), AppError> {
        self.desktop
            .stop_current("shortcut generation retired", false)
    }
    async fn proactive(&self) -> Result<(), AppError> {
        if self.safety.emergency_stop_active() {
            return Err(AppError::safety("emergency stop is active"));
        }
        self.runtime.proactive().await.map(|_| ())
    }
    async fn cancel_proactive(&self) -> Result<(), AppError> {
        self.runtime.cancel_proactive().await
    }
    fn local_minute(&self) -> Option<u16> {
        ai_ex_platform_windows::local_minute()
    }
}

async fn interrupt(runtime: &ConversationActivity, reason: &str) -> Result<(), AppError> {
    match runtime.interrupt(reason).await {
        Err(error) if error.kind == ErrorKind::InvalidTransition => Ok(()),
        result => result,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create(
    instance: Uuid,
    config_path: &Path,
    config: &AppConfig,
    runtime: ConversationActivity,
    safety: Arc<SafetyGate>,
    automation: TemplateHost,
    desktop: NativeDesktopHost,
    native: Option<Arc<NativeRuntime>>,
    vts: VtsClient,
) -> Result<DesktopRuntime, AppError> {
    let directory = std::path::absolute(config_path)
        .map_err(|error| AppError::configuration(error.to_string()))?;
    let directory = directory
        .parent()
        .ok_or_else(|| AppError::configuration("configuration directory unavailable"))?;
    let discovery: Option<Arc<dyn VtsDiscoveryPort>> = if config.vts.enabled && vts.health().ready {
        Some(Arc::new(vts))
    } else {
        None
    };
    DesktopRuntime::new(
        instance,
        directory.join("desktop-runtime-v1.json"),
        Arc::new(Actions {
            runtime,
            safety,
            automation,
            desktop,
            native,
        }),
        discovery,
    )
}
