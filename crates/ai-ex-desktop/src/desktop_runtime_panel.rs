use std::time::{Duration, Instant};

use ai_ex_domain::{
    AppError, DesktopRuntimeReply, DesktopRuntimeRequest, DesktopRuntimeSettings,
    DesktopRuntimeSnapshot, DiscoveredVtsHotkey,
};
use eframe::egui;
use uuid::Uuid;

use crate::worker::WorkerCommand;

#[cfg(test)]
mod tests;
mod view;

#[derive(Default)]
pub struct DesktopRuntimePanel {
    instance: Option<Uuid>,
    connected: bool,
    confirmed: Option<DesktopRuntimeSnapshot>,
    draft: DesktopRuntimeSettings,
    draft_revision: u64,
    sequence: u64,
    pending: std::collections::BTreeMap<u64, DesktopRuntimeRequest>,
    poll_at: Option<Instant>,
    fresh: bool,
    feedback: Option<String>,
    vts: Vec<DiscoveredVtsHotkey>,
}

impl DesktopRuntimePanel {
    pub fn synchronize(&mut self, instance: Option<Uuid>, connected: bool) {
        if self.instance != instance {
            self.instance = instance;
            self.confirmed = None;
            self.pending.clear();
            self.fresh = false;
            self.poll_at = None;
            self.vts.clear();
        }
        if !connected {
            self.fresh = false;
            self.pending.clear();
            self.poll_at = None;
        }
        self.connected = connected && instance.is_some();
    }

    pub fn poll(&mut self) -> Option<WorkerCommand> {
        if !self.connected
            || self
                .poll_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
            || self
                .pending
                .values()
                .any(|request| matches!(request, DesktopRuntimeRequest::State))
        {
            return None;
        }
        self.poll_at = Some(Instant::now());
        self.begin(DesktopRuntimeRequest::State)
    }

    pub fn begin(&mut self, request: DesktopRuntimeRequest) -> Option<WorkerCommand> {
        if !self.connected || self.pending.len() >= 8 {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        self.pending.insert(self.sequence, request.clone());
        Some(WorkerCommand::DesktopRuntime {
            request_id: self.sequence,
            request,
        })
    }

    pub fn receive(&mut self, id: u64, result: Result<DesktopRuntimeReply, AppError>) {
        let Some(request) = self.pending.remove(&id) else {
            return;
        };
        match result {
            Ok(DesktopRuntimeReply::State(snapshot)) => {
                if !self.connected
                    || Some(snapshot.instance_id) != self.instance
                    || self
                        .confirmed
                        .as_ref()
                        .is_some_and(|old| old.revision > snapshot.revision)
                {
                    return;
                }
                let replace_draft = self.confirmed.is_none()
                    || matches!(&request, DesktopRuntimeRequest::Apply { settings, .. } if *settings == self.draft)
                    || matches!(request, DesktopRuntimeRequest::Reload { .. })
                    || self
                        .confirmed
                        .as_ref()
                        .is_some_and(|old| self.draft == old.settings);
                if replace_draft {
                    self.draft = snapshot.settings.clone();
                    self.draft_revision = snapshot.revision;
                }
                self.fresh = true;
                self.confirmed = Some(snapshot);
            }
            Ok(DesktopRuntimeReply::Diagnostics(report)) => {
                if Some(report.desktop.instance_id) != self.instance || !self.connected {
                    return;
                }
                self.feedback = Some(match export_report(&report) {
                    Ok(path) => format!("诊断已导出：{}", path.display()),
                    Err(error) => format!("诊断导出失败：{error}"),
                });
            }
            Ok(DesktopRuntimeReply::VtsDiscovery(hotkeys)) => {
                self.vts = hotkeys;
                self.feedback = Some(format!("当前 VTS 模型有 {} 个热键。", self.vts.len()));
            }
            Err(error) => {
                if matches!(
                    request,
                    DesktopRuntimeRequest::Apply { .. } | DesktopRuntimeRequest::Reload { .. }
                ) {
                    self.fresh = false;
                }
                self.feedback = Some(format!("操作未确认，请刷新核对：{error}"));
            }
        }
    }

    pub fn captions_visible(&self) -> bool {
        self.connected
            && self.fresh
            && self
                .confirmed
                .as_ref()
                .is_some_and(|state| state.settings.captions_visible)
    }

    pub fn close_captions(&mut self) -> Option<WorkerCommand> {
        let state = self.confirmed.as_ref()?;
        if !self.connected
            || !self.fresh
            || !state.settings.captions_visible
            || self.pending.values().any(|request| {
                matches!(
                    request,
                    DesktopRuntimeRequest::Apply { .. } | DesktopRuntimeRequest::Reload { .. }
                )
            })
        {
            return None;
        }
        let mut settings = state.settings.clone();
        settings.captions_visible = false;
        self.begin(DesktopRuntimeRequest::Apply {
            instance_id: state.instance_id,
            expected_revision: state.revision,
            settings,
        })
    }

    fn mutation(&self, reload: bool) -> Option<DesktopRuntimeRequest> {
        let state = self.confirmed.as_ref()?;
        if !self.connected
            || !self.fresh
            || state.revision != self.draft_revision
            || self.pending.values().any(|request| {
                matches!(
                    request,
                    DesktopRuntimeRequest::Apply { .. } | DesktopRuntimeRequest::Reload { .. }
                )
            })
        {
            return None;
        }
        if reload {
            Some(DesktopRuntimeRequest::Reload {
                instance_id: state.instance_id,
                expected_revision: state.revision,
            })
        } else {
            self.draft.validate().ok()?;
            Some(DesktopRuntimeRequest::Apply {
                instance_id: state.instance_id,
                expected_revision: state.revision,
                settings: self.draft.clone(),
            })
        }
    }
}

fn export_report(
    report: &ai_ex_domain::DesktopDiagnosticReport,
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let path = std::env::current_dir()
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("desktop-diagnostics-{}.json", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(path)
}
