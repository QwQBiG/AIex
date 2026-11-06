use ai_ex_control::{
    ControlCommand, ControlPayload, LegacyMemoryBackupInspection, MemoryBackupInspection,
    SensitivePath,
};
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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum BackupFormat {
    #[default]
    Current,
    Legacy,
}

enum InspectionResult {
    Current(MemoryBackupInspection),
    Legacy(LegacyMemoryBackupInspection),
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
    format: BackupFormat,
    path_generation: u64,
    waiting_for_drop: bool,
    drop_message: Option<&'static str>,
    guard: Arc<AtomicU64>,
    next_id: u64,
    pending: Option<Pending>,
    result: Option<InspectionResult>,
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
            self.cancel_drop();
            self.drop_message = None;
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
        self.cancel_drop();
        self.drop_message = None;
        self.revoke_queued();
        self.pending = None;
        self.result = None;
        self.feedback = None;
        self.path_generation = self.path_generation.saturating_add(1);
    }

    fn set_format(&mut self, format: BackupFormat) {
        if self.format != format {
            self.format = format;
            self.path_edited();
        }
    }

    pub fn cancel_drop(&mut self) {
        self.waiting_for_drop = false;
    }

    pub fn arm_drop(&mut self) {
        if self.ready {
            self.revoke_queued();
            self.pending = None;
            self.result = None;
            self.feedback = None;
            self.waiting_for_drop = true;
            self.drop_message = None;
        }
    }

    pub fn waiting_for_drop(&self) -> bool {
        self.waiting_for_drop
    }

    pub fn show_drop_banner(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("等待拖入记忆备份");
            ui.label("拖入一个文件，仅填写路径；检查需另外点击。");
            if ui.button("取消拖入").clicked() {
                self.cancel_drop();
            }
        });
    }

    pub fn receive_drop(&mut self, ui: &mut egui::Ui) {
        if !self.ready || !self.waiting_for_drop {
            return;
        }
        let files = ui.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        if files.is_empty() {
            return;
        }
        self.cancel_drop();
        let path = (files.len() == 1)
            .then(|| files[0].path.as_ref())
            .flatten()
            .and_then(|path| path.to_str())
            .filter(|path| {
                !path.is_empty()
                    && path.chars().count() <= 1024
                    && !path.chars().any(char::is_control)
            });
        if let Some(path) = path {
            self.path = path.to_owned();
            self.path_edited();
            self.drop_message = Some("已填入路径，请核对后点击“检查备份”。");
        } else {
            self.drop_message = Some("请拖入一个带可用路径的文件，或手动填写路径。");
        }
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
            || self.waiting_for_drop
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
                let command = match self.format {
                    BackupFormat::Current => ControlCommand::InspectMemoryBackup {
                        instance_id,
                        profile_id: context.profile.clone(),
                        persona_epoch,
                        path: SensitivePath::new(selected_path(&self.path)),
                    },
                    BackupFormat::Legacy => ControlCommand::InspectLegacyMemoryBackup {
                        instance_id,
                        profile_id: context.profile.clone(),
                        persona_epoch,
                        path: SensitivePath::new(selected_path(&self.path)),
                    },
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
                && self.format == BackupFormat::Current
                && valid_summary(&summary) =>
            {
                self.result = Some(InspectionResult::Current(summary));
                self.feedback = Some("检查完成。");
                None
            }
            (
                Pending::Inspection {
                    path_generation,
                    context,
                    ..
                },
                Ok(ControlPayload::LegacyMemoryBackupInspection {
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
                && self.format == BackupFormat::Legacy
                && valid_legacy_summary(&summary) =>
            {
                self.result = Some(InspectionResult::Legacy(summary));
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
                ui.weak("手动输入服务所在电脑上的备份文件绝对路径。只有点击检查才会读取文件。");
                #[cfg(windows)]
                ui.weak("支持粘贴 Windows“复制为路径”的结果，包括外层双引号。");
                ui.weak("如果输入 UNC 或其他网络路径，检查会访问相应网络位置。");
                let mut format = self.format;
                ui.horizontal_wrapped(|ui| {
                    ui.label("备份格式：");
                    ui.selectable_value(&mut format, BackupFormat::Current, "新版 JSONL");
                    ui.selectable_value(&mut format, BackupFormat::Legacy, "旧版 JSON / JSON.GZ");
                });
                self.set_format(format);
                match self.format {
                    BackupFormat::Current => ui.weak("仅核对角色标签和统计记录，无法证明文件来源或完整性。"),
                    BackupFormat::Legacy => ui.weak("仅核对 memories 的哈希和数量；未核验角色归属，无法证明文件来源、其他字段完整性或能否导入。"),
                };
                ui.weak("检查不会恢复或合并记忆，迁移需要另行操作。");
                ui.weak("等待中的检查会在发送前核对；已开始发送的检查可能继续。");
                if ui.add(egui::TextEdit::singleline(&mut self.path).hint_text("备份文件绝对路径").desired_width(360.0).char_limit(1024)).changed() {
                    self.path_edited();
                }
                command = self.show_actions(ui);
                if let Some(message) = self.drop_message { ui.weak(message); }
                if let Some(feedback) = self.feedback { ui.label(feedback); }
                if let Some(result) = &self.result {
                    match result {
                        InspectionResult::Current(result) => {
                            ui.label(format!("记忆记录：{} · 实体候选：{}", result.memory_records, result.entity_candidates));
                            ui.label(format!("来源关联：{}（其中非活跃：{}）", result.source_associations, result.inactive_source_associations));
                            ui.label(format!("包含向量的记录：{} · 向量空间数量：{}", result.embedded_records, result.distinct_embedding_spaces));
                        }
                        InspectionResult::Legacy(result) => {
                            ui.label(format!("记忆记录：{} · 备份时间与集合字段：{}", result.records, if result.legacy_metadata_fields_present { "已包含" } else { "未全部包含" }));
                            ui.label(format!("需要指定时区的记录：{} · 时间未分类：{}", result.local_timestamp_records, result.unclassified_timestamp_records));
                            ui.label(format!("包含向量的记录：{} · 向量未分类：{}", result.nonempty_embedding_records, result.unclassified_embedding_records));
                            ui.weak("含本地时间或向量的记录迁移时需分别指定时区或向量空间；未分类的记录需要进一步核对。");
                        }
                    }
                }
            });
        command
    }

    fn show_actions(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        if ui
            .add_enabled(
                self.ready && !self.waiting_for_drop,
                egui::Button::new("准备拖入备份"),
            )
            .clicked()
        {
            self.arm_drop();
        }
        if ui
            .add_enabled(
                self.ready
                    && !self.waiting_for_drop
                    && self.pending.is_none()
                    && !self.path.trim().is_empty(),
                egui::Button::new("检查备份"),
            )
            .clicked()
        {
            return self.start();
        }
        None
    }
}

fn selected_path(path: &str) -> &str {
    #[cfg(windows)]
    {
        path.strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(path)
    }
    #[cfg(not(windows))]
    {
        path
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

fn valid_legacy_summary(summary: &LegacyMemoryBackupInspection) -> bool {
    summary.records <= 50_000
        && summary.local_timestamp_records <= summary.records
        && summary.unclassified_timestamp_records
            <= summary.records - summary.local_timestamp_records
        && summary.nonempty_embedding_records <= summary.records
        && summary.unclassified_embedding_records
            <= summary.records - summary.nonempty_embedding_records
}
