use super::*;
use ai_ex_config::native::NativeSpeechCleanup;

impl NativePanel {
    pub(super) fn speech_cleanup_settings(
        &mut self,
        ui: &mut egui::Ui,
        requests: &mut Vec<NativeRequest>,
    ) {
        let Some(config) = self.config.clone() else {
            return;
        };
        let mut cleanup = self
            .speech_cleanup_draft
            .clone()
            .unwrap_or_else(|| config.speech_cleanup.clone());
        crate::ui::theme::card().show(ui, |ui| {
            ui.strong("朗读文本处理");
            let locked = self.saving_settings();
            if locked {
                ui.weak("设置正在保存或结果未确认；请等待核对，必要时重启服务。草稿会保留。");
            }
            ui.add_enabled_ui(!locked, |ui| {
            let mut custom = cleanup.is_some();
            let mut changed = ui.checkbox(&mut custom, "为本地伙伴单独设置").changed();
            if changed {
                cleanup = custom.then(NativeSpeechCleanup::default);
            }
            if let Some(settings) = &mut cleanup {
                changed |= ui.checkbox(&mut settings.remove_emoji, "省略表情符号").changed();
                changed |= ui.checkbox(&mut settings.remove_markdown, "处理格式标记并省略代码片段").changed();
                changed |= ui.checkbox(&mut settings.remove_parenthetical, "省略括号中的内容").changed();
            } else {
                ui.weak("沿用本地服务启动配置；普通连接设置不会修改本地伙伴的配置。");
            }
            ui.small(
                "影响朗读和播放字幕，不改变对话正文；保存后重启服务生效，试听和正式发声使用同一设置。",
            );
            if changed {
                self.record_speech_cleanup_edit(cleanup.clone());
            }
            if self.speech_cleanup_draft.is_some() {
                ui.weak("朗读文本设置尚未保存。");
            }
            if ui
                .add_enabled(
                    self.speech_cleanup_draft.is_some() && !self.has_pending() && !self.uncertain,
                    egui::Button::new("保存朗读文本设置"),
                )
                .clicked()
            {
                match self.speech_cleanup_request() {
                    Ok(request) => requests.push(request),
                    Err(error) => self.status = error.to_string(),
                }
            }
            });
        });
    }

    fn record_speech_cleanup_edit(&mut self, cleanup: Option<NativeSpeechCleanup>) {
        if let Some(config) = &self.config {
            self.speech_cleanup_draft =
                (cleanup != config.speech_cleanup || self.saving_settings()).then_some(cleanup);
        }
    }

    fn speech_cleanup_request(&self) -> Result<NativeRequest, AppError> {
        let mut next = self
            .config
            .clone()
            .ok_or_else(|| AppError::unavailable("本地设置尚未就绪"))?;
        next.speech_cleanup = self
            .speech_cleanup_draft
            .clone()
            .ok_or_else(|| AppError::configuration("没有需要保存的朗读文本设置"))?;
        Ok(NativeRequest::SaveSettings {
            expected_revision: next.revision,
            document: next.to_toml()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_cleanup_save_preserves_unrelated_drafts_and_newer_edits() {
        let mut panel = NativePanel {
            config: Some(NativeConfig::default()),
            ..Default::default()
        };
        panel.device_draft = Some(ai_ex_config::native::NativeDevices {
            microphone_enabled: true,
            playback_enabled: false,
        });
        let first = NativeSpeechCleanup {
            remove_parenthetical: false,
            ..Default::default()
        };
        panel.speech_cleanup_draft = Some(Some(first.clone()));
        assert!(panel.has_unsaved());
        let request = panel.speech_cleanup_request().unwrap();
        let NativeRequest::SaveSettings { document, .. } = &request else {
            panic!("expected native settings update")
        };
        let mut saved = NativeConfig::parse(document).unwrap();
        assert_eq!(saved.speech_cleanup, Some(first));
        assert!(!saved.devices.microphone_enabled);
        let WorkerCommand::Native { request_id, .. } = panel.begin(request).unwrap() else {
            panic!("expected native request")
        };
        let newer = NativeSpeechCleanup {
            remove_emoji: false,
            ..Default::default()
        };
        panel.speech_cleanup_draft = Some(Some(newer.clone()));
        saved.revision += 1;
        panel.receive(
            request_id,
            Ok(NativeReply::Settings {
                revision: saved.revision,
                document: saved.to_toml().unwrap(),
            }),
        );
        assert_eq!(panel.speech_cleanup_draft, Some(Some(newer)));
        assert!(panel.device_draft.unwrap().microphone_enabled);
        assert!(panel.has_unsaved());
    }

    #[test]
    fn speech_cleanup_edit_back_to_old_value_survives_inflight_save() {
        let mut panel = NativePanel {
            config: Some(NativeConfig::default()),
            ..Default::default()
        };
        panel.record_speech_cleanup_edit(Some(NativeSpeechCleanup::default()));
        let request = panel.speech_cleanup_request().unwrap();
        let NativeRequest::SaveSettings { document, .. } = &request else {
            panic!("expected native settings update")
        };
        let mut submitted = NativeConfig::parse(document).unwrap();
        let WorkerCommand::Native { request_id, .. } = panel.begin(request).unwrap() else {
            panic!("expected native request")
        };
        panel.record_speech_cleanup_edit(None);
        let context = egui::Context::default();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            panel.speech_cleanup_settings(ui, &mut Vec::new());
        });
        assert_eq!(panel.speech_cleanup_draft, Some(None));
        submitted.revision += 1;
        panel.receive(
            request_id,
            Ok(NativeReply::Settings {
                revision: submitted.revision,
                document: submitted.to_toml().unwrap(),
            }),
        );
        assert_eq!(panel.speech_cleanup_draft, Some(None));
        assert!(panel.has_unsaved());
    }
}
