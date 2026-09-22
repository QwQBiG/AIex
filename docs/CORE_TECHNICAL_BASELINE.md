# AIex 核心技术基线与 Rust 迁移蓝图

> 历史基线：2026-08-10。本文保留当时的 Python 静态审视、迁移设计与环境记录。文中的 Git 不可用、桌面未编译等结论仅对应当时状态；当前实现、目录和验证入口分别见[架构说明](ARCHITECTURE.md)、[项目目录](PROJECT_LAYOUT.md)与[整体验收流程](MANUAL_TEST_PLAN.md)。本文中的清理建议不是当前执行授权。

## 1. 目标与当前判断

AIex 是一个 Windows 优先的 AI VTuber 桌面系统，组合本地 LLM、TTS、VTube Studio、持久记忆、全双工语音和可选的视觉自动化。

当前最重要的工作不是直接把整个项目翻译成 Rust，而是先建立可验证的行为边界，清除生成物和过期材料，再以渐进替换（strangler）方式迁移性能和可靠性关键路径。这样可以保留现有配置、模型、记忆数据与外部服务兼容性，避免一次性重写导致不可验收。

## 2. 已核实的系统结构

```text
main.py
  ├─ SystemConfig / ErrorHandler / WarmupManager
  ├─ ImprovedGUIController
  │   └─ SystemWorkflow
  │       ├─ LLMClient (Ollama 流式输出)
  │       ├─ StreamProcessor / TextCleaner
  │       ├─ TTSPipeline / TTSPlayer
  │       └─ VTSClient (表情、口型)
  ├─ MemoryCore (可选；ChromaDB 持久记忆)
  ├─ AgentManager + SafetyManager (可选；视觉分析和受限桌面操作)
  └─ Full-duplex engine (可选)
      ├─ AudioDeviceManager / StreamingEars
      ├─ TextProcessor / DuplexManager
      └─ ConfigurationManager / latency + health monitoring
```

| 范围 | 主要实现 | 职责 | 迁移优先级 |
| --- | --- | --- | --- |
| 启动与配置 | `main.py`, `src/config.py` | 配置加载、生命周期、组件注入 | 高 |
| 对话主链 | `src/system_workflow.py` | LLM 流、分句、TTS、VTS 协调 | 最高 |
| UI | `src/gui_controller.py` | Tkinter 桌面界面与状态展示 | 中，后移 |
| 语音 | `src/tts_pipeline.py`, `src/full_duplex_engine/` | 播放、打断、ASR、音频设备 | 最高 |
| 外部适配 | `src/llm_client.py`, `src/vts_client.py` | Ollama HTTP 与 VTS WebSocket | 高 |
| 记忆 | `src/memory_core/` | ChromaDB、实体提取、检索 | 中，先保持兼容 |
| 自动化 Agent | `src/agent_manager.py`, `src/action_engine.py`, `src/vision_client.py` | 视觉理解、执行、紧急停止 | 中低，安全优先 |

### 关键事实

- `main.py` 会在 GUI 创建后从 GUI 取得 `SystemWorkflow`，再向其中注入记忆和全双工组件；这是目前事实上的组合根。
- 语音、记忆与 Agent 被设计为可选能力：依赖导入失败时可降级。Rust 版本必须保留这种降级和错误可见性。
- 运行时依赖大量外部进程或设备：Ollama、VTube Studio、GPT-SoVITS、音频设备、Whisper/Torch/ChromaDB。因此“全部 Rust”不等于马上重写模型推理；先替换调度、网络、音频控制和平台适配层更有价值。

## 3. 审视发现的工程问题

1. 工作目录当前不是可用的 Git 仓库（`git status` 返回“not a git repository”）。重写前必须恢复版本控制、分支和可重复构建记录。
2. `requirements.txt` 仅有下限约束，没有锁定文件；其中既包含桌面 GUI、音频、深度学习又包含测试和开发依赖，无法保证可复现环境。
3. 当前终端找不到 `python` 或 `py`，所以没有执行现有测试。仓库内虽存在 Python 3.13 编译缓存，但它不能作为可运行环境的证据。
4. 文档明显漂移：根 README 引用的变更记录和修复报告不在仓库中；`assets/README.md`、`tests/README.md`、`tools/README.md` 还描述了未找到的管理器、工具、测试与开发文档。旧报告不能再视为当前实现状态。
5. 存在多组“optimized/ultra/v4.4”并行实现，部分只被对应测试引用；它们不在当前启动主链上。未完成行为对比前不得把它们接入新主线。
6. `config.json` 含机器相关的绝对参考音频路径；运行时数据（`memory_db/`、音频缓存、模型）与源码混放。配置应拆为安全可提交样例和本机私有覆盖。
7. 历史运行日志和 Python 缓存混在工作树内；它们均匹配现有 `.gitignore` 的生成物规则，且最大日志约 156 MB。

## 4. 清理边界

本轮可以直接删除、且不会改变产品配置或记忆的对象：

- `__pycache__/`、所有 `*.pyc`：Python 编译缓存。
- `logs/`：运行日志；已由 `.gitignore` 排除。
- `test_output.txt`、`test_results.txt`、`test_full_output.log`：历史测试输出，不能替代测试结果。

本轮保留：`config.json`、`token.json`、`memory_db/`、`assets/models/`、`assets/cache/reference_audio.wav`、游戏模板和全部源代码/测试。它们分别可能包含本机密钥、用户记忆、模型、语音身份或功能资产。根目录历史报告和散落测试脚本暂不删除，待其信息被本文件吸收并在 Git 建立后再作一次可追踪归档决策。

## 5. Rust 目标架构

当前已经采用单一 Cargo workspace、六边形边界和 actor 生命周期。GUI 技术选型不阻塞内核；界面只能消费命令与事件，不得持有网络、音频或自动化对象。

```text
crates/
  ai-ex-domain/          # 稳定领域类型、错误、事件
  ai-ex-config/          # TOML、默认值、跨模块校验
  ai-ex-text/            # Unicode 分句与语音文本清洗
  ai-ex-core/            # 会话状态机、端口、可打断运行时 actor
  ai-ex-duplex/          # 音频/转写端口、VAD、抢话指令
  ai-ex-memory/          # JSONL 记忆检索与回写
  ai-ex-ollama/          # LLM 流式 HTTP 适配器
  ai-ex-koboldcpp/       # KoboldCpp SSE 流式适配器
  ai-ex-vts/             # VTube Studio WebSocket actor
  ai-ex-audio/           # 语音背压、取消与播放
  ai-ex-tts/             # GPT-SoVITS 适配器
  ai-ex-asr/             # Whisper HTTP 与 PCM WAV 编码
  ai-ex-capture/         # feature-gated 原生输入设备
  ai-ex-observability/   # 事件广播、状态快照和计数
  ai-ex-safety/          # 能力许可、目标白名单和急停撤销
  ai-ex-control/         # 仅回环、令牌认证的 JSONL 控制面
  ai-ex-ui-model/        # UI reducer、会话视图和断线补偿
  ai-ex-vision/          # 只读视觉观察与 Ollama 多模态客户端
  ai-ex-automation/      # 安全动作协调、执行阶段与重试语义
  ai-ex-audit/           # 同步持久化、启动校验的审计日志
  ai-ex-migrate/         # 旧 JSON 到新 TOML 的安全迁移器
  ai-ex-service/         # 唯一组合根、CLI、健康检查
```

`ai-ex-desktop` 是使用 eframe/egui 的独立原生桌面包。当前因外部 GUI 依赖尚未下载而从默认 workspace 排除；这保证核心仍能离线验证，也明确禁止用未编译的界面代码冒充完成状态。

### 不可破坏的架构约束

1. `domain`、`text` 与端口契约不依赖网络、设备、数据库或 UI。
2. 一次对话只由运行时 actor 所有；提交可以排队，打断与关闭必须抢占流式生成。
3. 用户开口只产生“打断当前轮次”，语音结束才产生“提交转写”；VAD 不知道 LLM，ASR 不知道会话状态。
4. 外部调用必须有超时、结构化错误与显式降级；不得吞掉失败伪装成功。
5. 自动化默认关闭，并在实现任何动作前完成权限、范围限制、审计和急停设计。

## 6. 分阶段迁移计划

| 阶段 | 交付物 | 验收标准 |
| --- | --- | --- |
| 0：建立基线 | 配置样例、核心文档、清理边界 | 已完成；Git 元数据仍不可用 |
| 1：无副作用核心 | 配置、文本、领域事件与状态机 | 已完成并有单元测试 |
| 2：网络与记忆 | Ollama、KoboldCpp、VTS、TTS、JSONL 记忆 | 已完成默认构建与协议边界 |
| 3：语音控制 | 队列、取消、播放、VAD、ASR、采集 | 逻辑完成；原生 feature 仍需依赖下载后实机验收 |
| 4：编排服务 | 运行时 actor、抢占、排队、健康检查 | 已完成；需补端到端延迟指标 |
| 5：高风险能力 | 视觉、受限自动化、安全审计 | 只读视觉、安全协调和持久审计完成；平台动作适配器未启用 |
| 6：界面与发布 | 桌面 UI、安装包、可重复发布 | UI 模型与桌面壳已实现；原生依赖编译和视觉验收未完成 |

### 后续精进顺序

1. 完成 `native-capture` 与 `native-playback` feature 编译、设备枚举和实机抢话延迟验收。
2. 为 Ollama、KoboldCpp、VTS、TTS、ASR 与视觉适配器补本地假服务契约测试。
3. 完成 eframe 依赖下载、原生编译和视觉/交互验收。
4. 定义自动化审计日志，再实现视觉分析与受限动作。
5. 完成发布打包与行为验收后删除对应 Python 主链。

## 7. 接下来的执行顺序

1. 恢复有效 Git 元数据，在首次提交前审阅当前全部变更与本机私有文件。
2. 完成 Windows 音频 feature 下载和实机验证，记录 VAD、ASR、首音与打断延迟。
3. 编译并验收独立桌面壳；不允许 UI 绕过控制协议和运行时 actor。
4. 对照功能验收表逐模块冻结 Python 主链，确认无数据依赖后再列出精确删除目标。
5. 禁止新增 Python `*_optimized`、`*_ultra` 变体；所有新能力只进入 Rust workspace。

## 8. 决策记录

- 决策：采用渐进式 Rust 重构，而不是一次性重写。原因：系统高度依赖本机设备和外部 AI 服务，需要持续可运行与可回退。
- 决策：运行数据与源码分离，但本轮不删除用户数据。原因：记忆库和参考音频可能不可恢复。
- 决策：以自动化测试与协议契约取代历史优化报告作为事实依据。原因：报告无法证明当前代码仍可运行。

## 9. 从历史报告保留的产品意图

根目录旧报告曾描述多组未稳定接入的 Python “optimized/ultra/v4.4” 实现。以下内容仅作为需求候选保留，不继承其“已完成”或性能数字：

- 自然说话节奏、思考停顿、注意力变化、偶发失误与弹幕响应。
- 情绪状态到 VTS 表情/动作的映射与平滑过渡。
- 游戏场景模板、坐标偏移学习和视觉结果缓存。
- 多模态记忆、标签检索与可解释的记忆重要度。
- 连接复用、惰性初始化、热配置、端到端延迟和缓存命中率指标。

这些能力只有在进入 Rust 端口、具备测试、可观察性和安全授权后才算实现。旧报告中的自报测试结果、星级和性能提升不作为验收证据。

## 10. Rust 主线实施状态（2026-08-10）

已经完成：

- Cargo workspace、领域类型、结构化错误、TOML 配置与异步组合根。
- 可取消的对话状态机、事件分发、UTF-8 分句和 TTS 文本清理。
- 会话策略统一限制系统提示、历史窗口和记忆召回预算；中断、失败和空响应不会污染后续上下文。
- Ollama `/api/chat` NDJSON 与 KoboldCpp SSE 流式客户端，均包含超时、错误转换和任务取消。
- VTube Studio WebSocket 认证、嘴型参数和热键命令 actor；不可用时显式降级。
- 跨流分块的响应情绪标签解析、结构化情绪事件、运行快照/UI 状态和显式 VTS 热键映射。
- Rust 原生 JSONL 记忆库、相关性检索、上下文注入和对话回写。
- 有界语音队列、背压、代际取消、当前 Rodio sink 撤销、GPT-SoVITS 合成客户端。
- Rodio 本机 WAV 播放实现，置于 `native-playback` feature 下。
- 运行时 actor 可在 LLM 流式生成期间处理语音抢话、关闭和新轮次排队；打断轮次不写入记忆。
- 能量 VAD 使用启动确认、迟滞阈值、静音结束和最大语音段限制，并拒绝段内音频格式突变。
- Whisper/OpenAI 兼容 multipart ASR 客户端与 Rust 原生 PCM16 WAV 编码。
- CPAL Windows 原生麦克风采集适配器，置于 `native-capture` feature 下。
- 广播事件中心与只读运行快照；控制台和未来 UI 共享同一事件源。
- 自动化能力/目标双白名单、带理由的 permit 与可撤销急停机制。
- 本地控制协议支持提交、打断、状态与急停，具备令牌认证和消息尺寸上限。
- 事件具有单调序号和有界重放历史；UI reducer 能去重、检测缺口并限制会话视图容量。
- eframe/egui 原生桌面壳已实现连接状态、对话流、提交、打断、急停确认和中文字体回退。
- Ollama 多模态视觉客户端只返回观察文本，图片大小/签名和提示长度都有边界；服务支持按文件签名识别的独立视觉 CLI。
- 自动化执行采用“持久审计→授权→急停复核→适配器”顺序，并区分副作用前/执行中失败。
- JSONL 审计记录在执行前同步落盘，启动时逐行校验；敏感输入只记录长度。
- 配置迁移器采用原子新建、拒绝覆盖，并强制保持旧 Agent/视觉/控制/全双工关闭。
- 服务 CLI、结构化健康检查、交互模式、Clippy 严格门禁和 Allman 风格检查器。

验证证据：默认 workspace 的 22 个包共 65 项测试通过，覆盖运行时抢话/排队/完成关闭、故障恢复、会话历史回滚与上限、跨分块情绪标签、VAD、WAV、双模型配置与流协议、事件重放、安全许可、持久审计、控制协议、UI reducer、视觉边界、迁移器和服务 CLI；全 workspace 严格 Clippy 通过；Allman 检查器扫描 59 个 Rust 文件通过；架构门禁覆盖 22 个包。风格与架构的故意违规探针均按预期返回退出码 1。`--check` 已验证真实进程路径，在本机外部服务未启动时正确报告 Ollama/VTS unavailable。

尚未完成：ASR 实机契约、桌面壳依赖编译/视觉验收、视觉/自动化适配器、发布打包和旧 Python 主链删除。`native-playback`、`native-capture` 与 eframe 的外部依赖因下载超时或授权用量限制尚未完成编译；默认 Rust 主线不受影响。

逐批删除条件和精确范围见 `LEGACY_RETIREMENT.md`。

## 11. 当前可用性增量（Phase 13）

- 小白入口：独立 eframe 桌面端首次设置向导可选择 DeepSeek、KoboldCpp、Ollama，生成本地配置与控制令牌，并可选配置 Bilibili 房间号。
- 开发者入口：桌面开发者面板显示结构化控制事件，启动终端保留服务原始 stdout/stderr；两者共用本地控制协议。
- Bilibili：服务读取配置后在后台启动隔离连接任务，连接器负责握手、30 秒心跳、zlib 包边界和重连，统一事件先经 EventBus，再写入本地记忆投影；默认关闭，断线只影响平台输入；本地 WebSocket 契约测试覆盖双会话重连和重复礼物 ID 的 EventBus 去重。 启用时连接健康每约 2 秒投影到控制协议，未启用时不增加健康项。
- 并发安全：MemoryStore 的克隆实例共享写锁，运行时与直播输入不会交错破坏 JSONL 文件；Runtime 通过 `MemoryPort::recall_for_context` 使用受限分类上下文，不把 live_event 原始日志直接混入普通对话。
- 受控响应模式：Bilibili 默认 `suggest`，只有显式 `automatic`（或兼容旧 `auto_react = true`）且通过急停/冷却检查时才提交 Runtime；`confirm` 为人工确认扩展边界。
- 角色热切换边界：`PersonaSnapshot` 携带 `profile_id/revision`，控制协议在无活动回合时更新 Runtime 系统提示词并广播 `PersonaChanged`，失败保持旧角色。
- 新手/开发者双通道：桌面端通过 `persona` 快照编辑角色草稿，经过确认队列后发送 `set_persona`；桌面事件日志和服务端 tracing 同时记录确认、应用、版本变化与失败。
- 验证：workspace 测试、严格 Clippy、架构门禁、Allman 门禁、git diff 检查，以及独立桌面端离线编译均通过。
- 舞台协议：ai-ex-stage 已提供版本化 StageAction、能力声明、动作边界校验和 dry-run 执行器；ai-ex-stage-obs 提供字幕/场景/热键 JSONL 录制和 OBS WebSocket v5 外部连接边界。

OBS JSONL 回放、动作遥测和真实 OBS WebSocket v5 request/response 联调已接入服务/桌面开发者链路；本地 fake WebSocket 契约测试覆盖成功、失败和健康降级；游戏动作继续保持独立插件边界。

- OBS 适配：ai-ex-stage-obs 同时提供无副作用 JSONL dry-run 和 OBS WebSocket v5 连接器；真实连接默认关闭，密码只从环境变量读取。
- StageRouter：核心协议层按能力分发舞台动作并广播急停；Runtime 已通过 `StageOutput` 接入语音、口型、表情、停止和可选字幕动作，字幕/OBS 仍由舞台适配器负责执行。
- 舞台遥测：`StageOutput` 维护有界 `StageJournal`，控制协议只暴露 domain `StageSnapshot` 摘要；桌面开发者面板可观察动作而不触发外部副作用。
- OBS 健康投影：真实 WebSocket stage 的共享健康状态每约 2 秒投影到服务快照；断线、响应失败和恢复后的重连不会继续显示过期“已连接”。连接器在下一次动作或 5 秒探针时按原配置受控重连，恢复后重新标记 ready。

- 视觉/游戏自动化：`DryRunAutomationPort` 已提供确定性屏幕帧、有界动作队列、Permit 能力二次校验和急停拒绝路径；真实桌面适配器尚未启用。
- 插件进程边界：`ai-ex-plugin::StdioPlugin` 已提供带尺寸限制、响应校验和生命周期回收的 JSON-RPC stdio 客户端，真实视觉/游戏 Provider 仍默认关闭。
- Typed 插件契约：视觉观察与游戏动作统一使用带 schema/request UUID 的 `AutomationPluginRequest/Response`，原始帧通过引用传递，不进入 JSON-RPC 大字段。
- 插件注册表：组合根创建 `PluginRegistry` 并将注册表/插件健康投影到控制协议，桌面安全面板可直接显示；当前默认 0 个外部插件。
- 外部插件启动：`[plugins]` 默认关闭；显式启用时按 manifest ID/health 做隔离启动与超时校验，单个插件失败不影响主运行时。
- 插件运行期监控：已启动 Provider 每 15 秒刷新 health，并检测进程退出；失败只更新桌面/控制协议状态，不自动重启。
- Typed automation bridge：`PluginAutomationPort` 将安全许可与版本化 observe/execute/interrupt 请求连接起来，服务端 stdio 适配器只负责 JSON-RPC 编解码，核心状态机不感知插件协议。
- 首次设置连通性检查：桌面向导后台探测 HTTP/HTTPS 地址，不阻塞 UI；DeepSeek 密钥不落盘，服务启动仍做完整 provider health。
- Provider 健康诊断：DeepSeek、KoboldCpp、Ollama 将 HTTP 鉴权/地址/限流/服务端故障与 timeout/connect 错误转换为可操作中文详情，并保留 provider 原始边界。
- 模型清单验证：DeepSeek `/models` 和 Ollama `/api/tags` 在可解析时检查 configured model；非标准兼容响应降级为“已连接但未验证”，不伪造可用状态。
- 桌面健康轮询：控制连接保持后周期性重新拉取 provider、插件、VTS/TTS 和安全状态；健康请求失败只记录开发者日志，不伪造断线。 健康变化会以 ComponentHealthChanged 事件进入有界重放历史，桌面新手面板消费快照，开发者面板消费变化事件。
- 模拟直播闭环报告：`ai-ex-simulator --report` 记录事件过滤、响应建议和记忆投影，保证没有模型/平台凭据时也能验收编排链路。
- 服务事件回放报告：`--replay-events` 支持 `--replay-report`，与模拟器输出同一事件/响应/记忆字段；报告参数脱离事件回放时会明确拒绝。
- 诊断体验：桌面开发者面板支持关键词筛选和本地 UTF-8 日志导出；新手连接失败时显示可执行的下一步提示。
- Provider 配置体验：首次设置按 DeepSeek/KoboldCpp/Ollama 显示云端密钥、本地服务端口和模型安装提示；桌面主界面独立显示当前模型 Provider 健康详情，不把供应商 SDK 带入核心。
- 人格/记忆/安全投影：桌面只读消费 Persona、memory 和 safety 健康快照，展示人格版本、直播模式、记忆状态与急停状态；任何外部动作仍只能经过服务端 SafetyGate。
- 舞台能力投影：StageSnapshot 增加带默认值的 capabilities 字段，由 StageRouter 实际能力生成；桌面仅展示和回放，不推断或开放任意桌面控制。
