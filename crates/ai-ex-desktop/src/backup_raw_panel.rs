use super::*;
use ai_ex_control::{RawMemoryBackupState, RawMemoryBackupStatus};
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "backup_raw_panel_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct RawPanel {
    plan: Option<Plan>,
    tracked: Option<Tracked>,
    feedback: Option<&'static str>,
}

struct Plan {
    instance: Uuid,
    operation_id: Uuid,
    plan_id: Uuid,
    output: String,
    expires_at: Instant,
}

struct Tracked {
    instance: Uuid,
    operation_id: Uuid,
    output: String,
    state: RawMemoryBackupState,
}

impl BackupInspectionPanel {
    pub(super) fn invalidate_raw(&mut self) {
        if self.raw.plan.is_some() || matches!(&self.pending, Some(Pending::RawPrepare { .. })) {
            self.raw.feedback = Some("备份准备已失效，请重新准备。");
        }
        self.raw.plan = None;
        if let Some(tracked) = &mut self.raw.tracked
            && matches!(tracked.state, RawMemoryBackupState::Running)
        {
            tracked.state = RawMemoryBackupState::Unknown;
        }
    }

    pub fn raw_unresolved(&self) -> bool {
        self.raw.tracked.as_ref().is_some_and(|tracked| {
            matches!(
                tracked.state,
                RawMemoryBackupState::Running | RawMemoryBackupState::Unknown
            )
        })
    }

    fn prepare_raw(&mut self) -> Option<WorkerCommand> {
        let instance = self.instance?;
        if !self.ready
            || self.pending.is_some()
            || self.waiting_for_drop
            || self.raw.tracked.is_some()
            || self.guard.load(Ordering::Acquire) == u64::MAX
        {
            return None;
        }
        self.raw.plan = None;
        let operation_id = Uuid::new_v4();
        let requested_at = Instant::now();
        self.raw.feedback = Some("正在准备完整原始备份，请稍候。");
        self.next_request(
            ControlCommand::PrepareRawMemoryBackup {
                instance_id: instance,
                operation_id,
            },
            |id| Pending::RawPrepare {
                id,
                instance,
                operation_id,
                requested_at,
            },
        )
    }

    fn confirm_raw(&mut self) -> Option<WorkerCommand> {
        let plan = self.raw.plan.take()?;
        if !self.ready
            || self.pending.is_some()
            || self.waiting_for_drop
            || self.raw.tracked.is_some()
            || self.instance != Some(plan.instance)
            || Instant::now() >= plan.expires_at
        {
            self.raw.feedback = Some("准备信息已失效，请重新准备。");
            return None;
        }
        let operation_id = plan.operation_id;
        let command = self.next_request(
            ControlCommand::StartRawMemoryBackup {
                instance_id: plan.instance,
                operation_id,
                plan_id: plan.plan_id,
            },
            |id| Pending::RawStart { id, operation_id },
        )?;
        // Once queued, delivery is uncertain until the service confirms the outcome.
        self.raw.tracked = Some(Tracked {
            instance: plan.instance,
            operation_id,
            output: plan.output,
            state: RawMemoryBackupState::Unknown,
        });
        self.raw.feedback = None;
        Some(command)
    }

    fn query_raw(&mut self) -> Option<WorkerCommand> {
        let tracked = self.raw.tracked.as_ref()?;
        if !self.ready
            || self.pending.is_some()
            || self.instance != Some(tracked.instance)
            || !matches!(
                tracked.state,
                RawMemoryBackupState::Running | RawMemoryBackupState::Unknown
            )
        {
            return None;
        }
        let instance_id = tracked.instance;
        let operation_id = tracked.operation_id;
        self.next_request(
            ControlCommand::RawMemoryBackupStatus {
                instance_id,
                operation_id,
            },
            |id| Pending::RawQuery { id, operation_id },
        )
    }

    pub(super) fn receive_raw(
        &mut self,
        pending: Pending,
        reply: Result<ControlPayload, AppError>,
    ) -> Option<WorkerCommand> {
        match (pending, reply) {
            (
                Pending::RawPrepare {
                    instance,
                    operation_id,
                    requested_at,
                    ..
                },
                Ok(ControlPayload::RawMemoryBackupPlan {
                    instance_id,
                    operation_id: received,
                    plan_id,
                    expires_in_seconds,
                    output,
                }),
            ) if self.ready
                && self.instance == Some(instance)
                && instance_id == instance
                && received == operation_id
                && plan_id.get_version_num() == 4
                && expires_in_seconds > 0
                && valid_output(output.as_str()) =>
            {
                self.raw.plan = Some(Plan {
                    instance,
                    operation_id,
                    plan_id,
                    output: output.as_str().to_owned(),
                    expires_at: requested_at + Duration::from_secs(expires_in_seconds.min(300)),
                });
                self.raw.feedback = None;
            }
            (Pending::RawPrepare { .. }, _) => {
                self.raw.plan = None;
                self.raw.feedback = Some("无法准备备份，请检查服务后重新准备。");
            }
            (
                Pending::RawStart { operation_id, .. } | Pending::RawQuery { operation_id, .. },
                Ok(ControlPayload::RawMemoryBackupStatus(status)),
            ) => self.accept_raw_status(operation_id, status),
            (Pending::RawStart { .. } | Pending::RawQuery { .. }, _) => {
                if let Some(tracked) = &mut self.raw.tracked {
                    tracked.state = RawMemoryBackupState::Unknown;
                }
                self.raw.feedback = Some("结果尚未确认；请保留编号并查询，不会自动重试。");
            }
            _ => {}
        }
        None
    }

    fn accept_raw_status(&mut self, operation_id: Uuid, status: RawMemoryBackupStatus) {
        let Some(tracked) = &mut self.raw.tracked else {
            return;
        };
        let identity_matches = operation_id == tracked.operation_id
            && status.operation_id == tracked.operation_id
            && status.instance_id == tracked.instance;
        let output_matches = status
            .output
            .as_ref()
            .is_some_and(|path| path.as_str() == tracked.output);
        let unknown_without_output =
            status.output.is_none() && matches!(status.state, RawMemoryBackupState::Unknown);
        if identity_matches && (output_matches || unknown_without_output) {
            tracked.state = status.state;
            self.raw.feedback = None;
        } else {
            tracked.state = RawMemoryBackupState::Unknown;
            self.raw.feedback = Some("状态与原操作不匹配，请保留编号并核对目标文件。");
        }
    }

    pub(super) fn show_raw(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        let mut command = None;
        egui::CollapsingHeader::new("备份完整原始记忆文件")
            .id_salt("raw_memory_backup")
            .show(ui, |ui| {
                ui.weak("复制当前完整记忆文件的原始内容，包括同一文件中所有角色和未识别字段；其中可能含私人资料，请妥善保管。");
                ui.weak("副本保存在原文件所在目录，不会覆盖已有文件。其他程序直接修改文件时，无法保证副本是完整快照。");
                if self.raw.tracked.is_some() {
                    command = self.show_raw_tracking(ui);
                } else {
                    let may_prepare = self.ready && self.pending.is_none() && !self.waiting_for_drop;
                    if self.raw.plan.is_none() && ui.add_enabled(may_prepare, egui::Button::new("准备完整原始备份")).clicked() {
                        command = self.prepare_raw();
                    }
                    if let Some(plan) = &self.raw.plan {
                        ui.group(|ui| {
                            ui.strong("请核对备份范围与目标文件");
                            ui.label("范围：整份记忆文件，包含所有角色及私人资料。");
                            ui.label(format!("目标文件：{}", plan.output));
                            ui.label(format!("操作编号：{}", plan.operation_id));
                            ui.weak("确认后复制当时的文件内容；请确保相关服务及其他程序不会直接改写它。");
                        });
                        let current = self.ready && self.pending.is_none() && self.instance == Some(plan.instance)
                            && Instant::now() < plan.expires_at;
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(current, egui::Button::new("确认生成完整原始备份")).clicked() {
                                command = self.confirm_raw();
                            }
                            if ui.button("取消准备").clicked() { self.raw.plan = None; }
                        });
                        if !current { ui.weak("准备信息可能已失效，请取消后重新准备。"); }
                    }
                }
                if let Some(feedback) = self.raw.feedback { ui.label(feedback); }
            });
        command
    }

    fn show_raw_tracking(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        let tracked = self.raw.tracked.as_ref()?;
        ui.label(format!("操作编号：{}", tracked.operation_id));
        ui.label(format!("目标文件：{}", tracked.output));
        if ui.button("复制目标路径").clicked() {
            ui.ctx().copy_text(tracked.output.clone());
        }
        match tracked.state {
            RawMemoryBackupState::Running => ui.label("备份进行中，请查询状态。"),
            RawMemoryBackupState::Written {
                temporary_retained: false,
            } => ui.label("完整原始备份已生成，请妥善保管。"),
            RawMemoryBackupState::Written {
                temporary_retained: true,
            } => ui.label("备份已生成；目录可能保留临时文件，请核对并妥善处理。"),
            RawMemoryBackupState::CapacityReached => {
                ui.label("本次服务的备份操作记录已满，本次未开始；请核对后重启服务。")
            }
            RawMemoryBackupState::Unknown => {
                ui.label("结果尚未确认；文件可能已生成，请先核对，不会自动重试。")
            }
        };
        if self.instance.is_none() {
            ui.weak("当前未连接，请保留路径；恢复同一服务后可查询状态。");
        } else if self.instance != Some(tracked.instance) {
            ui.weak("服务实例已变化，无法查询原操作；请保留路径并核对目标文件。");
        } else if !self.ready {
            ui.weak("当前暂不能查询，请稍后在同一服务中重试。");
        }
        let can_query = self.ready
            && self.pending.is_none()
            && self.instance == Some(tracked.instance)
            && matches!(
                tracked.state,
                RawMemoryBackupState::Running | RawMemoryBackupState::Unknown
            );
        let can_dismiss =
            self.pending.is_none() && !matches!(tracked.state, RawMemoryBackupState::Running);
        let mut query = false;
        let mut dismiss = false;
        ui.horizontal_wrapped(|ui| {
            query = ui
                .add_enabled(can_query, egui::Button::new("查询完整备份状态"))
                .clicked();
            dismiss = ui
                .add_enabled(can_dismiss, egui::Button::new("已核对，关闭备份记录"))
                .clicked();
        });
        if dismiss {
            self.raw.tracked = None;
        }
        if query {
            return self.query_raw();
        }
        None
    }
}

fn valid_output(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 2048
        && !path.chars().any(char::is_control)
        && Path::new(path).is_absolute()
}
