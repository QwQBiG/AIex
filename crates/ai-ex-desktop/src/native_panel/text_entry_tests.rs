use super::*;
use ai_ex_domain::{NativePackage, NativePackageKind, NativePackageState, NativeState};

fn ready_panel() -> NativePanel {
    let packages = ["native-cpu", "qwen3-4b"]
        .into_iter()
        .map(|id| NativePackage {
            id: id.into(),
            name: id.into(),
            revision: "test".into(),
            kind: if id == "native-cpu" {
                NativePackageKind::Runtime
            } else {
                NativePackageKind::Model
            },
            state: NativePackageState::Ready,
            bytes: 1,
            installed: true,
            active_operation: None,
            error: None,
            required_packages: if id == "qwen3-4b" {
                vec!["native-cpu".into()]
            } else {
                Vec::new()
            },
            licenses: Vec::new(),
        })
        .collect();
    NativePanel {
        state: Some(NativeState {
            configured: true,
            configuration_path: "synthetic.toml".into(),
            preview_active: false,
            preview_generation: 0,
        }),
        config: Some(ai_ex_config::native::NativeConfig::default()),
        packages,
        ..Default::default()
    }
}

fn draw(
    panel: &mut NativePanel,
    context: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    context.run_ui(
        egui::RawInput {
            events,
            ..Default::default()
        },
        |ui| {
            panel.text_chat_entry(ui);
        },
    )
}

fn position(output: &egui::FullOutput, label: &str) -> egui::Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing label: {label}"))
}

fn click(panel: &mut NativePanel, context: &egui::Context) {
    let output = draw(panel, context, Vec::new());
    let position = position(&output, "进入聊天");
    for pressed in [true, false] {
        draw(
            panel,
            context,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ],
        );
    }
}

#[test]
fn ready_text_chat_opens_without_changing_audio_settings() {
    let mut panel = ready_panel();
    let before = panel.config.as_ref().unwrap().devices;
    let context = egui::Context::default();
    click(&mut panel, &context);
    assert!(panel.take_open_chat());
    assert_eq!(panel.config.as_ref().unwrap().devices, before);
    assert!(panel.pending.is_empty());
}

#[test]
fn ready_chat_keeps_existing_voice_settings_and_uses_selected_model() {
    let mut panel = ready_panel();
    let config = panel.config.as_mut().unwrap();
    config.devices.microphone_enabled = true;
    config.devices.playback_enabled = true;
    config.dialogue.as_mut().unwrap().model_id = "alternate".into();
    let mut alternate = panel.packages[1].clone();
    alternate.id = "alternate".into();
    panel.packages[1].state = NativePackageState::NotPrepared;
    panel.packages.push(alternate);
    let context = egui::Context::default();
    let output = draw(&mut panel, &context, Vec::new());
    position(
        &output,
        "已保存的语音设置中有设备开启，进入聊天不会自动关闭。",
    );
    click(&mut panel, &context);
    assert!(panel.take_open_chat());
    let devices = panel.config.as_ref().unwrap().devices;
    assert!(devices.microphone_enabled && devices.playback_enabled);
    assert!(panel.pending.is_empty());
}

#[test]
fn text_chat_requires_settings_and_ready_dependencies() {
    for (case, expected) in [
        (0, "先创建本地设置。"),
        (1, "正在读取本地设置。"),
        (2, "操作结果待核对，请先刷新状态。"),
        (3, "请等待当前操作完成。"),
        (4, "请先准备当前对话模型及其运行组件。"),
        (5, "正在读取本地设置。"),
        (6, "当前设置未启用本地对话，请先调整本地设置。"),
        (7, "请先准备当前对话模型及其运行组件。"),
        (8, "请先准备当前对话模型及其运行组件。"),
    ] {
        let mut panel = ready_panel();
        match case {
            0 => panel.state.as_mut().unwrap().configured = false,
            1 => panel.config = None,
            2 => panel.uncertain = true,
            3 => {
                panel.begin(NativeRequest::CreateDefaults).unwrap();
            }
            4 => panel.packages[0].state = NativePackageState::NotPrepared,
            5 => panel.state = None,
            6 => panel.config.as_mut().unwrap().dialogue = None,
            7 => {
                panel
                    .config
                    .as_mut()
                    .unwrap()
                    .dialogue
                    .as_mut()
                    .unwrap()
                    .model_id = "different-model".into()
            }
            _ => {
                panel
                    .config
                    .as_mut()
                    .unwrap()
                    .dialogue
                    .as_mut()
                    .unwrap()
                    .runtime_id = "different-runtime".into()
            }
        }
        let context = egui::Context::default();
        let output = draw(&mut panel, &context, Vec::new());
        position(&output, expected);
        click(&mut panel, &context);
        assert!(!panel.take_open_chat(), "case {case}");
    }
}
