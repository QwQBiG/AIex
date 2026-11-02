use ai_ex_domain::{NativePackageState as S, NativeRequest};
use eframe::egui;

use super::NativePanel;
use crate::ui::theme;

impl NativePanel {
    pub(super) fn contents(
        &mut self,
        ui: &mut egui::Ui,
        profile_id: &str,
        requests: &mut Vec<NativeRequest>,
    ) {
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for (index, title) in [
                "1 · 本地对话",
                "2 · 语音开关",
                "3 · 选择声音",
                "4 · 试听与保存",
            ]
            .iter()
            .enumerate()
            {
                ui.selectable_value(&mut self.step, index, *title);
            }
            if ui.small_button("刷新状态").clicked() {
                self.refresh = true;
            }
        });
        if !self.status.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("native_status")
                .max_height(140.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(&self.status).selectable(true));
                });
        }
        if let Some(state) = &self.state {
            if !state.configured {
                theme::card().show(ui, |ui| {
                    ui.strong("开始你的本地设置");
                    ui.label("模型与声音设置会保存在独立文件中。原有连接设置继续可用。");
                    if ui
                        .add_enabled(!self.has_pending(), theme::primary_button("创建本地设置"))
                        .clicked()
                    {
                        requests.push(NativeRequest::CreateDefaults);
                    }
                });
            } else {
                ui.weak(format!("设置：{}", state.configuration_path));
            }
        }
        ui.add_space(12.0);
        ui.push_id(("native_step", self.step), |ui| match self.step {
            0 => {
                ui.label("先准备运行组件和对话模型。下载后也可以在断网时使用。");
                self.package(ui, "native-cpu", requests);
                self.package(ui, "qwen3-4b", requests);
                ui.collapsing("同时准备本地记忆检索", |ui| {
                    self.package(ui, "bge-small-zh-v1.5", requests)
                });
            }
            1 => {
                self.device_settings(ui, requests);
                ui.label("需要语音输入时准备识别模型：");
                self.package(ui, "whisper-small-int8", requests);
            }
            2 => self.voice_choices(ui, requests),
            _ => self.preview_and_bind(ui, profile_id, requests),
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.step > 0, egui::Button::new("上一步"))
                .clicked()
            {
                self.step -= 1;
            }
            if ui
                .add_enabled(self.step < 3, theme::primary_button("下一步"))
                .clicked()
            {
                self.step += 1;
            }
        });
    }

    pub(super) fn package(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        requests: &mut Vec<NativeRequest>,
    ) {
        let Some(package) = self
            .packages
            .iter()
            .find(|package| package.id == id)
            .cloned()
        else {
            ui.weak(format!("正在读取组件信息：{id}"));
            return;
        };
        ui.push_id(("native_package", id), |ui| {
        theme::card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong(&package.name);
                ui.label(format!(
                    "文件合计 {:.1} MiB · {}",
                    package.bytes as f64 / 1_048_576.0,
                    label(package.state)
                ));
            });
            if !package.required_packages.is_empty() {
                ui.weak(format!(
                    "需要运行组件：{}",
                    package.required_packages.iter().map(|id| self.packages.iter()
                        .find(|item| item.id == *id).map_or(id.as_str(), |item| item.name.as_str()))
                        .collect::<Vec<_>>().join("、")
                ));
            }
            ui.collapsing("许可与来源", |ui| {
                ui.weak(format!("固定版本：{}", package.revision));
                for license in &package.licenses {
                    ui.hyperlink_to(&license.id, &license.source_url);
                }
            });
            if let Some(error) = &package.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            if let Some(id) = package.active_operation.or_else(|| {
                self.operations
                    .values()
                    .find(|operation| operation.package_id == package.id)
                    .map(|operation| operation.id)
            }) {
                if let Some(operation) = self.operations.get(&id) {
                    let progress = if operation.total_bytes == 0 {
                        0.0
                    } else {
                        operation.completed_bytes as f32 / operation.total_bytes as f32
                    };
                    ui.add(egui::ProgressBar::new(progress.clamp(0.0, 1.0)).show_percentage());
                } else {
                    ui.spinner();
                }
                if ui.button("取消准备").clicked() {
                    requests.push(NativeRequest::Cancel { id });
                }
            } else {
                ui.add_enabled_ui(!self.uncertain && !self.has_pending(), |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button(if package.installed {
                                "重新准备"
                            } else {
                                "下载准备"
                            })
                            .clicked()
                        {
                            self.download_confirmation = Some(package.id.clone());
                        }
                        if package.installed && ui.button("移除这项组件").clicked() {
                            self.remove_confirmation = Some(package.id.clone());
                        }
                    });
                    let path = self.import_paths.entry(package.id.clone()).or_default();
                    ui.add(
                        egui::TextEdit::singleline(path)
                            .hint_text("离线目录或 ZIP 文件路径")
                            .desired_width(f32::INFINITY)
                            .char_limit(1024),
                    );
                    if ui
                        .add_enabled(!path.trim().is_empty(), egui::Button::new("从本地导入"))
                        .clicked()
                    {
                        requests.push(NativeRequest::Import {
                            package_id: package.id.clone(),
                                path: path.trim().trim_matches('"').into(),
                        });
                    }
                });
            }
            if self.download_confirmation.as_deref() == Some(&package.id) {
                ui.separator();
                ui.label(format!(
                    "组件文件合计约 {:.1} MiB，下载与校验还需要临时空间。确认以上许可后，将从固定来源下载。",
                    package.bytes as f64 / 1_048_576.0
                ));
                ui.horizontal(|ui| {
                    if ui.button("确认下载").clicked() {
                        requests.push(NativeRequest::Download {
                            package_id: package.id.clone(),
                        });
                        self.download_confirmation = None;
                    }
                    if ui.button("暂不下载").clicked() {
                        self.download_confirmation = None;
                    }
                });
            }
            if self.remove_confirmation.as_deref() == Some(&package.id) {
                ui.label("移除后，使用这项组件的能力会暂时停止，可随时重新准备。");
                ui.horizontal(|ui| {
                    if ui.button("确认移除").clicked() {
                        requests.push(NativeRequest::Remove {
                            package_id: package.id.clone(),
                        });
                        self.remove_confirmation = None;
                    }
                    if ui.button("保留").clicked() {
                        self.remove_confirmation = None;
                    }
                });
            }
        });
        });
    }
}

fn label(state: S) -> &'static str {
    match state {
        S::NotPrepared => "未准备",
        S::Downloading => "下载中",
        S::Verifying => "校验中",
        S::Ready => "已就绪",
        S::InUse => "使用中",
        S::Error => "准备失败",
    }
}
