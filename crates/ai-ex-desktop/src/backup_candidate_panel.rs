use super::*;
use ai_ex_control::{LegacyCandidateState, LegacyCandidateStatus};
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(test)]
#[path = "backup_candidate_panel_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct CandidatePanel {
    preview: Option<Preview>,
    output_directory: String,
    utc_offset: String,
    embedding_space: String,
    confirmation: Option<Confirmation>,
    tracked: Option<Tracked>,
    feedback: Option<&'static str>,
}

struct Preview {
    context: Context,
    path_generation: u64,
    preview_id: Uuid,
    summary: LegacyMemoryBackupInspection,
    expires_at: Instant,
}

struct Confirmation {
    context: Context,
    path_generation: u64,
    preview_id: Uuid,
    operation_id: Uuid,
    source: String,
    output_directory: String,
    output: String,
    utc_offset: Option<i16>,
    embedding_space: Option<String>,
}

struct Tracked {
    instance: Uuid,
    profile: String,
    operation_id: Uuid,
    output: String,
    state: LegacyCandidateState,
}

impl BackupInspectionPanel {
    pub(super) fn invalidate_candidate(&mut self) {
        self.candidate.preview = None;
        self.candidate.confirmation = None;
        if let Some(tracked) = &mut self.candidate.tracked
            && matches!(tracked.state, LegacyCandidateState::Running)
        {
            tracked.state = LegacyCandidateState::Unknown;
        }
    }

    pub fn candidate_unresolved(&self) -> bool {
        self.candidate.tracked.as_ref().is_some_and(|tracked| {
            matches!(
                tracked.state,
                LegacyCandidateState::Running | LegacyCandidateState::Unknown
            )
        })
    }

    fn prepare_candidate(&mut self) -> Option<WorkerCommand> {
        if !self.ready
            || self.waiting_for_drop
            || self.pending.is_some()
            || self.format != BackupFormat::Legacy
            || self.path.trim().is_empty()
            || self.candidate.tracked.is_some()
            || self.path_generation == u64::MAX
            || self.guard.load(Ordering::Acquire) == u64::MAX
        {
            return None;
        }
        self.candidate.preview = None;
        self.candidate.confirmation = None;
        self.candidate.feedback = Some("正在重新核对备份和当前角色…");
        let generation = self.path_generation;
        self.next_request(ControlCommand::MemoryInspectionContext, |id| {
            Pending::CandidateContext {
                id,
                path_generation: generation,
            }
        })
    }

    fn query_candidate(&mut self) -> Option<WorkerCommand> {
        let tracked = self.candidate.tracked.as_ref()?;
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
            ControlCommand::LegacyCandidateStatus {
                instance_id,
                operation_id,
            },
            |id| Pending::CandidateQuery { id, operation_id },
        )
    }

    pub(super) fn receive_candidate(
        &mut self,
        pending: Pending,
        reply: Result<ControlPayload, AppError>,
    ) -> Option<WorkerCommand> {
        match (pending, reply) {
            (
                Pending::CandidateContext {
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
                && self.path_generation == path_generation
                && self.format == BackupFormat::Legacy =>
            {
                let context = Context {
                    instance: instance_id,
                    profile: profile_id,
                    epoch: persona_epoch,
                };
                let command = ControlCommand::PrepareLegacyCandidate {
                    instance_id,
                    profile_id: context.profile.clone(),
                    persona_epoch,
                    path: SensitivePath::new(selected_path(&self.path)),
                };
                self.next_request(command, |id| Pending::CandidatePreview {
                    id,
                    path_generation,
                    context,
                })
            }
            (
                Pending::CandidatePreview {
                    path_generation,
                    context,
                    ..
                },
                Ok(ControlPayload::LegacyCandidatePreview {
                    instance_id,
                    profile_id,
                    persona_epoch,
                    preview_id,
                    expires_in_seconds,
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
                && preview_id.get_version_num() == 4
                && expires_in_seconds > 0
                && valid_legacy_summary(&summary)
                && summary.records > 0 =>
            {
                self.result = Some(InspectionResult::Legacy(summary.clone()));
                self.candidate.preview = Some(Preview {
                    context,
                    path_generation,
                    preview_id,
                    summary,
                    expires_at: Instant::now()
                        + Duration::from_secs(expires_in_seconds.min(300).saturating_sub(2)),
                });
                self.candidate.feedback = Some("备份已核对；请填写参数并审阅候选文件。");
                None
            }
            (
                Pending::CandidateStart { operation_id, .. }
                | Pending::CandidateQuery { operation_id, .. },
                Ok(ControlPayload::LegacyCandidateStatus(status)),
            ) => {
                self.accept_candidate_status(operation_id, status);
                None
            }
            (Pending::CandidateStart { .. } | Pending::CandidateQuery { .. }, _) => {
                if let Some(tracked) = &mut self.candidate.tracked {
                    tracked.state = LegacyCandidateState::Unknown;
                }
                self.candidate.feedback =
                    Some("结果尚未确认，请查询状态并核对目标文件；不会自动重试。");
                None
            }
            _ => {
                self.candidate.preview = None;
                self.candidate.feedback = Some("准备未完成，请核对当前角色和源文件后重新检查。");
                None
            }
        }
    }

    fn accept_candidate_status(&mut self, operation_id: Uuid, status: LegacyCandidateStatus) {
        let Some(tracked) = &mut self.candidate.tracked else {
            return;
        };
        if tracked.operation_id != operation_id {
            return;
        }
        if status.operation_id != operation_id || status.instance_id != tracked.instance {
            tracked.state = LegacyCandidateState::Unknown;
            self.candidate.feedback = Some("状态响应不匹配，请核对操作编号和目标文件。");
            return;
        }
        let known = status.profile_id.as_deref() == Some(tracked.profile.as_str())
            && status
                .output
                .as_ref()
                .is_some_and(|path| path.as_str() == tracked.output);
        let unknown = status.profile_id.is_none()
            && status.output.is_none()
            && matches!(status.state, LegacyCandidateState::Unknown);
        tracked.state = if known || unknown {
            status.state
        } else {
            LegacyCandidateState::Unknown
        };
        self.candidate.feedback = None;
    }

    fn review_candidate(&mut self) {
        let Some(preview) = &self.candidate.preview else {
            return;
        };
        if Instant::now() >= preview.expires_at {
            self.candidate.preview = None;
            self.candidate.confirmation = None;
            self.candidate.feedback = Some("准备结果已过期，请重新准备后再审阅。");
            return;
        }
        let directory = selected_path(&self.candidate.output_directory);
        if !self.ready
            || self.pending.is_some()
            || self.waiting_for_drop
            || self.instance != Some(preview.context.instance)
            || self.profile != preview.context.profile
            || self.path_generation != preview.path_generation
            || !Path::new(directory).is_absolute()
            || directory.chars().count() > 900
            || directory.chars().any(char::is_control)
        {
            self.candidate.feedback =
                Some("请填写服务所在电脑上的输出目录绝对路径，并重新核对当前角色。");
            return;
        }
        let utc_offset = if self.candidate.utc_offset.is_empty() {
            None
        } else {
            match self.candidate.utc_offset.parse::<i16>() {
                Ok(value) if (-1439..=1439).contains(&value) => Some(value),
                _ => {
                    self.candidate.feedback = Some("UTC 偏移请填写 -1439 到 1439 的整数分钟。");
                    return;
                }
            }
        };
        let embedding_space = if self.candidate.embedding_space.is_empty() {
            None
        } else {
            Some(self.candidate.embedding_space.clone())
        };
        if (preview.summary.local_timestamp_records > 0 && utc_offset.is_none())
            || (preview.summary.nonempty_embedding_records > 0 && embedding_space.is_none())
            || embedding_space.as_ref().is_some_and(|value| {
                value.trim().is_empty()
                    || value.chars().count() > 256
                    || value.chars().any(char::is_control)
            })
        {
            self.candidate.feedback =
                Some("备份含本地时间或向量，请分别填写 UTC 偏移和对应向量空间。");
            return;
        }
        let operation_id = Uuid::new_v4();
        let output = Path::new(directory).join(format!("legacy-candidate-{operation_id}.jsonl"));
        let Some(output) = output.to_str() else {
            self.candidate.feedback = Some("输出路径无法表示，请选择其他目录。");
            return;
        };
        self.candidate.confirmation = Some(Confirmation {
            context: preview.context.clone(),
            path_generation: preview.path_generation,
            preview_id: preview.preview_id,
            operation_id,
            source: selected_path(&self.path).to_owned(),
            output_directory: directory.to_owned(),
            output: output.to_owned(),
            utc_offset,
            embedding_space,
        });
        self.candidate.feedback = None;
    }

    fn confirm_candidate(&mut self) -> Option<WorkerCommand> {
        let confirmation = self.candidate.confirmation.take()?;
        if !self.ready
            || self.pending.is_some()
            || self.waiting_for_drop
            || self.instance != Some(confirmation.context.instance)
            || self.profile != confirmation.context.profile
            || self.path_generation != confirmation.path_generation
            || self.candidate.tracked.is_some()
            || self.guard.load(Ordering::Acquire) == u64::MAX
            || self.candidate.preview.as_ref().is_none_or(|preview| {
                preview.preview_id != confirmation.preview_id
                    || Instant::now() >= preview.expires_at
            })
        {
            self.candidate.feedback = Some("当前角色或备份已改变，请重新准备。");
            return None;
        }
        let operation_id = confirmation.operation_id;
        let command = ControlCommand::StartLegacyCandidate {
            instance_id: confirmation.context.instance,
            profile_id: confirmation.context.profile.clone(),
            persona_epoch: confirmation.context.epoch,
            preview_id: confirmation.preview_id,
            operation_id,
            output_directory: SensitivePath::new(confirmation.output_directory),
            utc_offset: confirmation.utc_offset,
            embedding_space: confirmation.embedding_space,
        };
        let command =
            self.next_request(command, |id| Pending::CandidateStart { id, operation_id })?;
        self.candidate.tracked = Some(Tracked {
            instance: confirmation.context.instance,
            profile: confirmation.context.profile,
            operation_id,
            output: confirmation.output,
            state: LegacyCandidateState::Unknown,
        });
        self.candidate.preview = None;
        self.candidate.feedback = Some("正在提交；如连接中断，请先查询状态并核对目标文件。");
        Some(command)
    }

    pub(super) fn show_candidate(&mut self, ui: &mut egui::Ui) -> Option<WorkerCommand> {
        if self.format != BackupFormat::Legacy && self.candidate.tracked.is_none() {
            return None;
        }
        ui.separator();
        ui.strong("生成独立记忆候选");
        ui.weak("仅生成新文件，不合并、不启用。旧备份的角色归属需要自己确认。");
        if let Some(tracked) = &self.candidate.tracked {
            ui.label(format!(
                "原角色：{} · 操作编号：{}",
                tracked.profile, tracked.operation_id
            ));
            ui.label(format!("目标文件：{}", tracked.output));
            match tracked.state {
                LegacyCandidateState::Running => ui.label("操作进行中，请查询确认。"),
                LegacyCandidateState::Written {
                    temporary_retained: false,
                } => ui.label("候选文件已生成，尚未启用。"),
                LegacyCandidateState::Written {
                    temporary_retained: true,
                } => ui.label("候选文件已生成；输出目录可能保留临时文件，请核对。"),
                LegacyCandidateState::Rejected => {
                    ui.label("检查未通过，没有生成候选；请重新准备并核对参数。")
                }
                LegacyCandidateState::CapacityReached => ui.label(
                    "本次服务的候选操作记录已满，本次未开始。请先核对未完成操作，再重启服务。",
                ),
                LegacyCandidateState::Unknown => {
                    ui.label("结果尚未确认；文件可能已生成，请先核对，不会自动重试。")
                }
            };
            if self.instance != Some(tracked.instance) {
                ui.weak("服务实例已变化，无法查询原操作；请核对上面的目标文件。");
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
                        egui::Button::new("查询候选状态"),
                    )
                    .clicked();
                dismiss = ui
                    .add_enabled(
                        self.pending.is_none()
                            && !matches!(tracked.state, LegacyCandidateState::Running),
                        egui::Button::new("已核对，关闭本次记录"),
                    )
                    .clicked();
            });
            if dismiss {
                self.candidate.tracked = None;
            }
            if query {
                return self.query_candidate();
            }
        } else {
            let may_prepare = self.ready
                && self.pending.is_none()
                && !self.waiting_for_drop
                && matches!(self.result, Some(InspectionResult::Legacy(_)));
            if ui
                .add_enabled(may_prepare, egui::Button::new("准备生成候选"))
                .clicked()
            {
                return self.prepare_candidate();
            }
            if let Some(preview) = &self.candidate.preview {
                ui.label(format!(
                    "备份记录：{} · 本地时间：{} · 含向量：{}",
                    preview.summary.records,
                    preview.summary.local_timestamp_records,
                    preview.summary.nonempty_embedding_records
                ));
                ui.weak("准备结果短时有效；源文件或角色改变后需重新准备。固定偏移不能表示跨夏令时的变化。");
                ui.weak("向量空间必须与旧向量的实际模型一致；未知时请先核对，不要猜测。");
                let mut changed = false;
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.candidate.output_directory)
                            .hint_text("输出目录绝对路径")
                            .desired_width(360.0)
                            .char_limit(900),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.candidate.utc_offset)
                            .hint_text("UTC 偏移（整数分钟，例如中国标准时间 480）")
                            .desired_width(360.0)
                            .char_limit(5),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut self.candidate.embedding_space)
                            .hint_text("旧向量所属空间（无向量可留空）")
                            .desired_width(360.0)
                            .char_limit(256),
                    )
                    .changed();
                if changed {
                    self.candidate.confirmation = None;
                }
                ui.weak("网络目录会访问对应位置；目标文件所在目录须已存在。");
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new("审阅生成内容"))
                    .clicked()
                {
                    self.review_candidate();
                }
            }
            if let Some(confirmation) = &self.candidate.confirmation {
                ui.group(|ui| {
                    ui.strong("请核对后再生成");
                    ui.label(format!("原角色：{}", confirmation.context.profile));
                    ui.label(format!("源备份：{}", confirmation.source));
                    ui.label(format!("目标文件：{}", confirmation.output));
                    ui.label(format!(
                        "UTC 偏移：{}；向量空间：{}",
                        confirmation
                            .utc_offset
                            .map_or_else(|| "未指定".into(), |value| format!("{value} 分钟")),
                        confirmation.embedding_space.as_deref().unwrap_or("未指定")
                    ));
                    ui.weak("仅写入新的候选文件，不覆盖已有文件，也不恢复或启用记忆。");
                });
                let mut confirm = false;
                let mut cancel = false;
                ui.horizontal(|ui| {
                    confirm = ui.button("确认生成候选").clicked();
                    cancel = ui.button("取消").clicked();
                });
                if cancel {
                    self.candidate.confirmation = None;
                }
                if confirm {
                    return self.confirm_candidate();
                }
            }
        }
        if let Some(feedback) = self.candidate.feedback {
            ui.label(feedback);
        }
        None
    }
}
