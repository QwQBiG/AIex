use ai_ex_control::{ControlCommand, ControlPayload, MemoryBackupInspection, SensitivePath};
use ai_ex_domain::AppError;
use eframe::egui;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use uuid::Uuid;

use crate::worker::WorkerCommand;

#[cfg(test)]
#[path = "backup_inspection_panel_tests.rs"]
mod tests;

#[derive(Clone)]
struct Context {
    instance: Uuid,
    profile: String,
    epoch: u64,
}

enum Pending {
    Context {
        id: u64,
        path_generation: u64,
    },
    Inspection {
        id: u64,
        path_generation: u64,
        context: Context,
    },
}

#[derive(Default)]
pub struct BackupInspectionPanel {
    instance: Option<Uuid>,
    profile: String,
    persona_revision: u64,
    ready: bool,
    path: String,
    path_generation: u64,
    guard: Arc<AtomicU64>,
    next_id: u64,
    pending: Option<Pending>,
    result: Option<MemoryBackupInspection>,
    feedback: Option<&'static str>,
}

impl BackupInspectionPanel {
    pub fn synchronize(
        &mut self,
        instance: Option<Uuid>,
        profile: &str,
        persona_revision: u64,
        ready: bool,
    ) {
        let identity_changed = self.instance != instance || self.profile != profile;
        let persona_changed = self.persona_revision != persona_revision;
        let ready = ready && instance.is_some();
        if identity_changed || persona_changed || (self.ready && !ready) {
            self.revoke_queued();
            self.pending = None;
            self.result = None;
            self.feedback = None;
            if identity_changed {
                self.path.clear();
            }
        }
        self.instance = instance;
        self.profile = profile.to_owned();
        self.persona_revision = persona_revision;
        self.ready = ready;
    }

    fn path_edited(&mut self) {
        self.revoke_queued();
        self.pending = None;
        self.result = None;
        self.feedback = None;
        self.path_generation = self.path_generation.saturating_add(1);
    }

    fn revoke_queued(&self) {
        let _ = self
            .guard
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            });
    }

    fn next_request(
        &mut self,
        command: ControlCommand,
        pending: impl FnOnce(u64) -> Pending,
    ) -> Option<WorkerCommand> {
        let id = self.next_id.checked_add(1)?;
        self.next_id = id;
        self.pending = Some(pending(id));
        Some(WorkerCommand::BackupInspection {
            request_id: id,
            command,
            generation: self.guard.load(Ordering::Acquire),
            guard: Arc::clone(&self.guard),
        })
    }

    fn start(&mut self) -> Option<WorkerCommand> {
        if !self.ready
            || self.pending.is_some()
            || self.path.trim().is_empty()
            || self.path_generation == u64::MAX
            || self.guard.load(Ordering::Acquire) == u64::MAX
        {
            return None;
        }
        self.result = None;
        self.feedback = Some("正在确认当前角色…");
        let generation = self.path_generation;
        self.next_request(ControlCommand::MemoryInspectionContext, |id| {
            Pending::Context {
                id,
                path_generation: generation,
            }
        })
    }

    pub fn receive(
        &mut self,
        request_id: u64,
        reply: Result<ControlPayload, AppError>,
    ) -> Option<WorkerCommand> {
        let pending = self.pending.take()?;
        let id = match &pending {
            Pending::Context { id, .. } | Pending::Inspection { id, .. } => *id,
        };
        if id != request_id {
            self.pending = Some(pending);
            return None;
        }
        match (pending, reply) {
            (
                Pending::Context {
                    path_generation, ..
                },
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id,
                    profile_id,
                    persona_epoch,
                }),
            ) if self.ready
                && self.instance == Some(instance_id)
                && self.profile == profile_id
                && self.path_generation == path_generation =>
            {
                let context = Context {
                    instance: instance_id,
                    profile: profile_id,
                    epoch: persona_epoch,
                };
                let command = ControlCommand::InspectMemoryBackup {
                    instance_id,
                    profile_id: context.profile.clone(),
                    persona_epoch,
                    path: SensitivePath::new(self.path.clone()),
                };
                self.feedback = Some("正在检查备份…");
                self.next_request(command, |id| Pending::Inspection {
                    id,
                    path_generation,
                    context,
                })
            }
            (
                Pending::Inspection {
                    path_generation,
                    context,
                    ..
                },
                Ok(ControlPayload::MemoryBackupInspection {
                    instance_id,
                    profile_id,
                    persona_epoch,
                    summary,
                }),
            ) if self.ready
                && self.instance == Some(instance_id)
                && self.profile == profile_id
                && self.path_generation == path_generation
                && context.instance == instance_id
                && context.profile == profile_id
                && context.epoch == persona_epoch
                && valid_summary(&summary) =>
            {
                self.result = Some(summary);
                self.feedback = Some("检查完成。");
                None
            }
            _ => {
                self.result = None;
                self.feedback = Some("检查未完成，请确认角色、服务及文件后重试；不会自动重试。");
                None
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        let mut command = None;
        egui::CollapsingHeader::new("检查已有记忆备份")
            .id_salt("inspect_memory_backup")
            .show(ui, |ui| {
                ui.weak("手动输入服务所在电脑上的备份文件绝对路径，仅支持新版 JSONL。只有点击检查才会读取文件。");
                ui.weak("如果输入 UNC 或其他网络路径，检查会访问相应网络位置。");
                ui.weak("仅核对角色标签和统计记录，不会恢复或合并记忆，也无法证明文件来源或完整性。旧版压缩备份请使用迁移工具检查。");
                ui.weak("等待中的检查会在发送前核对；已开始发送的检查可能继续。");
                if ui.add(egui::TextEdit::singleline(&mut self.path).hint_text("备份文件绝对路径").desired_width(360.0).char_limit(1024)).changed() {
                    self.path_edited();
                }
                if ui.add_enabled(self.ready && self.pending.is_none() && !self.path.trim().is_empty(), egui::Button::new("检查备份")).clicked() {
                    command = self.start();
                }
                if let Some(feedback) = self.feedback { ui.label(feedback); }
                if let Some(result) = &self.result {
                    ui.label(format!("记忆记录：{} · 实体候选：{}", result.memory_records, result.entity_candidates));
                    ui.label(format!("来源关联：{}（其中非活跃：{}）", result.source_associations, result.inactive_source_associations));
                    ui.label(format!("包含向量的记录：{} · 向量空间数量：{}", result.embedded_records, result.distinct_embedding_spaces));
                }
            });
        command
    }
}

fn valid_summary(summary: &MemoryBackupInspection) -> bool {
    summary.memory_records <= 50_000
        && summary.entity_candidates <= 50_000
        && summary.source_associations <= 50_000
        && summary.inactive_source_associations <= summary.source_associations
        && summary.embedded_records <= summary.memory_records
        && summary.distinct_embedding_spaces <= 16
        && summary
            .matching_embeddings
            .is_none_or(|count| count <= summary.embedded_records)
}
