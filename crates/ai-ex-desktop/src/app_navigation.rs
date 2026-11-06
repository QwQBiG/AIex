use super::*;
use crate::navigation::Destination;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryState {
    NotDelivered,
    Uncertain,
}

pub(super) struct RetainedMessage {
    pub(super) text: String,
    pub(super) state: DeliveryState,
}

impl DesktopApp {
    pub(super) fn request_navigation(&mut self, context: &egui::Context, destination: Destination) {
        let has_drafts = !self.input.is_empty()
            || !self.retained_messages.is_empty()
            || self.persona_dirty
            || self.confirm_persona
            || self.persona_apply_pending
            || self.state.runtime.active_turn.is_some()
            || self.scene_busy()
            || self.character_files.is_loading()
            || self.appearance.is_loading();
        let has_drafts = has_drafts
            || self.memory.has_unsaved()
            || self.native.has_unsaved()
            || self.speech_split.has_unsaved();
        if has_drafts {
            self.pending_navigation = Some(destination);
        } else {
            self.navigation.request(context, destination);
        }
    }

    pub(super) fn show_navigation_confirmation(&mut self, context: &egui::Context) {
        let Some(destination) = self.pending_navigation else {
            return;
        };
        let mut leave = false;
        let mut stay = false;
        egui::Window::new("离开当前对话？")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                ui.label("当前还有消息草稿、角色修改或正在处理的操作。");
                ui.label("离开后，部分草稿或操作状态可能无法恢复。");
                if self.memory.backup_unresolved() {
                    ui.label(
                        "有记忆备份尚未确认。关闭本应用的服务可能中断备份；请检查原保存目录。",
                    );
                }
                if self.owned_service.is_some() {
                    ui.label("当前回复也会停止。");
                }
                ui.horizontal(|ui| {
                    stay = ui.button("继续编辑").clicked();
                    leave = ui.button("离开并继续").clicked();
                });
            });
        if leave {
            self.pending_navigation = None;
            self.allow_backup_close = true;
            self.navigation.request(context, destination);
        } else if stay {
            self.pending_navigation = None;
        }
    }

    pub(super) fn handle_backup_close(&mut self, context: &egui::Context, requested: bool) {
        if requested && !self.allow_backup_close && self.memory.backup_unresolved() {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_backup_close = true;
        }
    }

    pub(super) fn show_backup_close_confirmation(&mut self, context: &egui::Context) {
        if !self.confirm_backup_close {
            return;
        }
        if !self.memory.backup_unresolved() {
            self.confirm_backup_close = false;
            return;
        }
        let mut stay = false;
        let mut close = false;
        egui::Window::new("备份尚未确认")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                ui.label("关闭应用会丢失本次备份编号；如果本应用管理服务，备份也可能中断。请检查原保存目录。");
                ui.horizontal(|ui| {
                    stay = ui.button("返回等待").clicked();
                    close = ui.button("仍要退出").clicked();
                });
            });
        if stay {
            self.confirm_backup_close = false;
        } else if close {
            self.confirm_backup_close = false;
            self.allow_backup_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    pub(super) fn show_retained_messages(&mut self, context: &egui::Context) {
        if !self.show_retained {
            return;
        }
        let mut open = true;
        let mut restore = None;
        let mut remove = None;
        egui::Window::new("保留的消息")
            .open(&mut open)
            .default_width(460.0)
            .show(context, |ui| {
                ui.label("原文在这里保留。请先查看每条消息的状态，再决定是否恢复为草稿。");
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        for (index, message) in self.retained_messages.iter().enumerate() {
                            ui.push_id(index, |ui| {
                                ui.group(|ui| {
                                    let uncertain = message.state == DeliveryState::Uncertain;
                                    if uncertain {
                                        ui.colored_label(ui.visuals().warn_fg_color,
                                            "结果待核对：服务可能已收到，请先检查聊天记录，避免重复发送。");
                                    } else {
                                        ui.weak("未送达，可以恢复后重新发送。");
                                    }
                                    ui.add(egui::Label::new(&message.text).wrap());
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button("复制").clicked() {
                                            ui.ctx().copy_text(message.text.clone());
                                        }
                                        if ui
                                            .add_enabled(
                                                self.input.is_empty(),
                                                egui::Button::new(if uncertain { "确认未收到，恢复草稿" } else { "恢复到输入框" }),
                                            )
                                            .clicked()
                                        {
                                            restore = Some(index);
                                        }
                                        if ui.button("移除此草稿").clicked() {
                                            remove = Some(index);
                                        }
                                    });
                                });
                            });
                        }
                    });
                if self.retained_messages.is_empty() {
                    ui.weak("没有待恢复的消息。");
                }
            });
        if let Some(index) = restore {
            if let Some(message) = self.retained_messages.remove(index) {
                self.input = message.text;
                self.page = super::layout::Page::Conversation;
                open = false;
            }
        } else if let Some(index) = remove {
            self.retained_messages.remove(index);
        }
        self.show_retained = open;
    }
}
