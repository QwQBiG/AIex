use super::*;
use ai_ex_domain::MemoryBackupStatus;
use eframe::egui;

#[cfg(test)]
#[path = "memory_panel_backup_tests.rs"]
mod tests;

pub(super) struct TrackedBackup {
    id: uuid::Uuid,
    instance: uuid::Uuid,
    pub(super) state: MemoryBackupState,
    pub(super) acknowledged: bool,
}

impl MemoryPanel {
    pub fn synchronize_backup_instance(&mut self, instance: Option<uuid::Uuid>) {
        if self.backup_instance == instance {
            return;
        }
        self.backup_request_interrupted();
        self.backup_instance = instance;
        for backup in self.backups.values_mut().flatten() {
            if Some(backup.instance) != instance
                && matches!(backup.state, MemoryBackupState::Running)
            {
                backup.state = MemoryBackupState::Unknown;
            }
        }
    }

    pub(super) fn backup_request_interrupted(&mut self) {
        let request = self
            .pending
            .as_ref()
            .and_then(|pending| match &pending.request {
                MemoryRequest::Backup { .. } | MemoryRequest::BackupStatus { .. } => {
                    Some(pending.request.clone())
                }
                _ => None,
            });
        if let Some(request) = request {
            self.backup_response_failed(&request);
            self.pending = None;
        }
    }

    pub(super) fn prepare_backup_request(&mut self, request: &MemoryRequest) -> bool {
        match request {
            MemoryRequest::Backup {
                profile_id,
                backup_id,
            } => {
                let Some(instance) = self.backup_instance else {
                    self.feedback = Some("服务身份尚未确认，请等待连接完成。".into());
                    return false;
                };
                if self
                    .backups
                    .values()
                    .flatten()
                    .any(|backup| backup.id == *backup_id)
                {
                    self.feedback = Some("该备份请求已经提交，请手动查询状态。".into());
                    return false;
                }
                self.backups
                    .entry(profile_id.clone())
                    .or_default()
                    .push(TrackedBackup {
                        id: *backup_id,
                        instance,
                        state: MemoryBackupState::Running,
                        acknowledged: false,
                    });
            }
            MemoryRequest::BackupStatus {
                profile_id,
                backup_id,
            } if !self.backups.get(profile_id).is_some_and(|backups| {
                backups.iter().any(|backup| {
                    backup.id == *backup_id && Some(backup.instance) == self.backup_instance
                })
            }) =>
            {
                self.feedback = Some("备份状态不属于当前服务，请查看保存目录核对。".into());
                return false;
            }
            _ => {}
        }
        true
    }

    pub(super) fn backup_response_failed(&mut self, request: &MemoryRequest) {
        if let MemoryRequest::Backup {
            profile_id,
            backup_id,
        }
        | MemoryRequest::BackupStatus {
            profile_id,
            backup_id,
        } = request
            && let Some(backup) = self
                .backups
                .get_mut(profile_id)
                .and_then(|backups| backups.iter_mut().find(|backup| backup.id == *backup_id))
        {
            backup.state = MemoryBackupState::Unknown;
        }
    }

    pub(super) fn accept_backup_status(
        &mut self,
        request: &MemoryRequest,
        status: MemoryBackupStatus,
    ) {
        let expected = match request {
            MemoryRequest::Backup {
                profile_id,
                backup_id,
            }
            | MemoryRequest::BackupStatus {
                profile_id,
                backup_id,
            } => Some((profile_id, backup_id)),
            _ => None,
        };
        if let Some((profile_id, backup_id)) = expected
            && status.profile_id == *profile_id
            && status.backup_id == *backup_id
            && profile_id == &self.profile_id
            && let Some(backup) = self
                .backups
                .get_mut(profile_id)
                .and_then(|backups| backups.iter_mut().find(|backup| backup.id == *backup_id))
            && Some(backup.instance) == self.backup_instance
        {
            backup.state = status.state;
            if matches!(backup.state, MemoryBackupState::Running) {
                backup.acknowledged = false;
            }
            self.feedback = None;
        } else {
            self.backup_response_failed(request);
            self.feedback = Some("备份响应不匹配，请手动查询状态。".into());
        }
    }

    pub(super) fn show_backup(
        &mut self,
        ui: &mut egui::Ui,
        ready: bool,
        enabled: bool,
        action: &mut Option<MemoryRequest>,
    ) {
        let pending = self.backups.get(&self.profile_id).is_some_and(|backups| {
            backups.iter().any(|backup| {
                !backup.acknowledged
                    && matches!(
                        backup.state,
                        MemoryBackupState::Unknown | MemoryBackupState::Running
                    )
            })
        });
        let title = if pending {
            "备份当前角色的记忆 · 有待核对"
        } else {
            "备份当前角色的记忆"
        };
        egui::CollapsingHeader::new(title)
            .id_salt("memory_backup_section")
            .show(ui, |ui| {
                self.show_backup_contents(ui, ready, enabled, action);
            });
    }

    fn show_backup_contents(
        &mut self,
        ui: &mut egui::Ui,
        ready: bool,
        enabled: bool,
        action: &mut Option<MemoryRequest>,
    ) {
        ui.weak("备份包含私人记忆及相关线索，会保存在服务记忆文件所在目录；请妥善保管。备份不会清空聊天。");
        ui.weak("备份编号仅在本次桌面会话保留；关闭应用或服务重启后，请检查原保存目录，无法保证再次查询状态。");
        let mut acknowledged = None;
        if let Some(backups) = self.backups.get(&self.profile_id) {
            for backup in backups.iter().rev() {
                ui.push_id(backup.id, |ui| {
                    ui.weak(format!("备份编号：{}", backup.id));
                    let same_instance = Some(backup.instance) == self.backup_instance;
                    match &backup.state {
                        MemoryBackupState::Unknown => {
                            ui.label("上一次备份结果未知，文件可能已经生成；请先核对保存目录。");
                        }
                        MemoryBackupState::Running => {
                            ui.label("备份进行中或等待确认；其他操作可能需要等待。");
                        }
                        MemoryBackupState::Completed {
                            path,
                            memory_count,
                            temporary_retained,
                        } => {
                            ui.label(format!("备份完成，共 {memory_count} 条记忆，保存位置："));
                            ui.add(egui::Label::new(path).selectable(true).wrap());
                            if *temporary_retained {
                                ui.label("备份文件可用，但临时链接未能清理，请检查保存目录。");
                            }
                        }
                        MemoryBackupState::Failed { message } => {
                            ui.label(format!("备份未完成：{message}"));
                        }
                    }
                    if !same_instance
                        && matches!(
                            backup.state,
                            MemoryBackupState::Unknown | MemoryBackupState::Running
                        )
                    {
                        ui.weak("服务已更换或重启，无法确认旧操作；请查看原保存目录。");
                    }
                    if matches!(
                        backup.state,
                        MemoryBackupState::Unknown | MemoryBackupState::Running
                    ) && ui
                        .add_enabled(
                            ready && same_instance,
                            egui::Button::new("查询上次备份状态"),
                        )
                        .clicked()
                    {
                        *action = Some(MemoryRequest::BackupStatus {
                            profile_id: self.profile_id.clone(),
                            backup_id: backup.id,
                        });
                    }
                    if matches!(backup.state, MemoryBackupState::Unknown)
                        && !backup.acknowledged
                        && ui.button("已核对，不再提醒").clicked()
                    {
                        acknowledged = Some(backup.id);
                    }
                });
            }
        }
        if let Some(id) = acknowledged
            && let Some(backup) = self
                .backups
                .get_mut(&self.profile_id)
                .and_then(|backups| backups.iter_mut().find(|backup| backup.id == id))
        {
            backup.acknowledged = true;
        }
        let running = self.backups.values().flatten().any(|backup| {
            Some(backup.instance) == self.backup_instance
                && matches!(backup.state, MemoryBackupState::Running)
        });
        if running {
            ui.weak("已有备份正在进行，请等待后查询状态。");
        } else if self.backup_instance.is_none() {
            ui.weak("等待服务身份确认后可备份。");
        }
        let can_start = ready && enabled && self.backup_instance.is_some() && !running;
        if ui
            .add_enabled(can_start, egui::Button::new("准备备份记忆"))
            .clicked()
        {
            self.backup_confirmation = true;
        }
        if self.backup_confirmation {
            ui.label("确认生成当前角色的私人记忆备份？结果未知时，旧文件可能已存在；新备份会使用新的编号。");
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(can_start, egui::Button::new("确认生成备份"))
                    .clicked()
                {
                    *action = Some(MemoryRequest::Backup {
                        profile_id: self.profile_id.clone(),
                        backup_id: uuid::Uuid::new_v4(),
                    });
                    self.backup_confirmation = false;
                }
                if ui.button("取消备份").clicked() {
                    self.backup_confirmation = false;
                }
            });
        }
    }
}
