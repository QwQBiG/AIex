use ai_ex_config::native::VoiceSource;
use ai_ex_domain::{NativePackageState, NativeRequest};
use eframe::egui;

use super::NativePanel;
use crate::ui::theme;

pub(super) struct VoiceImport {
    request: NativeRequest,
    previous_ids: Vec<String>,
}

impl NativePanel {
    pub(super) fn track_voice_import(
        &mut self,
        request: &NativeRequest,
        operation: &ai_ex_domain::NativeOperation,
    ) {
        if matches!(request, NativeRequest::ImportVoice { .. }) {
            self.voice_imports.insert(
                operation.id,
                VoiceImport {
                    request: request.clone(),
                    previous_ids: self
                        .config
                        .as_ref()
                        .map(|config| config.voices.iter().map(|voice| voice.id.clone()).collect())
                        .unwrap_or_default(),
                },
            );
        }
        if operation.phase.is_terminal()
            && let Some(import) = self.voice_imports.remove(&operation.id)
            && operation.phase == ai_ex_domain::NativeOperationPhase::Completed
        {
            self.completed_voice_import = Some(import);
        }
    }

    pub(super) fn confirm_voice_import(&mut self, config: &ai_ex_config::native::NativeConfig) {
        let Some(import) = &self.completed_voice_import else {
            return;
        };
        let NativeRequest::ImportVoice {
            name,
            path,
            text,
            language,
            ..
        } = &import.request
        else {
            return;
        };
        let Some(voice) = config.voices.iter().find(|voice| !import.previous_ids.contains(&voice.id)
            && voice.label == *name && voice.language.as_str() == language
            && matches!(&voice.source,VoiceSource::ReferenceAudio {text: original,..} if original==text)) else {
            self.refresh = true;
            return;
        };
        self.selected_voice = voice.id.clone();
        if self.voice_name.trim() == name
            && self.reference_path.trim().trim_matches('"') == path
            && self.reference_text.trim() == text
            && self.reference_english == (language == "en")
        {
            self.reference_path.clear();
            self.reference_text.clear();
        }
        self.completed_voice_import = None;
    }

    pub(super) fn device_settings(&mut self, ui: &mut egui::Ui, requests: &mut Vec<NativeRequest>) {
        let Some(config) = self.config.clone() else {
            ui.label("先创建本地设置，再选择语音开关。");
            return;
        };
        let mut devices = self.device_draft.unwrap_or(config.devices);
        theme::card().show(ui, |ui| {
            ui.strong("按你的习惯开启语音");
            ui.label("启用后，对话会使用相应设备。默认可以只用文字聊天。");
            ui.weak("麦克风开关重新进入对话后生效；对话声音从下一句话起使用新设置。");
            if ui
                .checkbox(&mut devices.microphone_enabled, "启用麦克风，进行语音输入")
                .changed()
                | ui.checkbox(&mut devices.playback_enabled, "启用扬声器，朗读对话回复")
                    .changed()
            {
                self.device_draft = Some(devices);
            }
            if self.device_draft.is_some() {
                ui.weak("开关尚未保存。");
            }
            if ui
                .add_enabled(
                    self.device_draft.is_some() && !self.has_pending() && !self.uncertain,
                    egui::Button::new("应用语音开关"),
                )
                .clicked()
            {
                let mut next = config.clone();
                next.devices = devices;
                match next.to_toml() {
                    Ok(document) => requests.push(NativeRequest::SaveSettings {
                        expected_revision: config.revision,
                        document,
                    }),
                    Err(error) => self.status = error.to_string(),
                }
            }
        });
    }

    pub(super) fn voice_choices(&mut self, ui: &mut egui::Ui, requests: &mut Vec<NativeRequest>) {
        self.package(ui, "kokoro-v1.1-zh-en", requests);
        theme::card().show(ui, |ui| {
            ui.strong("现有声音");
            if let Some(config) = &self.config {
                for voice in &config.voices {
                    ui.radio_value(
                        &mut self.selected_voice,
                        voice.id.clone(),
                        format!(
                            "{} · {} · {}",
                            voice.label,
                            voice.language.as_str(),
                            match voice.source {
                                VoiceSource::Preset { .. } => "内置音色",
                                VoiceSource::ReferenceAudio { .. } => "专属音色",
                            }
                        ),
                    );
                }
            } else {
                ui.label("创建设置后会提供默认中文声音。");
            }
        });
        ui.collapsing("用参考音频制作专属声音", |ui| {
            self.package(ui, "qwen3-tts-base-0.6b", requests);
            ui.label("选择你拥有使用权的清晰 WAV 录音，并填写录音中实际说出的文字。");
            ui.weak("录音由本地服务导入私人声音目录，人物分享包不会包含录音或对应原文。");
            ui.label("声音名称");
            ui.add(egui::TextEdit::singleline(&mut self.voice_name).char_limit(64));
            ui.label("参考 WAV 路径");
            ui.add(
                egui::TextEdit::singleline(&mut self.reference_path)
                    .desired_width(f32::INFINITY)
                    .char_limit(1024),
            );
            ui.label("录音对应文字");
            ui.add(
                egui::TextEdit::multiline(&mut self.reference_text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .char_limit(16_000),
            );
            ui.checkbox(&mut self.reference_english, "英文录音（默认中文）");
            let ready = self.config.is_some()
                && !self.reference_path.trim().is_empty()
                && !self.reference_text.trim().is_empty()
                && !self.voice_name.trim().is_empty()
                && !self.has_pending()
                && self.voice_imports.is_empty()
                && self.completed_voice_import.is_none()
                && !self.uncertain;
            if ui
                .add_enabled(ready, egui::Button::new("导入并保存专属音色"))
                .clicked()
                && let Some(config) = &self.config
            {
                requests.push(NativeRequest::ImportVoice {
                    name: self.voice_name.trim().into(),
                    path: self.reference_path.trim().trim_matches('"').into(),
                    text: self.reference_text.trim().into(),
                    language: if self.reference_english { "en" } else { "zh" }.into(),
                    expected_revision: config.revision,
                });
            }
        });
    }

    pub(super) fn preview_and_bind(
        &mut self,
        ui: &mut egui::Ui,
        profile_id: &str,
        requests: &mut Vec<NativeRequest>,
    ) {
        theme::card().show(ui, |ui| {
            let voice = self.config.as_ref().and_then(|config| {
                config
                    .voices
                    .iter()
                    .find(|voice| voice.id == self.selected_voice)
            });
            let voice_name = voice.map_or("尚未选择声音", |voice| voice.label.as_str());
            ui.strong(format!("试听：{voice_name}"));
            let ready = voice.is_some_and(|voice| self.ready(&voice.model.model_id));
            let playback = self
                .config
                .as_ref()
                .is_some_and(|config| config.devices.playback_enabled);
            if !ready {
                ui.label("先准备所选声音的模型与运行组件，再进行试听。");
            }
            ui.add(
                egui::TextEdit::multiline(&mut self.preview_text)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .char_limit(1024),
            );
            ui.weak("点击试听将使用扬声器。试听时可随时停止。");
            if !playback {
                ui.label("先启用对话声音。保存后可试听，也会朗读后续回复；保存不会自动播放。");
                if ui
                    .add_enabled(
                        self.config.is_some() && !self.has_pending() && !self.uncertain,
                        egui::Button::new("启用对话声音"),
                    )
                    .clicked()
                    && let Some(mut config) = self.config.clone()
                {
                    config.devices.playback_enabled = true;
                    match config.to_toml() {
                        Ok(document) => requests.push(NativeRequest::SaveSettings {
                            expected_revision: config.revision,
                            document,
                        }),
                        Err(error) => self.status = error.to_string(),
                    }
                }
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        ready
                            && playback
                            && !self.uncertain
                            && !self.has_pending()
                            && self.preview_operation.is_none()
                            && !self
                                .state
                                .as_ref()
                                .is_some_and(|state| state.preview_active)
                            && !self.preview_text.trim().is_empty(),
                        egui::Button::new("播放试听"),
                    )
                    .clicked()
                    && let Some(state) = &self.state
                {
                    requests.push(NativeRequest::PreviewVoice {
                        voice_id: self.selected_voice.clone(),
                        text: self.preview_text.trim().into(),
                        expected_generation: state.preview_generation,
                    });
                }
                if ui.button("停止试听").clicked() {
                    requests.push(NativeRequest::StopPreview);
                }
                if ui
                    .add_enabled(
                        self.config.is_some()
                            && !self.selected_voice.is_empty()
                            && !self.has_pending()
                            && !self.uncertain,
                        theme::primary_button("保存声音到当前人物"),
                    )
                    .clicked()
                    && let Some(config) = &self.config
                {
                    requests.push(NativeRequest::BindVoice {
                        profile_id: profile_id.into(),
                        voice_id: self.selected_voice.clone(),
                        expected_revision: config.revision,
                    });
                }
            });
            if self.preview_operation.is_some() {
                ui.weak("正在准备或播放试听，可点击停止。");
            }
            if let Some(config) = &self.config
                && let Some(voice) = config.voice_for_profile(profile_id)
            {
                ui.weak(format!("当前人物已保存：{}", voice.label));
            }
        });
        ui.label("准备好对话模型后，可以切到“聊天”开始交流；其他组件可随后继续准备。");
        if self.microphone_restart_required() {
            ui.weak("麦克风开关重新进入对话后生效；自有本地服务会在进入聊天时重新打开。");
        }
        if ui
            .add_enabled(
                self.config.is_some()
                    && self.ready("qwen3-4b")
                    && !self.has_pending()
                    && !self.uncertain,
                theme::primary_button("进入聊天"),
            )
            .clicked()
        {
            self.open_chat = true;
        }
    }

    fn ready(&self, id: &str) -> bool {
        self.packages
            .iter()
            .find(|package| package.id == id)
            .is_some_and(|package| {
                package.installed
                    && matches!(
                        package.state,
                        NativePackageState::Ready | NativePackageState::InUse
                    )
                    && package.required_packages.iter().all(|required| {
                        self.packages.iter().any(|runtime| {
                            runtime.id == *required
                                && runtime.installed
                                && matches!(
                                    runtime.state,
                                    NativePackageState::Ready | NativePackageState::InUse
                                )
                        })
                    })
            })
    }
}
