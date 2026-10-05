use super::*;
use ai_ex_domain::{MemoryEntityKind, MemorySource};
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
        ui.weak("记下重要的事，查看经历与线索，也可以更正或遗忘。");
        ui.weak("保存或遗忘成功后会清空当前对话上下文与聊天显示；聊天输入草稿保留。");
        if let Some(feedback) = &self.feedback {
            ui.add(egui::Label::new(feedback).wrap());
        }
        if !connected {
            ui.label("连接并同步角色后，可以查看记忆。笔记草稿可以先写。");
        } else if !idle {
            ui.label("请等当前回复和声音结束，或先打断，再管理记忆。");
        }
        let ready = connected && idle && self.pending.is_none();
        let enabled = self.storage_enabled.unwrap_or(false);
        let mut action = None;
        self.show_filters(ui, ready, &mut action);
        if self.storage_enabled == Some(false) {
            ui.label("此服务未启用长期记忆。可在连接设置中启用，重启服务后生效。");
        }
        ui.separator();
        self.show_editor(ui, ready && enabled && !self.requires_review, &mut action);
        if self.view == MemoryView::Entities {
            self.show_entities(ui, ready, &mut action);
        } else {
            self.show_records(ui, ready && enabled && !self.requires_review, &mut action);
        }
        self.show_forget(ui, ready && enabled && !self.requires_review, &mut action);
        if !self.loaded && ready && action.is_none() {
            action = Some(self.list_request(0));
        }
        action
    }

    fn show_filters(&mut self, ui: &mut egui::Ui, ready: bool, action: &mut Option<MemoryRequest>) {
        let mut selected = self.view.clone();
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut selected, MemoryView::Records, "记忆列表");
                ui.selectable_value(&mut selected, MemoryView::Entities, "实体线索");
                ui.selectable_value(&mut selected, MemoryView::Media, "媒体与标签");
            });
        });
        self.set_view(selected);
        if let MemoryView::Evidence { name, .. } = &self.view {
            let name = name.clone();
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!("{name} · 关联原记录"));
                if ui
                    .add_enabled(self.pending.is_none(), egui::Button::new("返回实体线索"))
                    .clicked()
                {
                    self.set_view(MemoryView::Entities);
                }
            });
        }
        let before = (
            self.query.clone(),
            self.kind,
            self.filter_modality,
            self.filter_tags.clone(),
            self.filter_importance,
        );
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                if !matches!(self.view, MemoryView::Evidence { .. }) {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text(if self.view == MemoryView::Entities {
                                "查找姓名、偏好或事实"
                            } else {
                                "查找记忆内容"
                            })
                            .desired_width(200.0)
                            .char_limit(512),
                    );
                }
                if self.view == MemoryView::Records {
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
                }
                if self.view == MemoryView::Media {
                    egui::ComboBox::from_id_salt("memory_media_filter")
                        .selected_text(modality_filter_name(self.filter_modality))
                        .show_ui(ui, |ui| {
                            for modality in [
                                None,
                                Some(MemoryModality::Text),
                                Some(MemoryModality::Audio),
                                Some(MemoryModality::Visual),
                                Some(MemoryModality::Multimodal),
                            ] {
                                ui.selectable_value(
                                    &mut self.filter_modality,
                                    modality,
                                    modality_filter_name(modality),
                                );
                            }
                        });
                }
                if ui
                    .add_enabled(ready, egui::Button::new("查找 / 刷新"))
                    .clicked()
                {
                    *action = Some(self.list_request(0));
                }
            });
            if self.view == MemoryView::Media {
                ui.horizontal_wrapped(|ui| {
                    ui.label("标签");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter_tags)
                            .hint_text("多个标签用逗号分隔")
                            .desired_width(220.0)
                            .char_limit(1040),
                    );
                    importance_slider(ui, &mut self.filter_importance, "最低重要程度");
                });
            }
        });
        if before
            != (
                self.query.clone(),
                self.kind,
                self.filter_modality,
                self.filter_tags.clone(),
                self.filter_importance,
            )
        {
            self.invalidate_results();
            // If refresh was clicked in the same frame, include every changed filter.
            if action.is_some() {
                *action = Some(self.list_request(0));
            }
        }
        if self.pending.is_some() {
            ui.spinner();
        }
    }

    fn show_editor(&mut self, ui: &mut egui::Ui, ready: bool, action: &mut Option<MemoryRequest>) {
        let expand = self.editor_has_draft() || self.focus_editor;
        egui::CollapsingHeader::new("记下一件事")
            .id_salt("memory_editor")
            .open(expand.then_some(true))
            .show(ui, |ui| {
                ui.label(if self.editing.is_some() {
                    "更正这条记忆"
                } else {
                    "希望你记住……"
                });
                if self.editing.is_some() {
                    ui.weak("写下准确的事实。保存会替换这条原文，并标记为你更正的内容。");
                }
                ui.add_enabled_ui(self.pending.is_none() && self.forget.is_none(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt("memory_draft_modality")
                            .selected_text(modality_name(self.media_draft.modality))
                            .show_ui(ui, |ui| {
                                for modality in [
                                    MemoryModality::Text,
                                    MemoryModality::Audio,
                                    MemoryModality::Visual,
                                    MemoryModality::Multimodal,
                                ] {
                                    ui.selectable_value(
                                        &mut self.media_draft.modality,
                                        modality,
                                        modality_name(modality),
                                    );
                                }
                            });
                        importance_slider(ui, &mut self.media_draft.importance, "重要程度");
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("memory_draft_scroll")
                        .max_height(120.0)
                        .show(ui, |ui| {
                            let response = ui.add(
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
                    ui.horizontal(|ui| {
                        ui.label("标签");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.media_draft.tags)
                                .hint_text("例如：学习、日常；使用逗号分隔")
                                .desired_width(f32::INFINITY)
                                .char_limit(1040),
                        );
                    });
                    if editor::has_audio(self.media_draft.modality) {
                        ui.label("声音描述");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.media_draft.audio_description)
                                .desired_width(f32::INFINITY)
                                .desired_rows(2)
                                .char_limit(2048)
                                .hint_text("例如：语气轻快，背景有雨声。"),
                        );
                    }
                    if editor::has_visual(self.media_draft.modality) {
                        ui.label("画面描述");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.media_draft.visual_description)
                                .desired_width(f32::INFINITY)
                                .desired_rows(2)
                                .char_limit(2048)
                                .hint_text("例如：窗边的蓝色杯子，傍晚的暖光。"),
                        );
                    }
                    if self.media_draft.clears_features() {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "保存新的类型会移除不再适用的声音或画面特征；其它记录信息会保留。",
                        );
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
                            ready && self.forget.is_none() && !self.draft.trim().is_empty(),
                            egui::Button::new(label),
                        )
                        .clicked()
                    {
                        match self.editor_request() {
                            Ok(request) => *action = Some(request),
                            Err(error) => self.feedback = Some(error),
                        }
                    }
                    if self.editor_has_draft()
                        && ui
                            .add_enabled(
                                self.pending.is_none(),
                                egui::Button::new(if self.editing.is_some() {
                                    "取消更正"
                                } else {
                                    "清空草稿"
                                }),
                            )
                            .clicked()
                    {
                        self.clear_editor();
                    }
                    ui.weak(format!("{} / 4096", self.draft.chars().count()));
                });
                ui.separator();
            });
    }

    fn show_paging(
        &self,
        ui: &mut egui::Ui,
        ready: bool,
        total: usize,
        offset: usize,
        count: usize,
        action: &mut Option<MemoryRequest>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "找到 {total} 条{}",
                if self.view == MemoryView::Entities {
                    "线索"
                } else {
                    "记忆"
                }
            ));
            if ui
                .add_enabled(ready && offset > 0, egui::Button::new("上一页"))
                .clicked()
            {
                *action = Some(self.list_request(self.previous_offset()));
            }
            if ui
                .add_enabled(
                    ready && count > 0 && offset.saturating_add(count) < total,
                    egui::Button::new("下一页"),
                )
                .clicked()
            {
                *action = Some(self.list_request(offset.saturating_add(count)));
            }
        });
    }

    fn show_records(&mut self, ui: &mut egui::Ui, ready: bool, action: &mut Option<MemoryRequest>) {
        let Some(page) = &self.page else {
            ui.weak("记忆会在这里显示；修改筛选后点击“查找 / 刷新”。");
            return;
        };
        self.show_paging(
            ui,
            ready,
            page.total,
            page.offset,
            page.entries.len(),
            action,
        );
        if page.entries.is_empty() {
            ui.weak("还没有符合条件的记忆。可以先记下一件事，或换个条件查找。");
        }
        let mut edit = None;
        let mut forget = None;
        let mut evidence = None;
        for entry in &page.entries {
            let truncated = page.truncated_ids.contains(&entry.id);
            let knowledge = page.knowledge.get(&entry.id);
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
                            .add_enabled(
                                ready && !self.editor_has_draft(),
                                egui::Button::new("更正"),
                            )
                            .clicked()
                        {
                            edit = Some((entry.clone(), knowledge.cloned(), truncated));
                        }
                        if ui
                            .add_enabled(
                                ready && !self.editor_has_draft(),
                                egui::Button::new("遗忘"),
                            )
                            .clicked()
                        {
                            forget = Some(entry.clone());
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
                    if let Some(media) = knowledge.and_then(|item| item.media.as_ref()) {
                        ui.horizontal_wrapped(|ui| {
                            ui.weak(modality_name(media.modality));
                            ui.weak(format!(
                                "重要程度 {:.1}%",
                                f32::from(media.importance) / 10.0
                            ));
                            for tag in &media.tags {
                                ui.label(format!("#{tag}"));
                            }
                        });
                        for (label, features) in [
                            ("声音", &media.audio_features),
                            ("画面", &media.visual_features),
                        ] {
                            if let Some(description) = features
                                .as_ref()
                                .and_then(|map| map.get("description"))
                                .and_then(serde_json::Value::as_str)
                            {
                                ui.add(
                                    egui::Label::new(format!(
                                        "{label}：{}",
                                        description.chars().take(240).collect::<String>()
                                    ))
                                    .wrap(),
                                );
                            }
                        }
                    }
                    if let Some(knowledge) = knowledge.filter(|item| !item.entities.is_empty()) {
                        ui.weak("提取的候选线索 · 可查看关联原记录核对");
                        ui.horizontal_wrapped(|ui| {
                            for entity in &knowledge.entities {
                                if ui
                                    .add_enabled(
                                        self.pending.is_none(),
                                        egui::Button::new(format!(
                                            "{}：{}",
                                            entity.name, entity.value
                                        ))
                                        .small(),
                                    )
                                    .clicked()
                                {
                                    evidence = Some((entity.key.clone(), entity.name.clone()));
                                }
                            }
                        });
                    }
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
        if let Some((entry, knowledge, truncated)) = edit {
            self.start_edit(&entry, knowledge.as_ref(), truncated);
        }
        if let Some(entry) = forget {
            self.forget = Some(entry);
        }
        if let Some((key, name)) = evidence {
            self.set_view(MemoryView::Evidence { key, name });
        }
    }

    fn show_entities(
        &mut self,
        ui: &mut egui::Ui,
        ready: bool,
        action: &mut Option<MemoryRequest>,
    ) {
        ui.weak(
            "这些是从记忆内容提取的候选线索。原记录会保留来源；出现不同说法时，可先查看再更正。",
        );
        let Some(page) = &self.entity_page else {
            ui.weak("输入关键词后点击“查找 / 刷新”查看线索。");
            return;
        };
        self.show_paging(
            ui,
            ready,
            page.total,
            page.offset,
            page.entities.len(),
            action,
        );
        if page.entities.is_empty() {
            ui.weak("暂无线索。记下姓名、偏好或事实后，可在这里查看关联记录。");
        }
        let mut evidence = None;
        for group in &page.entities {
            ui.push_id(&group.key, |ui| {
                crate::ui::theme::card().inner_margin(14).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(&group.name);
                        ui.weak(entity_kind_name(group.kind));
                        ui.weak(format!("{} 次提及", group.total_mentions));
                    });
                    ui.add(egui::Label::new(format!("当前候选：{}", group.current_value)).wrap());
                    let values = group
                        .evidence
                        .iter()
                        .map(|item| item.value.as_str())
                        .collect::<std::collections::BTreeSet<_>>();
                    if values.len() > 1 {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "存在不同说法，请核对关联原记录。",
                        );
                    }
                    ui.weak(format!(
                        "首次提及：{} · 最近更新：{}",
                        age(u128::from(group.first_mentioned_ms)),
                        age(u128::from(group.last_updated_ms))
                    ));
                    egui::CollapsingHeader::new("查看候选来源概览").show(ui, |ui| {
                        for item in &group.evidence {
                            ui.add(
                                egui::Label::new(format!(
                                    "{} · {}",
                                    source_name(item.source),
                                    item.value
                                ))
                                .wrap(),
                            );
                        }
                    });
                    if ui
                        .add_enabled(ready, egui::Button::new("查看关联原记录"))
                        .clicked()
                    {
                        evidence = Some((group.key.clone(), group.name.clone()));
                    }
                });
            });
        }
        if let Some((key, name)) = evidence {
            self.set_view(MemoryView::Evidence { key, name });
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

fn importance_slider(ui: &mut egui::Ui, value: &mut u16, label: &str) {
    let mut percent = f32::from(*value) / 10.0;
    if ui
        .add(
            egui::Slider::new(&mut percent, 0.0..=100.0)
                .text(label)
                .suffix("%"),
        )
        .changed()
    {
        *value = (percent * 10.0).round() as u16;
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
fn modality_name(modality: MemoryModality) -> &'static str {
    match modality {
        MemoryModality::Text => "文字",
        MemoryModality::Audio => "声音描述",
        MemoryModality::Visual => "画面描述",
        MemoryModality::Multimodal => "声画描述",
    }
}
fn modality_filter_name(modality: Option<MemoryModality>) -> &'static str {
    modality.map(modality_name).unwrap_or("全部媒体类型")
}
fn entity_kind_name(kind: MemoryEntityKind) -> &'static str {
    match kind {
        MemoryEntityKind::Name => "姓名线索",
        MemoryEntityKind::Preference => "偏好线索",
        MemoryEntityKind::Fact => "事实线索",
    }
}
fn source_name(source: MemorySource) -> &'static str {
    match source {
        MemorySource::Automatic => "自动记录 · 未经确认",
        MemorySource::UserNote => "你明确记下的事",
        MemorySource::UserCorrection => "你更正过的内容",
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
