use super::*;
use ai_ex_domain::MemorySource;
use eframe::egui;

impl MemoryPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        connected: bool,
        idle: bool,
    ) -> Option<MemoryRequest> {
        ui.heading(format!("和 {name} 的记忆"));
        ui.weak("把希望记住的事写在这里，也可以查看、更正或遗忘这个角色的经历。");
        ui.weak("新建、更正和遗忘成功后，会清空当前对话上下文与聊天显示；输入草稿保留。");
        if let Some(feedback) = &self.feedback {
            ui.add(egui::Label::new(feedback).wrap());
        }
        if !connected {
            ui.label("连接并同步角色后，可以查看记忆。笔记草稿可以先写。");
        } else if !idle {
            ui.label("请等当前回复和声音结束，或先打断，再管理记忆。");
        }
        let ready = connected && idle && self.pending.is_none();
        let enabled = self.page.as_ref().is_some_and(|page| page.enabled);
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("查找记忆内容")
                    .desired_width(200.0)
                    .char_limit(512),
            );
            egui::ComboBox::from_id_salt("memory_kind")
                .selected_text(kind_name(self.kind))
                .show_ui(ui, |ui| {
                    for kind in [
                        None,
                        Some(MemoryKind::Persona),
                        Some(MemoryKind::Conversation),
                        Some(MemoryKind::Viewer),
                        Some(MemoryKind::LiveEvent),
                    ] {
                        ui.selectable_value(&mut self.kind, kind, kind_name(kind));
                    }
                });
            if ui
                .add_enabled(ready, egui::Button::new("查找 / 刷新"))
                .clicked()
            {
                action = Some(self.list_request(0));
            }
            if self.pending.is_some() {
                ui.spinner();
            }
        });
        if self.page.as_ref().is_some_and(|page| !page.enabled) {
            ui.label("此服务未启用长期记忆。可在连接设置中启用，重启服务后生效。");
        }
        ui.separator();
        let expand_editor = self.editing.is_some() || !self.draft.is_empty() || self.focus_editor;
        egui::CollapsingHeader::new("记下一件事")
            .id_salt("memory_editor")
            .open(expand_editor.then_some(true))
            .show(ui, |ui| {
                ui.label(if self.editing.is_some() {
                    "更正这条记忆"
                } else {
                    "希望你记住……"
                });
                if self.editing.is_some() {
                    ui.weak("写下准确的事实。保存会替换这条记录的原文，并标记为你更正的内容。");
                }
                egui::ScrollArea::vertical()
                    .id_salt("memory_draft_scroll")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        let response = ui.add_enabled(
                            self.pending.is_none() && self.forget.is_none(),
                            egui::TextEdit::multiline(&mut self.draft)
                                .desired_width(f32::INFINITY)
                                .desired_rows(3)
                                .char_limit(4096)
                                .hint_text("例如：请叫我小林；学习时希望你简短回应。"),
                        );
                        if self.focus_editor {
                            response.request_focus();
                            response.scroll_to_me(Some(egui::Align::Min));
                            self.focus_editor = false;
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    let label = if self.editing.is_some() {
                        "保存更正"
                    } else {
                        "记住这件事"
                    };
                    if ui
                        .add_enabled(
                            ready
                                && enabled
                                && !self.requires_review
                                && self.forget.is_none()
                                && !self.draft.trim().is_empty(),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        let text = self.draft.trim().to_owned();
                        action = Some(match &self.editing {
                            Some(entry) => MemoryRequest::Correct {
                                profile_id: self.profile_id.clone(),
                                id: entry.id,
                                expected_revision: entry.revision,
                                text,
                            },
                            None => MemoryRequest::Remember {
                                profile_id: self.profile_id.clone(),
                                text,
                            },
                        });
                    }
                    if self.editing.is_some()
                        && ui
                            .add_enabled(self.pending.is_none(), egui::Button::new("取消更正"))
                            .clicked()
                    {
                        self.editing = None;
                        self.draft.clear();
                    }
                    ui.weak(format!("{} / 4096", self.draft.chars().count()));
                });
                ui.separator();
            });
        self.show_records(ui, ready && enabled && !self.requires_review, &mut action);
        self.show_forget(ui, ready && enabled && !self.requires_review, &mut action);
        if !self.loaded && ready && action.is_none() {
            action = Some(self.list_request(0));
        }
        action
    }
}

fn kind_name(kind: Option<MemoryKind>) -> &'static str {
    match kind {
        None => "全部记忆",
        Some(MemoryKind::Persona) => "长期设定与笔记",
        Some(MemoryKind::Conversation) => "对话经历",
        Some(MemoryKind::Viewer) => "观众信息",
        Some(MemoryKind::LiveEvent) => "直播事件",
    }
}

fn source_name(source: MemorySource) -> &'static str {
    match source {
        MemorySource::Automatic => "自动记录 · 未经确认",
        MemorySource::UserNote => "你明确记下的事",
        MemorySource::UserCorrection => "你更正过的内容",
    }
}

impl MemoryPanel {
    fn show_records(&mut self, ui: &mut egui::Ui, ready: bool, action: &mut Option<MemoryRequest>) {
        let Some(page) = &self.page else {
            ui.weak("记忆会在这里显示，仅属于当前角色。");
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("找到 {} 条记忆", page.total));
            if ui
                .add_enabled(ready && page.offset > 0, egui::Button::new("上一页"))
                .clicked()
            {
                *action = Some(self.list_request(self.previous_offset()));
            }
            if ui
                .add_enabled(
                    ready && page.offset.saturating_add(page.entries.len()) < page.total,
                    egui::Button::new("下一页"),
                )
                .clicked()
            {
                *action = Some(self.list_request(page.offset.saturating_add(page.entries.len())));
            }
        });
        if page.entries.is_empty() {
            ui.weak("还没有符合条件的记忆。可以先记下一件事，或换个关键词查找。");
        }
        for entry in &page.entries {
            let truncated = page.truncated_ids.contains(&entry.id);
            ui.push_id(entry.id, |ui| {
                crate::ui::theme::card().inner_margin(14).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(source_name(entry.source));
                        ui.weak(kind_name(Some(entry.kind)));
                        ui.weak(age(entry.updated_ms.unwrap_or(entry.created_ms)));
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(if truncated {
                                "复制片段"
                            } else {
                                "复制内容"
                            })
                            .clicked()
                        {
                            let text = if entry.assistant_text.is_empty() {
                                entry.user_text.clone()
                            } else {
                                format!("{}\n\n{}", entry.user_text, entry.assistant_text)
                            };
                            ui.ctx().copy_text(text);
                        }
                        if ui
                            .add_enabled(ready && self.draft.is_empty(), egui::Button::new("更正"))
                            .clicked()
                        {
                            self.draft = if truncated {
                                String::new()
                            } else {
                                entry.user_text.clone()
                            };
                            self.editing = Some(entry.clone());
                            self.focus_editor = true;
                            self.forget = None;
                        }
                        if ui
                            .add_enabled(ready && self.draft.is_empty(), egui::Button::new("遗忘"))
                            .clicked()
                        {
                            self.forget = Some(entry.clone());
                        }
                    });
                    let preview: String = entry
                        .user_text
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(160)
                        .collect();
                    ui.add(egui::Label::new(preview).wrap());
                    if truncated {
                        ui.weak("较长记录，仅显示片段；更正时请重新写下准确的事实。");
                    }
                    egui::CollapsingHeader::new(if truncated {
                        "查看片段"
                    } else {
                        "查看原文"
                    })
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(220.0)
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(&entry.user_text).wrap());
                                if !entry.assistant_text.is_empty() {
                                    ui.separator();
                                    ui.weak("当时的回复");
                                    ui.add(egui::Label::new(&entry.assistant_text).wrap());
                                }
                            });
                    });
                });
            });
        }
    }

    fn show_forget(&mut self, ui: &mut egui::Ui, ready: bool, action: &mut Option<MemoryRequest>) {
        let Some(entry) = &self.forget else {
            return;
        };
        let mut cancel = false;
        egui::Window::new("遗忘这条记忆？")
            .id(egui::Id::new("memory_forget_confirm"))
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label("这条记录会从当前角色的记忆中移除，无法撤销。");
                egui::ScrollArea::vertical()
                    .id_salt("forget_preview")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(entry.user_text.chars().take(200).collect::<String>())
                                .wrap(),
                        );
                    });
                ui.weak("同样的信息若存在于其他记录，仍需单独更正或遗忘。角色设定保持独立。");
                ui.horizontal(|ui| {
                    cancel = ui
                        .add_enabled(self.pending.is_none(), egui::Button::new("保留记忆"))
                        .clicked();
                    if ui
                        .add_enabled(ready, egui::Button::new("确认遗忘"))
                        .clicked()
                    {
                        *action = Some(MemoryRequest::Forget {
                            profile_id: self.profile_id.clone(),
                            id: entry.id,
                            expected_revision: entry.revision,
                        });
                    }
                });
            });
        if cancel {
            self.forget = None;
        }
    }
}

fn age(milliseconds: u128) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let minutes = now.saturating_sub(milliseconds) / 60_000;
    match minutes {
        0 => "刚刚".to_owned(),
        1..60 => format!("{minutes} 分钟前"),
        60..1440 => format!("{} 小时前", minutes / 60),
        _ => format!("{} 天前", minutes / 1440),
    }
}
