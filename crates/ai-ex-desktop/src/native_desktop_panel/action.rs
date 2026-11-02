use ai_ex_domain::{NativeDesktopAction, NativePointerButton, NativeWindow};
use eframe::egui;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Kind {
    #[default]
    Click,
    Text,
    Keys,
    Drag,
    Scroll,
    Wait,
    Noop,
}

pub(super) struct ActionDraft {
    kind: Kind,
    x: i32,
    y: i32,
    to_x: i32,
    to_y: i32,
    button: NativePointerButton,
    text: String,
    keys: String,
    horizontal: bool,
    delta: i32,
    milliseconds: u32,
    steps: u16,
}

impl Default for ActionDraft {
    fn default() -> Self {
        Self {
            kind: Kind::Click,
            x: 0,
            y: 0,
            to_x: 0,
            to_y: 0,
            button: NativePointerButton::Left,
            text: String::new(),
            keys: "Ctrl+A".into(),
            horizontal: false,
            delta: -120,
            milliseconds: 300,
            steps: 12,
        }
    }
}

impl ActionDraft {
    pub(super) fn centered(window: &NativeWindow) -> Self {
        let x = window
            .left
            .saturating_add((window.width / 2).min(i32::MAX as u32) as i32);
        let y = window
            .top
            .saturating_add((window.height / 2).min(i32::MAX as u32) as i32);
        Self {
            x,
            y,
            to_x: window
                .left
                .saturating_add((window.width.saturating_mul(3) / 4).min(i32::MAX as u32) as i32),
            to_y: y,
            ..Default::default()
        }
    }

    pub(super) fn from_action(action: &NativeDesktopAction) -> Self {
        let mut draft = Self::default();
        match action {
            NativeDesktopAction::Click { x, y, button } => {
                draft.x = *x;
                draft.y = *y;
                draft.button = *button;
            }
            NativeDesktopAction::TypeText { text } => {
                draft.kind = Kind::Text;
                draft.text = text.clone();
            }
            NativeDesktopAction::KeyChord { keys } => {
                draft.kind = Kind::Keys;
                draft.keys = keys.join("+");
            }
            NativeDesktopAction::Drag {
                from_x,
                from_y,
                to_x,
                to_y,
                button,
                duration_ms,
                steps,
            } => {
                draft.kind = Kind::Drag;
                draft.x = *from_x;
                draft.y = *from_y;
                draft.to_x = *to_x;
                draft.to_y = *to_y;
                draft.button = *button;
                draft.milliseconds = *duration_ms;
                draft.steps = *steps;
            }
            NativeDesktopAction::Scroll { horizontal, delta } => {
                draft.kind = Kind::Scroll;
                draft.horizontal = *horizontal;
                draft.delta = *delta;
            }
            NativeDesktopAction::Wait { milliseconds } => {
                draft.kind = Kind::Wait;
                draft.milliseconds = *milliseconds;
            }
            NativeDesktopAction::Noop => {
                draft.kind = Kind::Noop;
            }
        }
        draft
    }

    pub(super) fn action(&self) -> Option<NativeDesktopAction> {
        let action = match self.kind {
            Kind::Click => NativeDesktopAction::Click {
                x: self.x,
                y: self.y,
                button: self.button,
            },
            Kind::Text => NativeDesktopAction::TypeText {
                text: self.text.clone(),
            },
            Kind::Keys => NativeDesktopAction::KeyChord {
                keys: self
                    .keys
                    .split('+')
                    .map(|key| key.trim().to_owned())
                    .collect(),
            },
            Kind::Drag => NativeDesktopAction::Drag {
                from_x: self.x,
                from_y: self.y,
                to_x: self.to_x,
                to_y: self.to_y,
                button: self.button,
                steps: self.steps,
                duration_ms: self.milliseconds,
            },
            Kind::Scroll => NativeDesktopAction::Scroll {
                horizontal: self.horizontal,
                delta: self.delta,
            },
            Kind::Wait => NativeDesktopAction::Wait {
                milliseconds: self.milliseconds,
            },
            Kind::Noop => NativeDesktopAction::Noop,
        };
        action.validate().ok()?;
        Some(action)
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            for (kind, label) in [
                (Kind::Click, "点击"),
                (Kind::Text, "输入文字"),
                (Kind::Keys, "组合键"),
                (Kind::Drag, "拖动"),
                (Kind::Scroll, "滚动"),
                (Kind::Wait, "等待"),
                (Kind::Noop, "无动作"),
            ] {
                ui.selectable_value(&mut self.kind, kind, label);
            }
        });
        if matches!(self.kind, Kind::Click | Kind::Drag) {
            ui.weak("使用屏幕物理坐标，可为负数；先保存，再切回专用窗口执行。");
            ui.horizontal_wrapped(|ui| {
                coordinate(ui, "X ", &mut self.x);
                coordinate(ui, "Y ", &mut self.y);
                for (button, label) in [
                    (NativePointerButton::Left, "左键"),
                    (NativePointerButton::Right, "右键"),
                    (NativePointerButton::Middle, "中键"),
                ] {
                    ui.selectable_value(&mut self.button, button, label);
                }
            });
        }
        match self.kind {
            Kind::Drag => {
                ui.horizontal(|ui| {
                    coordinate(ui, "终点 X ", &mut self.to_x);
                    coordinate(ui, "终点 Y ", &mut self.to_y);
                    ui.add(
                        egui::DragValue::new(&mut self.steps)
                            .range(1..=64)
                            .prefix("步数 "),
                    );
                });
            }
            Kind::Text => {
                ui.add(
                    egui::TextEdit::multiline(&mut self.text)
                        .hint_text("只在专用窗口输入的测试文字")
                        .desired_rows(2),
                );
            }
            Kind::Keys => {
                ui.text_edit_singleline(&mut self.keys);
                ui.weak("例如 Ctrl+A、Enter、Shift+Left。每次执行后释放本适配器按下的键。");
            }
            Kind::Scroll => {
                ui.checkbox(&mut self.horizontal, "水平滚动");
                ui.add(
                    egui::DragValue::new(&mut self.delta)
                        .range(-12000..=12000)
                        .prefix("滚动量 "),
                );
            }
            _ => {}
        }
        if matches!(self.kind, Kind::Drag | Kind::Wait) {
            ui.add(
                egui::DragValue::new(&mut self.milliseconds)
                    .range(0..=5000)
                    .suffix("毫秒"),
            );
        }
        if self.action().is_none() {
            ui.weak("请补全动作参数；空文字、空按键和零滚动量不能保存。");
        }
    }
}

fn coordinate(ui: &mut egui::Ui, label: &str, value: &mut i32) {
    ui.add(egui::DragValue::new(value).speed(1).prefix(label));
}
