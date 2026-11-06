use super::*;

impl DesktopRuntimePanel {
    pub fn show(&mut self, ui: &mut egui::Ui) -> Vec<WorkerCommand> {
        let mut requests = Vec::new();
        ui.group(|ui| {
            ui.strong("陪伴方式与快捷键");
            ui.weak("修改后点击应用。这里只调整陪伴行为，不重载模型或设备。");
            ui.add_enabled_ui(self.connected, |ui| {
                ui.checkbox(
                    &mut self.draft.proactive_enabled,
                    "允许伙伴在空闲时主动说话",
                );
                ui.add(
                    egui::Slider::new(&mut self.draft.cooldown_seconds, 10..=86400)
                        .logarithmic(true)
                        .text("两次主动说话至少间隔（秒）"),
                );
                ui.checkbox(&mut self.draft.do_not_disturb, "免打扰（停止主动说话）");
                let mut quiet = self.draft.quiet_hours.is_some();
                if ui.checkbox(&mut quiet, "定时安静时段").changed() {
                    self.draft.quiet_hours = quiet.then_some(ai_ex_domain::QuietHours {
                        start_minute: 1320,
                        end_minute: 480,
                    });
                }
                if let Some(hours) = self.draft.quiet_hours.as_mut() {
                    ui.horizontal(|ui| {
                        minute_editor(ui, "从", &mut hours.start_minute);
                        minute_editor(ui, "到", &mut hours.end_minute);
                        ui.weak("本地时间，支持跨午夜");
                    });
                }
                ui.checkbox(&mut self.draft.captions_visible, "显示独立悬浮字幕");
                ui.checkbox(
                    &mut self.draft.hotkeys_enabled,
                    "启用全局快捷键（本次启动需重新应用）",
                );
                ui.weak(
                    "Ctrl + Alt + F6 打断 · F7 急停 · F8 暂停 · F9 恢复 · F10 字幕 · F11 已保存单步",
                );
            });
            if let Some(state) = self.confirmed.as_ref() {
                ui.label(format!(
                    "已确认设置 #{} · 快捷键{}",
                    state.revision,
                    if state.hotkeys_registered {
                        "已注册"
                    } else {
                        "未注册"
                    }
                ));
                if state.hotkeys_error {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "快捷键未就绪，可能与其他程序冲突；处理后重新应用。",
                    );
                }
                if state.revision != self.draft_revision {
                    ui.weak("设置已在其他位置变化；编辑草稿已保留，请先核对。 ");
                }
            } else {
                ui.weak("等待服务确认当前设置。");
            }
            if self.draft.validate().is_err() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "请设置不同的安静时段起止时间，再应用。",
                );
            }
            ui.horizontal_wrapped(|ui| {
                let apply = self.mutation(false);
                if ui
                    .add_enabled(apply.is_some(), egui::Button::new("应用设置"))
                    .clicked()
                    && let Some(request) = apply
                {
                    requests.push(request);
                }
                let reload = self.mutation(true);
                if ui
                    .add_enabled(reload.is_some(), egui::Button::new("重新读取行为设置文件"))
                    .clicked()
                    && let Some(request) = reload
                {
                    requests.push(request);
                }
                if ui.button("采用已确认设置").clicked()
                    && let Some(state) = &self.confirmed
                {
                    self.draft = state.settings.clone();
                    self.draft_revision = state.revision;
                }
                if ui
                    .add_enabled(self.connected, egui::Button::new("刷新"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::State);
                }
            });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(self.connected, egui::Button::new("导出脱敏诊断"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::Diagnostics);
                }
                let vts = self.connected
                    && self
                        .confirmed
                        .as_ref()
                        .is_some_and(|state| state.vts_discovery_available);
                if ui
                    .add_enabled(vts, egui::Button::new("发现 VTS 人物热键"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::DiscoverVts);
                }
                let mappings_pending = self.pending.values().any(|request| {
                    matches!(request, DesktopRuntimeRequest::CheckVtsMappings)
                });
                if ui
                    .add_enabled(vts && !mappings_pending, egui::Button::new("核对 VTS 表情映射"))
                    .clicked()
                {
                    requests.push(DesktopRuntimeRequest::CheckVtsMappings);
                }
                let parameter_pending = self.pending.values().any(is_parameter_request);
                for (label, filter) in [
                    ("全部输入参数", None),
                    ("嘴部参数", Some(VtsParameterFilter::Mouth)),
                    ("声音参数", Some(VtsParameterFilter::Voice)),
                ] {
                    if ui.add_enabled(vts && !parameter_pending, egui::Button::new(label)).clicked() {
                        requests.push(parameter_request(filter, 0));
                    }
                }
            });
            ui.weak(
                "诊断包含版本、运行时长、行为设置和组件状态，不包含对话、原始日志、私人音频或路径。",
            );
            if let Some(check) = &self.vts_mappings {
                ui.strong("当前生效的表情映射");
                ui.weak("这是核对时的人物与配置快照；更换人物或修改配置后请重新核对。");
                if !check.model_loaded {
                    ui.weak("VTS 尚未载入人物，已设置的映射暂时无法核对。");
                }
                for item in &check.mappings {
                    let emotion = match item.emotion {
                        ai_ex_domain::Emotion::Neutral => "平静",
                        ai_ex_domain::Emotion::Happy => "开心",
                        ai_ex_domain::Emotion::Sad => "低落",
                        ai_ex_domain::Emotion::Angry => "生气",
                        ai_ex_domain::Emotion::Surprised => "惊讶",
                    };
                    let status = match item.status {
                        VtsMappingState::Unconfigured => "未设置",
                        VtsMappingState::Matched => "匹配",
                        VtsMappingState::Missing => "未找到热键",
                        VtsMappingState::Unknown => "无法核对",
                    };
                    let id = item.hotkey_id.as_deref().unwrap_or("—");
                    ui.label(format!("{emotion}：{status} · {id}{}", if item.id_truncated { "（已截短）" } else { "" }));
                }
            } else if !self.connected {
                ui.weak("表情映射暂时无法核对：请先连接服务。");
            } else if self.confirmed.as_ref().is_some_and(|state| !state.vts_discovery_available) {
                ui.weak("表情映射暂时无法核对：请确认 VTS 已启用并连接。");
            }
            if let Some(observations) = &self.component_observations {
                ui.strong("组件状态记录");
                ui.weak("显示导出时已有的记录，可能已过期；不代表模型或设备已通过实测。 ");
                for (label, state) in [("记录为可用", Some(true)), ("关闭或不可用", Some(false)), ("暂无记录", None)] {
                    let names = component_names(observations, state);
                    if !names.is_empty() {
                        ui.label(format!("{label}：{}", names.join("、")));
                    }
                }
            }
            for key in &self.vts {
                ui.label(format!("{} · {}", key.name, key.id));
            }
            if let Some(page) = &self.vts_parameters {
                ui.strong(match self.vts_parameter_filter {
                    None => "全部输入参数",
                    Some(VtsParameterFilter::Mouth) => "嘴部参数",
                    Some(VtsParameterFilter::Voice) => "声音参数",
                });
                ui.weak("筛选按参数名称匹配 mouth、lip 或 voice；其他命名请查看全部参数。 ");
                ui.weak("翻页时会重新读取当前参数；人物或参数变化时结果可能改变。");
                if !page.model_loaded {
                    ui.weak("VTS 当前没有载入人物，输入参数仍可查询。");
                }
                if page.parameters.is_empty() && page.total > 0 {
                    ui.weak("参数列表已变化，当前页为空，请回到首页重新查看。");
                    if ui.add_enabled(!self.pending.values().any(is_parameter_request), egui::Button::new("回到首页")).clicked() {
                        requests.push(parameter_request(self.vts_parameter_filter, 0));
                    }
                }
                for parameter in &page.parameters {
                    ui.label(format!(
                        "{}{} · 当前 {} · 映射参考范围 {}–{} · 默认 {}",
                        parameter.name,
                        if parameter.is_custom { "（自定义参数）" } else { "（默认参数）" },
                        parameter.value,
                        parameter.min,
                        parameter.max,
                        parameter.default_value,
                    ));
                    if parameter.is_custom && !parameter.added_by.is_empty() {
                        ui.weak(format!("来源：{}", parameter.added_by));
                    }
                }
                let busy = self.pending.values().any(is_parameter_request);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy && page.offset > 0, egui::Button::new("上一页")).clicked() {
                        requests.push(parameter_request(self.vts_parameter_filter, page.offset.saturating_sub(16)));
                    }
                    ui.label(format!("{}–{} / {}", if page.parameters.is_empty() { page.offset } else { page.offset + 1 }, page.offset + page.parameters.len(), page.total));
                    if ui.add_enabled(!busy && page.offset + page.parameters.len() < page.total, egui::Button::new("下一页")).clicked() {
                        requests.push(parameter_request(self.vts_parameter_filter, page.offset + page.parameters.len()));
                    }
                });
            }
            if let Some(message) = &self.feedback {
                ui.label(message);
            }
        });
        requests
            .into_iter()
            .filter_map(|request| self.begin(request))
            .collect()
    }
}

fn component_names(
    observations: &[ai_ex_domain::DiagnosticComponentObservation],
    expected: Option<bool>,
) -> Vec<&'static str> {
    use ai_ex_domain::DiagnosticComponent as Component;
    const COMPONENTS: &[(Component, &str)] = &[
        (Component::Native, "原生组件"),
        (Component::Ollama, "Ollama"),
        (Component::OllamaNative, "托管 Ollama"),
        (Component::Deepseek, "DeepSeek"),
        (Component::Koboldcpp, "KoboldCpp"),
        (Component::Memory, "记忆"),
        (Component::AudioQueue, "语音队列"),
        (Component::AudioInput, "麦克风"),
        (Component::AudioOutput, "扬声器"),
        (Component::Asr, "语音识别"),
        (Component::GptSovits, "外部语音"),
        (Component::Vts, "VTube Studio"),
        (Component::Vision, "视觉"),
        (Component::Safety, "安全控制"),
    ];
    COMPONENTS
        .iter()
        .filter_map(|(component, label)| {
            let state = observations
                .iter()
                .filter(|entry| entry.component == *component)
                .map(|entry| entry.observed_ready)
                .reduce(|left, right| left && right);
            (state == expected).then_some(*label)
        })
        .collect()
}

fn minute_editor(ui: &mut egui::Ui, label: &str, minute: &mut u16) {
    let mut hour = *minute / 60;
    let mut minutes = *minute % 60;
    ui.label(label);
    ui.add(egui::DragValue::new(&mut hour).range(0..=23).suffix("时"));
    ui.add(
        egui::DragValue::new(&mut minutes)
            .range(0..=59)
            .suffix("分"),
    );
    *minute = hour * 60 + minutes;
}
