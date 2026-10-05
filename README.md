# AIex

AIex 是一个 Windows 优先、Rust-first 的本地 AI VTuber 运行时。新架构负责本地 LLM 流式对话、文本分句、持久记忆、语音队列、VTube Studio 控制和可观察性。

旧 Python 实现位于 `main.py` 与 `src/`，当前仅作为行为参考；不再向旧实现增加新功能。新功能、修复和性能优化全部进入 Cargo workspace。

## 当前状态

| 能力 | Rust 状态 | 说明 |
| --- | --- | --- |
| 配置 | 可用 | TOML、默认值、校验、异步读取 |
| 对话核心 | 可用 | 显式状态机、运行时 actor、排队、流式打断、结构化事件 |
| 模型后端 | 可用 | DeepSeek V4 SSE、KoboldCpp SSE、Ollama NDJSON，均支持超时和取消 |
| VTube Studio | 可用 | WebSocket 认证、嘴型参数、响应情绪到显式热键 ID 的映射 |
| 文本处理 | 可用 | UTF-8 分句、Markdown/TTS 清理 |
| 持久记忆 | 可用 | JSONL 持久化、相关性检索、上下文注入 |
| 语音调度 | 可用 | 有界队列、背压、代际取消、当前播放停止令牌 |
| 音频合成/播放 | 条件可用 | GPT-SoVITS 与 Rodio 实现完成；本机播放由 feature 控制 |
| 全双工 ASR/VAD | 条件可用 | Rust VAD、HTTP Whisper、抢话和采集实现完成；原生采集由 feature 控制 |
| 桌面 UI | 实现中 | UI reducer 与控制客户端已验证；独立 eframe 壳待依赖下载后编译 |
| 视觉自动化 | 安全核心可用 | 视觉观察、能力许可和持久审计已完成；Windows 动作适配器尚未启用 |

## 快速开始

### 小白用户：双击打开可视化向导

在 Windows 资源管理器中双击仓库根目录的 `AIex-Desktop.cmd` 即可打开桌面端；如果已经构建了 `crates/ai-ex-desktop/target/release`/`debug` 二进制，它会直接启动，否则才回退到 Cargo 开发启动。首次打开会进入可视化设置向导，不需要先手写 TOML 或控制令牌。

开发者需要同时查看结构化诊断时，双击 `AIex-Desktop-Developer.cmd`；服务原始 stdout/stderr 仍保留在启动终端。

向导会选择 DeepSeek、KoboldCpp 或 Ollama，生成本地配置和控制令牌，也可以配置 Bilibili 房间号。勾选自动启动后，服务会在后台运行；普通界面显示连接、对话和急停状态。

### 开发者：同时看可视化状态和原始日志

~~~powershell
cargo run --manifest-path "crates/ai-ex-desktop/Cargo.toml" -- --developer
~~~

桌面默认显示新手控制台；点击右上角“开发者诊断”可展开结构化事件，启动它的终端仍保留服务 stdout/stderr，适合定位连接、模型和事件问题。

要求：Rust 1.85 或更高版本；推荐使用仓库已验证的 Rust 1.96。

```powershell
cargo test --workspace
cargo run -p ai-ex-service -- --check
cargo run -p ai-ex-service -- --prompt "你好"
```

Developer stage replay:

```powershell
cargo run -p ai-ex-service -- --replay-stage path/to/stage.jsonl
cargo run -p ai-ex-service -- --replay-automation config_examples/automation-replay.jsonl
```

使用只读视觉分析（需在配置中启用 `[vision]`）：

```powershell
cargo run -p ai-ex-service -- --config "config/ai-ex.local.toml" --vision-image "screen.png" --vision-prompt "描述当前界面"
```

启用 GPT-SoVITS 本机播放时使用：

```powershell
cargo run -p ai-ex-service --features native-playback -- --config "config/ai-ex.local.toml"
```

启用 Windows 原生麦克风与全双工输入时，在本机配置中设置
`duplex.enabled = true`，并使用：

```powershell
cargo run -p ai-ex-service --features native-capture -- --config "config/ai-ex.local.toml"
```

交互模式：

```powershell
cargo run -p ai-ex-service
```

默认读取 `config/ai-ex.example.toml`。本机私有配置应复制为 `config/ai-ex.local.toml` 并通过参数指定：

```powershell
cargo run -p ai-ex-service -- --config "config/ai-ex.local.toml" --check
```

`config/ai-ex.local.toml`、`token.json`、运行日志和构建产物已被 `.gitignore` 排除。

`[conversation]` 可设置系统提示、保留的历史轮数和每轮记忆召回上限；默认历史窗口为 12 轮，避免长期运行时上下文无限增长。

模型可在响应开头返回 `[neutral]`、`[happy]`、`[angry]`、`[sad]` 或 `[surprised]`。标签会转为事件而不会进入语音、字幕或记忆；只有 `[vts.expression_hotkeys]` 中明确配置的映射才会触发 VTS。

从旧 `config.json` 生成新的本机配置（目标已存在时拒绝覆盖）：

```powershell
cargo run -p ai-ex-migrate -- --input "config.json" --output "config/ai-ex.local.toml"
```

迁移器不会自动启用旧 Agent、视觉、全双工或控制端口；这些高风险能力必须在新配置中显式复核后开启。

## 外部服务

- DeepSeek V4：官方 API 默认 `https://api.deepseek.com`；密钥从 `DEEPSEEK_API_KEY` 读取，示例见 `docs/MODEL_BACKENDS.md`。`deepseek-v4-flash` 适合先测，`deepseek-v4-pro` 可按需切换。
- Ollama：默认 `http://127.0.0.1:11434`，适配仍保留。
- KoboldCpp：可选后端，默认 `http://127.0.0.1:5001`；设置 `model.backend = "koboldcpp"` 启用。
- VTube Studio：默认 `ws://127.0.0.1:8001`，令牌文件结构为 `{ "token": "..." }`。
- GPT-SoVITS：默认 `http://127.0.0.1:9880`，默认关闭。
- Whisper 兼容 ASR：默认 `http://127.0.0.1:8000/v1/audio/transcriptions`，默认关闭全双工。
- VTS 不可用时，服务降级为无头像输出；`--check` 会将其报告为 unavailable。
- Ollama 不可用时无法完成对话，服务会返回结构化连接错误。

## Rust 工作空间

```text
crates/
  ai-ex-domain/   领域类型、错误和事件
  ai-ex-config/   TOML 配置与校验
  ai-ex-text/     分句和 TTS 文本清理
  ai-ex-core/     对话状态机、端口和异步编排
  ai-ex-deepseek/ DeepSeek V4 SSE 流式 HTTP 适配器
  ai-ex-ollama/   Ollama 流式 HTTP 适配器
  ai-ex-koboldcpp/ KoboldCpp 流式 SSE 适配器
  ai-ex-vts/      VTube Studio WebSocket actor
  ai-ex-stage/     舞台动作协议、能力声明和 dry-run 执行器
  ai-ex-stage-obs/ OBS 字幕/场景/热键 dry-run 适配器与 JSONL 录制
  ai-ex-audio/    有界语音队列与取消
  ai-ex-tts/      GPT-SoVITS HTTP 适配器
  ai-ex-memory/   Rust 原生持久记忆
  ai-ex-duplex/   VAD、音频/ASR 端口与全双工指令
  ai-ex-asr/      Whisper 兼容 HTTP 转写与 WAV 编码
  ai-ex-capture/  Windows 原生麦克风输入（feature）
  ai-ex-observability/ 事件广播与运行快照
  ai-ex-safety/   能力白名单、目标范围和急停许可
  ai-ex-control/  令牌认证的本地 JSONL 控制协议
  ai-ex-ui-model/ 框架无关的 UI reducer 与断线补偿
  ai-ex-vision/   只读视觉观察与 Ollama 多模态适配器
  ai-ex-automation/ 动作验证、许可、执行阶段与重试语义
  ai-ex-audit/    启动校验、同步落盘的 JSONL 审计日志
  ai-ex-migrate/  旧 JSON 到新 TOML 的安全迁移器
  ai-ex-service/  CLI、组合根和健康检查
```

`crates/ai-ex-desktop/` 是独立的 eframe 原生桌面包。它保持在默认 workspace 之外，保证核心可以离线验证；桌面端有自己的锁文件和独立构建命令。

依赖方向固定为：`domain/text/duplex contracts → core → adapters → service`。网络、设备、数据库和 UI 不得反向进入领域层。


不要运行会把控制流左花括号移回同一行的自动格式化。详细规则见 `CONTRIBUTING.md`。

## 验证

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pwsh -NoProfile -File "tools/check_rust_style.ps1"
pwsh -NoProfile -File "tools/check_architecture.ps1"
```

健康检查在外部服务未启动时返回退出码 1，这是预期行为，不代表配置或 Rust 二进制构建失败。

## 迁移原则

1. 不翻译旧类结构，按领域、端口和 actor 重建。
2. 每个外部能力都有超时、健康状态、取消和降级路径。
3. 自动化默认关闭，急停与授权优先于功能数量。
4. 迁移完成并通过行为验收后，才删除对应 Python 模块。

详细评估和阶段计划见 `docs/CORE_TECHNICAL_BASELINE.md`。
Python 退役批次和删除门禁见 `docs/LEGACY_RETIREMENT.md`。

交互模式支持 `/status`、`/interrupt`、`/emergency-stop` 和 `/quit`。
急停一旦触发，只能通过重启服务清除。

桌面客户端通过本地控制端口接入。首次设置向导会自动创建至少 32 字节的 `config/control.token` 并启用控制端；高级手动配置时仍需自行确认 `control.enabled = true`。协议定义见
`docs/CONTROL_PROTOCOL.md`；服务拒绝任何非回环监听地址。

桌面端首次启动会自动打开可视化向导，适合不熟悉命令行的用户：

```powershell
cargo run --manifest-path "crates/ai-ex-desktop/Cargo.toml"
```

向导可选择 DeepSeek、KoboldCpp 或 Ollama，自动生成本地配置和控制令牌，并可自动启动服务。开发者使用 `--developer` 查看桌面控制日志；服务原始 stdout/stderr 仍保留在终端。完整说明见 [`docs/DESKTOP_USER_GUIDE.md`](docs/DESKTOP_USER_GUIDE.md)。
