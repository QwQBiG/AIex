use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use ai_ex_domain::AppError;
use eframe::egui;

use crate::image_appearance::{DecodedAppearance, ImageAppearance};

#[cfg(test)]
#[path = "appearance_import_tests.rs"]
mod tests;

#[derive(Default)]
pub struct AppearanceImport {
    pub current: Option<ImageAppearance>,
    pub input: String,
    pub error: Option<String>,
    source: String,
    restore: bool,
    activate: bool,
    drop_notice: bool,
    pending: Option<Receiver<Result<DecodedAppearance, AppError>>>,
}

impl AppearanceImport {
    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let source = storage
            .and_then(|storage| storage.get_string("appearance.image_source"))
            .unwrap_or_default();
        Self {
            restore: !source.is_empty(),
            input: source.clone(),
            source,
            ..Default::default()
        }
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string("appearance.image_source", self.source.clone());
    }

    pub fn install(&mut self, context: &egui::Context, decoded: DecodedAppearance) {
        self.source = decoded.source.to_string_lossy().into_owned();
        self.input = self.source.clone();
        self.current = Some(ImageAppearance::upload(context, decoded));
        self.error = None;
        self.restore = false;
    }

    pub fn keep_selection(&mut self) {
        self.activate = false;
    }

    pub fn begin(&mut self, context: &egui::Context, path: PathBuf) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let context = context.clone();
        match std::thread::Builder::new()
            .name("ai-ex-appearance-import".to_owned())
            .spawn(move || {
                let result = DecodedAppearance::load(&path);
                let _ignored = sender.send(result);
                context.request_repaint();
            }) {
            Ok(_) => {
                self.pending = Some(receiver);
                self.activate = true;
                self.error = None;
                self.drop_notice = false;
            }
            Err(error) => self.error = Some(format!("无法开始导入：{error}")),
        }
    }

    pub fn poll(&mut self, context: &egui::Context) -> bool {
        if self.restore {
            self.restore = false;
            self.begin(context, PathBuf::from(&self.source));
            self.keep_selection();
        }
        let Some(pending) = &self.pending else {
            return false;
        };
        let result = match pending.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => {
                context.request_repaint_after(std::time::Duration::from_millis(100));
                return false;
            }
            Err(TryRecvError::Disconnected) => {
                Err(AppError::unavailable("appearance import worker stopped"))
            }
        };
        self.pending = None;
        match result {
            Ok(decoded) => {
                self.install(context, decoded);
                self.activate
            }
            Err(error) => {
                self.error = Some(format!("外形导入失败，保留原外形：{error}"));
                false
            }
        }
    }

    pub fn controls(&mut self, ui: &mut egui::Ui) {
        let dropped = ui.input_mut(|input| std::mem::take(&mut input.raw.dropped_files));
        if let Some(path) = dropped.into_iter().find_map(|file| file.path) {
            if self.is_loading() {
                self.drop_notice = true;
            } else {
                self.input = path.to_string_lossy().into_owned();
                self.begin(ui.ctx(), path);
            }
        }
        ui.collapsing("导入图片外形包", |ui| {
            ui.weak("拖入外形文件夹或 appearance.toml，也可在下方填写路径。");
            ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .desired_width(f32::INFINITY)
                    .hint_text("外形清单路径"),
            );
            if ui
                .add_enabled(
                    !self.is_loading() && !self.input.trim().is_empty(),
                    egui::Button::new("导入 / 重新加载"),
                )
                .clicked()
            {
                self.begin(ui.ctx(), PathBuf::from(self.input.trim().trim_matches('"')));
            }
            ui.weak("默认图必需；说话、倾听、思考和情绪图可选。支持 PNG / JPEG。");
        });
        if self.pending.is_some() {
            ui.label("正在读取外形……");
        }
        if self.drop_notice {
            ui.label(if self.is_loading() {
                "正在读取外形，请完成后重新拖入其他文件。"
            } else {
                "上次拖入的文件未导入，请重新拖入。"
            });
        }
        if let Some(pack) = &self.current {
            ui.weak(format!(
                "外形包：{} · {} 张图片",
                pack.manifest.name,
                pack.manifest.images.len()
            ))
            .on_hover_text(pack.source.display().to_string());
        }
        if let Some(error) = &self.error {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "外形导入失败，已保留原外形（悬停查看原因）",
            )
            .on_hover_text(error);
        }
        if self.current.is_none() {
            ui.weak("尚未载入图片外形，暂时显示内置伙伴。");
        }
    }
}
