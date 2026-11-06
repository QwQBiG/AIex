use super::*;
use ai_ex_control::{
    LegacyCandidateState, LegacyCandidateStatus, MemoryMergeConflict, MemoryMergeSummary,
    MergeCandidatePreviewResult,
};
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "backup_merge_panel_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct MergePanel {
    base: String,
    incoming: String,
    output_directory: String,
    generation: u64,
    preview: Option<Preview>,
    confirmation: Option<Confirmation>,
    tracked: Option<Tracked>,
    feedback: Option<&'static str>,
}

struct Preview {
    context: Context,
    generation: u64,
    preview_id: Uuid,
    summary: MemoryMergeSummary,
    expires_at: Instant,
}

struct Confirmation {
    context: Context,
    generation: u64,
    preview_id: Uuid,
    operation_id: Uuid,
    base: String,
    incoming: String,
    output_directory: String,
    output: String,
}

struct Tracked {
    instance: Uuid,
    profile: String,
    operation_id: Uuid,
    output: String,
    state: LegacyCandidateState,
}

impl BackupInspectionPanel {
    pub(super) fn invalidate_merge(&mut self) {
        self.merge.preview = None;
        self.merge.confirmation = None;
        if let Some(tracked) = &mut self.merge.tracked
            && matches!(tracked.state, LegacyCandidateState::Running)
        {
            tracked.state = LegacyCandidateState::Unknown;
        }
    }

    pub(super) fn merge_tracking(&self) -> bool {
        self.merge.tracked.is_some()
    }

    pub(super) fn clear_merge_sources(&mut self) {
        self.merge.base.clear();
        self.merge.incoming.clear();
        self.merge.output_directory.clear();
        self.merge.generation = self.merge.generation.saturating_add(1);
        self.merge.feedback = None;
    }

    pub(super) fn merge_unresolved(&self) -> bool {
        self.merge.tracked.as_ref().is_some_and(|tracked| {
            matches!(
                tracked.state,
                LegacyCandidateState::Running | LegacyCandidateState::Unknown
            )
        })
    }

    fn merge_sources_edited(&mut self) {
        self.invalidate_merge();
        self.revoke_queued();
        self.pending = None;
        self.merge.generation = self.merge.generation.saturating_add(1);
        self.merge.feedback = None;
    }

    fn prepare_merge(&mut self) -> Option<WorkerCommand> {
        let base = selected_path(&self.merge.base);
        let incoming = selected_path(&self.merge.incoming);
        if !self.ready
            || self.waiting_for_drop
            || self.pending.is_some()
            || self.merge.tracked.is_some()
            || self.candidate_tracking()
            || self.merge.generation == u64::MAX
            || self.guard.load(Ordering::Acquire) == u64::MAX
        {
            self.merge.feedback = Some("请等待当前操作完成，或先核对已有候选记录。");
            return None;
        }
        if !valid_path(base, 1024) || !valid_path(incoming, 1024) || base == incoming {
            self.merge.feedback = Some("请填写两个不同的备份绝对路径，并核对当前角色。");
            return None;
        }
        self.invalidate_candidate();
        self.merge.preview = None;
        self.merge.confirmation = None;
        self.merge.feedback = Some("正在确认当前角色…");
        let generation = self.merge.generation;
        self.next_request(ControlCommand::MemoryInspectionContext, |id| {
            Pending::MergeContext { id, generation }
        })
    }

    pub(super) fn receive_merge(
        &mut self,
        pending: Pending,
        reply: Result<ControlPayload, AppError>,
    ) -> Option<WorkerCommand> {
        match (pending, reply) {
            (
                Pending::MergeContext { generation, .. },
                Ok(ControlPayload::MemoryInspectionContext {
                    instance_id,
                    profile_id,
                    persona_epoch,
                }),
            ) if self.ready
                && self.instance == Some(instance_id)
                && self.profile == profile_id
                && self.merge.generation == generation =>
            {
                let context = Context {
                    instance: instance_id,
                    profile: profile_id,
                    epoch: persona_epoch,
                };
                let command = ControlCommand::PrepareMergeCandidate {
                    instance_id,
                    profile_id: context.profile.clone(),
                    persona_epoch,
                    base_path: SensitivePath::new(selected_path(&self.merge.base)),
                    incoming_path: SensitivePath::new(selected_path(&self.merge.incoming)),
                };
                self.merge.feedback = Some("正在检查两份备份…");
                self.next_request(command, |id| Pending::MergePreview {
                    id,
                    generation,
                    context,
                })
            }
            (
                Pending::MergePreview {
                    generation,
                    context,
                    ..
                },
                Ok(ControlPayload::MergeCandidatePreview {
                    instance_id,
                    profile_id,
                    persona_epoch,
                    preview_id,
                    expires_in_seconds,
                    result,
                }),
            ) if self.ready
                && self.instance == Some(instance_id)
                && self.profile == profile_id
                && self.merge.generation == generation
                && context.instance == instance_id
                && context.profile == profile_id
                && context.epoch == persona_epoch =>
            {
                match (preview_id, expires_in_seconds, result) {
                    (Some(preview_id), seconds, MergeCandidatePreviewResult::Ready { summary })
                        if preview_id.get_version_num() == 4
                            && seconds > 0
                            && valid_summary(&summary) =>
                    {
                        self.merge.preview = Some(Preview {
                            context,
                            generation,
                            preview_id,
                            summary,
                            expires_at: Instant::now()
                                + Duration::from_secs(seconds.min(300).saturating_sub(2)),
                        });
                        self.merge.feedback = Some("检查完成；请核对内容并审阅候选文件。");
                    }
                    (None, 0, MergeCandidatePreviewResult::Conflict { reason }) => {
                        self.merge.feedback = Some(conflict_message(reason));
                    }
                    _ => {
                        self.merge.feedback = Some("检查结果无效，请重新准备。");
                    }
                }
                None
            }
            (
                Pending::MergeStart { operation_id, .. } | Pending::MergeQuery { operation_id, .. },
                Ok(ControlPayload::MergeCandidateStatus(status)),
            ) => {
                self.accept_merge_status(operation_id, status);
                None
            }
            (Pending::MergeStart { .. } | Pending::MergeQuery { .. }, _) => {
                if let Some(tracked) = &mut self.merge.tracked {
                    tracked.state = LegacyCandidateState::Unknown;
                }
                self.merge.feedback =
                    Some("结果尚未确认，请查询状态并核对目标文件；不会自动重试。");
                None
            }
            _ => {
                self.merge.preview = None;
                self.merge.feedback = Some("检查未完成，请核对角色、服务和两份备份。");
                None
            }
        }
    }

    fn accept_merge_status(&mut self, operation_id: Uuid, status: LegacyCandidateStatus) {
        let Some(tracked) = &mut self.merge.tracked else {
            return;
        };
        if tracked.operation_id != operation_id {
            return;
        }
        let known = status.operation_id == operation_id
            && status.instance_id == tracked.instance
            && status.profile_id.as_deref() == Some(tracked.profile.as_str())
            && status
                .output
                .as_ref()
                .is_some_and(|path| path.as_str() == tracked.output);
        let unknown = status.operation_id == operation_id
            && status.instance_id == tracked.instance
            && status.profile_id.is_none()
            && status.output.is_none()
            && matches!(status.state, LegacyCandidateState::Unknown);
        tracked.state = if known || unknown {
            status.state
        } else {
            LegacyCandidateState::Unknown
        };
        self.merge.feedback = None;
    }

    fn review_merge(&mut self) {
        let Some(preview) = &self.merge.preview else {
            return;
        };
        if Instant::now() >= preview.expires_at {
            self.merge.preview = None;
            self.merge.confirmation = None;
            self.merge.feedback = Some("检查结果已过期，请重新检查。");
            return;
        }
        let directory = selected_path(&self.merge.output_directory);
        if !self.ready
            || self.pending.is_some()
            || self.waiting_for_drop
            || self.instance != Some(preview.context.instance)
            || self.profile != preview.context.profile
            || self.merge.generation != preview.generation
            || !valid_path(directory, 900)
        {
            self.merge.feedback = Some("请填写服务所在电脑上的现有输出目录绝对路径。");
            return;
        }
        let operation_id = Uuid::new_v4();
        let output = Path::new(directory).join(format!("merge-candidate-{operation_id}.jsonl"));
        let Some(output) = output.to_str() else {
            self.merge.feedback = Some("输出路径无法表示，请选择其他目录。");
            return;
        };
        self.merge.confirmation = Some(Confirmation {
            context: preview.context.clone(),
            generation: preview.generation,
            preview_id: preview.preview_id,
            operation_id,
            base: selected_path(&self.merge.base).to_owned(),
            incoming: selected_path(&self.merge.incoming).to_owned(),
            output_directory: directory.to_owned(),
            output: output.to_owned(),
        });
        self.merge.feedback = None;
    }

    fn confirm_merge(&mut self) -> Option<WorkerCommand> {
        let confirmation = self.merge.confirmation.take()?;
        if !self.ready
            || self.pending.is_some()
            || self.waiting_for_drop
            || self.instance != Some(confirmation.context.instance)
            || self.profile != confirmation.context.profile
            || self.merge.generation != confirmation.generation
            || self.merge.tracked.is_some()
            || self.candidate_tracking()
            || self.guard.load(Ordering::Acquire) == u64::MAX
            || self.merge.preview.as_ref().is_none_or(|preview| {
                preview.preview_id != confirmation.preview_id
                    || Instant::now() >= preview.expires_at
            })
        {
            self.merge.feedback = Some("当前角色或备份已改变，请重新检查。");
            return None;
        }
        let operation_id = confirmation.operation_id;
        let command = ControlCommand::StartMergeCandidate {
            instance_id: confirmation.context.instance,
            profile_id: confirmation.context.profile.clone(),
            persona_epoch: confirmation.context.epoch,
            preview_id: confirmation.preview_id,
            operation_id,
            output_directory: SensitivePath::new(confirmation.output_directory),
        };
        let command = self.next_request(command, |id| Pending::MergeStart { id, operation_id })?;
        self.merge.tracked = Some(Tracked {
            instance: confirmation.context.instance,
            profile: confirmation.context.profile,
            operation_id,
            output: confirmation.output,
            state: LegacyCandidateState::Unknown,
        });
        self.merge.preview = None;
        self.merge.feedback = Some("正在提交；如连接中断，请先查询并核对目标文件。");
        Some(command)
    }

    fn query_merge(&mut self) -> Option<WorkerCommand> {
        let tracked = self.merge.tracked.as_ref()?;
        if !self.ready
            || self.pending.is_some()
            || self.instance != Some(tracked.instance)
            || self.guard.load(Ordering::Acquire) == u64::MAX
            || !matches!(
                tracked.state,
                LegacyCandidateState::Running | LegacyCandidateState::Unknown
            )
        {
            return None;
        }
        let operation_id = tracked.operation_id;
        let instance_id = tracked.instance;
        self.next_request(
            ControlCommand::MergeCandidateStatus {
                instance_id,
                operation_id,
            },
            |id| Pending::MergeQuery { id, operation_id },
        )
    }

    pub(super) fn show_merge(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        let mut command = None;
        egui::CollapsingHeader::new("合并两份备份为独立候选")
            .id_salt("merge_memory_candidate")
            .show(ui, |ui| {
                ui.weak("请使用已关闭且不再变化的新版 JSONL 备份副本。检查不会修改源文件或现用记忆。");
                ui.weak("生成时核对与预览相同的字节；如果其他程序同时改写文件，读取不能保证是原子快照。");
                ui.weak("仅生成独立文件，不会启用或合并到现用记忆；后续存储重写可能丢失未知字段。");
                ui.weak("只显示首个发现的冲突；同名向量空间不能单凭名称或维度认定兼容。网络路径会访问相应位置。");
                if self.merge.tracked.is_some() {
                    command = self.show_merge_tracking(ui);
                } else {
                    let editable = self.pending.is_none() && !self.waiting_for_drop;
                    let mut changed = false;
                    ui.add_enabled_ui(editable, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("第一份备份");
                            if ui.add_enabled(!self.path.is_empty(), egui::Button::new("使用上方备份")).clicked() {
                                self.merge.base = self.path.clone();
                                changed = true;
                            }
                        });
                        changed |= ui.add(egui::TextEdit::singleline(&mut self.merge.base)
                            .hint_text("服务所在电脑上的绝对路径").desired_width(360.0).char_limit(1024)).changed();
                        ui.horizontal(|ui| {
                            ui.label("第二份备份");
                            if ui.add_enabled(!self.path.is_empty(), egui::Button::new("使用上方备份")).clicked() {
                                self.merge.incoming = self.path.clone();
                                changed = true;
                            }
                        });
                        changed |= ui.add(egui::TextEdit::singleline(&mut self.merge.incoming)
                            .hint_text("服务所在电脑上的绝对路径").desired_width(360.0).char_limit(1024)).changed();
                    });
                    if changed { self.merge_sources_edited(); }
                    if self.candidate_tracking() {
                        ui.weak("请先核对并关闭上一次候选操作记录。");
                    }
                    let can_prepare = self.ready && editable && !self.candidate_tracking()
                        && !self.merge.base.is_empty() && !self.merge.incoming.is_empty();
                    if ui.add_enabled(can_prepare, egui::Button::new("检查两份备份")).clicked() {
                        command = self.prepare_merge();
                    }
                    if let Some(preview) = &self.merge.preview {
                        let summary = &preview.summary;
                        ui.label(format!("第一份记忆：{} · 第二份记忆：{}", summary.base_memory_records, summary.incoming_memory_records));
                        ui.label(format!("实体候选：{} · 来源关联：{}（非活跃：{}）", summary.entity_candidates, summary.source_associations, summary.inactive_source_associations));
                        ui.label(format!("不同向量空间：{}", summary.distinct_embedding_spaces));
                        if summary.base_empty || summary.incoming_empty {
                            ui.weak("存在空备份；空文件不含角色标签，无法验证其归属，请自行核对。");
                        }
                        ui.weak("不同名向量空间也不能证明语义兼容。");
                        if ui.add(egui::TextEdit::singleline(&mut self.merge.output_directory)
                            .hint_text("服务所在电脑上的现有输出目录").desired_width(360.0).char_limit(900)).changed() {
                            self.merge.confirmation = None;
                        }
                        if ui.add_enabled(self.ready && self.pending.is_none(), egui::Button::new("审阅合并候选")).clicked() {
                            self.review_merge();
                        }
                    }
                    if let Some(confirmation) = &self.merge.confirmation {
                        ui.group(|ui| {
                            ui.strong("请核对后确认");
                            ui.label(format!("角色：{}", confirmation.context.profile));
                            ui.label(format!("第一份备份：{}", confirmation.base));
                            ui.label(format!("第二份备份：{}", confirmation.incoming));
                            ui.label(format!("目标文件：{}", confirmation.output));
                            ui.weak("仅写入新的候选文件，不覆盖、恢复或启用现有记忆。");
                        });
                        ui.horizontal(|ui| {
                            if ui.button("确认生成合并候选").clicked() { command = self.confirm_merge(); }
                            if ui.button("取消审阅").clicked() { self.merge.confirmation = None; }
                        });
                    }
                }
                if let Some(feedback) = self.merge.feedback { ui.label(feedback); }
            });
        command
    }

    fn show_merge_tracking(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        let tracked = self.merge.tracked.as_ref()?;
        ui.label(format!(
            "原角色：{} · 操作编号：{}",
            tracked.profile, tracked.operation_id
        ));
        ui.label(format!("目标文件：{}", tracked.output));
        match tracked.state {
            LegacyCandidateState::Running => ui.label("操作进行中，请查询确认。"),
            LegacyCandidateState::Written {
                temporary_retained: false,
            } => ui.label("合并候选已生成，尚未启用。"),
            LegacyCandidateState::Written {
                temporary_retained: true,
            } => ui.label("合并候选已生成；目录可能保留临时文件，请核对。"),
            LegacyCandidateState::Rejected => {
                ui.label("检查未通过，没有生成候选；请重新检查备份。")
            }
            LegacyCandidateState::CapacityReached => {
                ui.label("本次服务的候选操作记录已满，本次未开始；请核对后重启服务。")
            }
            LegacyCandidateState::Unknown => {
                ui.label("结果尚未确认；文件可能已生成，请先核对，不会自动重试。")
            }
        };
        if self.instance != Some(tracked.instance) {
            ui.weak("服务实例已变化，无法查询原操作；请核对目标文件。");
        }
        let mut query = false;
        let mut dismiss = false;
        ui.horizontal_wrapped(|ui| {
            query = ui
                .add_enabled(
                    self.ready
                        && self.pending.is_none()
                        && self.instance == Some(tracked.instance)
                        && matches!(
                            tracked.state,
                            LegacyCandidateState::Running | LegacyCandidateState::Unknown
                        ),
                    egui::Button::new("查询合并候选状态"),
                )
                .clicked();
            dismiss = ui
                .add_enabled(
                    self.pending.is_none()
                        && !matches!(tracked.state, LegacyCandidateState::Running),
                    egui::Button::new("已核对，关闭合并记录"),
                )
                .clicked();
        });
        if dismiss {
            self.merge.tracked = None;
        }
        if query {
            return self.query_merge();
        }
        None
    }
}

fn valid_path(path: &str, limit: usize) -> bool {
    !path.is_empty()
        && Path::new(path).is_absolute()
        && path.chars().count() <= limit
        && !path.chars().any(char::is_control)
}

fn valid_summary(summary: &MemoryMergeSummary) -> bool {
    summary.base_memory_records <= 50_000
        && summary.incoming_memory_records <= 50_000
        && summary.base_memory_records + summary.incoming_memory_records <= 50_000
        && summary.entity_candidates <= 50_000
        && summary.source_associations <= 50_000
        && summary.base_memory_records
            + summary.incoming_memory_records
            + summary.entity_candidates
            + summary.source_associations
            <= 50_000
        && summary.inactive_source_associations <= summary.source_associations
        && summary.distinct_embedding_spaces <= 16
        && (!summary.base_empty || summary.base_memory_records == 0)
        && (!summary.incoming_empty || summary.incoming_memory_records == 0)
        && !(summary.base_empty && summary.incoming_empty)
}

fn conflict_message(reason: MemoryMergeConflict) -> &'static str {
    match reason {
        MemoryMergeConflict::InvalidData => "备份格式无效或包含不支持的记录。",
        MemoryMergeConflict::ProfileOrVersion => "备份版本或角色标签不匹配。",
        MemoryMergeConflict::LimitExceeded => "备份超过支持的大小或记录数量。",
        MemoryMergeConflict::MemoryIdentityOverlap => "两份备份有重复的记忆编号。",
        MemoryMergeConflict::EntityIdentityOverlap => "两份备份有重复的实体编号。",
        MemoryMergeConflict::SourceAssociationOverlap => "两份备份有重复的来源关联。",
        MemoryMergeConflict::UnstableSources => "备份含缺失或不稳定的有效来源，不能直接合并。",
        MemoryMergeConflict::EmbeddingSpaceOverlap => "两份备份包含同名向量空间，无法确认兼容。",
        MemoryMergeConflict::EmbeddingDimensionsConflict => "备份中的向量空间维度不一致。",
        MemoryMergeConflict::EmptyInputs => "两份备份都是空文件，无法生成候选。",
    }
}
