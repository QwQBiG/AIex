# AIex 遗留 Python 退役清单

## 原则

删除依据是功能覆盖和验证证据，不是文件年龄或命名。用户数据、模型、令牌、参考音频和配置迁移前始终保留。当前仓库已有 Git 历史；批量删除仍需列出精确目标、保留可追溯快照并取得用户明确授权。本文不执行清理。

遗留范围包括 `src/`、`tests/`、`tools/` 中的 Python 工具、`config_examples/` 中的旧 JSON 配置，另有根入口 `main.py`、`requirements.txt` 和旧 `config.json`。这些目录不全是历史内容：`tools/` 的 PowerShell 脚本和 `config_examples/` 的 JSONL 回放仍服务于 Rust 主线。精确归属见[项目目录](PROJECT_LAYOUT.md)，文件数以当前工作树为准。

## 历史清理记录与当前边界

- 早期曾清理日志和 Python 缓存；运行后它们可能重新生成，历史记录不能证明当前目录不存在。它们的再次清理不属于当前架构整理。
- 旧报告中的产品意图已压缩到 `CORE_TECHNICAL_BASELINE.md`。
- 早期记录中，10 份 `memory_db/backups/*.json.gz` 的只读核验结果为空集；这不是当前内容的重新核验。`chroma.sqlite3` 仍须保留，迁移或删除前需针对实际文件单独验证。

## 退役批次

| 批次 | 遗留范围 | Rust 替代 | 当前结论 | 删除门禁 |
| --- | --- | --- | --- | --- |
| A | 9 份根目录历史报告、`.hypothesis/` | 核心基线与本清单 | 历史归档候选，需重新核对内容 | 用户确认精确清单 |
| B | `config.py`、`conversation.py`、`domain.py`、`error_handler.py`、`text_cleaner.py`、`stream_processor.py` | config/domain/core/text/migrate | 配置迁移正常/拒绝覆盖路径已验证 | 有效 Git + 用户复核迁移结果 |
| C | `llm_client.py`、`llm/ollama.py`、`llm/koboldcpp.py`、`vts_client.py` | ollama/koboldcpp/vts | 两种 LLM 与 VTS 的 Rust 边界已覆盖 | 外部服务契约测试 |
| D | `tts_pipeline.py`、`tts_player.py`、`full_duplex_engine/` | audio/tts/duplex/asr/capture | 默认逻辑覆盖 | native playback/capture 编译与设备实测 |
| E | `memory_core/` | memory | 基础检索/持久化覆盖 | ChromaDB/多模态数据导入或明确放弃旧数据 |
| F | `gui_controller.py`、`subtitle_window.py`、主题和提示组件 | control/ui-model/desktop | 0.7 发布记录已包含桌面构建、离屏画面和交互回归；尚不等于旧功能全部完成实机替代 | 对照旧能力的窗口、模型与设备验收 |
| G | `vision_client.py`、`screen_capturer.py`、`action_engine.py`、`agent_manager.py`、`safety_manager.py` | vision/safety/automation/audit | 安全核心覆盖，平台动作未实现 | Windows 捕获/输入适配器、急停实测、审计验收 |
| H | `*_optimized.py`、`*_monitor.py`、预加载/热重载/自然行为等旁支 | 可观察性与后续明确功能 | 不在 Rust 主链 | 逐项确认无独有产品能力后删除 |
| I | 对应 Python 测试、诊断工具、`requirements.txt`、`main.py` | Rust 测试、服务、桌面端 | 最后处理 | 所有前置批次完成，发布包可启动 |

## 批次 A 精确目标

- `.hypothesis/`
- `BUG_FIX_REPORT.md`
- `COMPLETION_SUMMARY.md`
- `FINAL_REPORT.md`
- `IMPROVEMENT_REPORT.md`
- `INTEGRATION_GUIDE.md`
- `NATURAL_BEHAVIOR_REPORT.md`
- `ULTRA_OPTIMIZATION_REPORT.md`
- `V44_SUPER_OPTIMIZATION_REPORT.md`
- `V44_TEST_REPORT.md`

该清单只记录历史候选项，不表示当前删除授权，也不保证候选内容没有变化。操作前需要重新核对精确路径和实际内容。

## 完成定义

只有当以下条件同时满足，才可宣布 Python 主链退役：

1. 默认与原生 feature 构建、测试、Clippy 和 `cargo fmt` 检查通过。
2. Ollama、VTS、TTS、ASR、麦克风、播放设备和视觉服务完成实机健康与关键路径验收。
3. 桌面端完成启动、连接、流式对话、打断、急停、断线恢复和退出验收。
4. 旧记忆与配置完成迁移或形成用户确认的弃用决定。
5. Windows 自动化只能经 permit 和持久审计执行，急停能撤销进行中许可。
6. 所有删除目标在执行前重新列出绝对路径并确认位于工作区内。
