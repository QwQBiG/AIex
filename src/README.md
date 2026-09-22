# 历史 Python 源码

`src/` 保留早期 Python 实现，用于行为对照、旧配置与数据迁移。当前 Rust 主线位于 [`crates/`](../crates/)，桌面便携版不依赖本目录。当前职责与依赖方向见[架构说明](../docs/ARCHITECTURE.md)。

## 当前入口

| 入口 | 用途 |
| --- | --- |
| [桌面入口](../crates/ai-ex-desktop/src/main.rs) | 原生桌面、离线人物预览与连接入口 |
| [服务入口](../crates/ai-ex-service/src/main.rs) | 运行时与模型、记忆、声音等组件的装配 |
| [旧配置迁移](../crates/ai-ex-migrate/src/main.rs) | 旧 JSON 配置迁移工具 |
| [根目录 main.py](../main.py) | 保留的 Python 启动入口；不是 Rust 桌面启动方式 |

日常使用见 [Windows 便携版](../docs/PORTABLE_WINDOWS.md)，当前验收见[整体验收流程](../docs/MANUAL_TEST_PLAN.md)。

## 历史代码索引

下表用于定位旧实现，不代表这些模块已全部由 Rust 等价替代或完成当前环境验收。

| 历史职责 | 主要文件或目录 |
| --- | --- |
| 配置、领域与工作流 | [config.py](config.py)、[domain.py](domain.py)、[conversation.py](conversation.py)、[system_workflow.py](system_workflow.py)、[error_handler.py](error_handler.py) |
| 桌面与字幕 | [gui_controller.py](gui_controller.py)、[subtitle_window.py](subtitle_window.py)、[pink_theme.py](pink_theme.py)、[tooltip.py](tooltip.py) |
| 模型连接 | [llm_client.py](llm_client.py)、[enhanced_llm_client.py](enhanced_llm_client.py)、[llm/](llm/) |
| 记忆与检索 | [memory_core/](memory_core/) |
| 语音与全双工 | [tts_pipeline.py](tts_pipeline.py)、[tts_player.py](tts_player.py)、[text_cleaner.py](text_cleaner.py)、[full_duplex_engine/](full_duplex_engine/) |
| 外形与流处理 | [vts_client.py](vts_client.py)、[stream_processor.py](stream_processor.py) |
| 观察、动作与安全 | [vision_client.py](vision_client.py)、[screen_capturer.py](screen_capturer.py)、[action_engine.py](action_engine.py)、[agent_manager.py](agent_manager.py)、[safety_manager.py](safety_manager.py)、[reflex_engine.py](reflex_engine.py) |
| 自然行为与监控 | [natural_behavior.py](natural_behavior.py)、[natural_speaker.py](natural_speaker.py)、[natural_thinker.py](natural_thinker.py)、[performance_monitor.py](performance_monitor.py)、[health_check.py](health_check.py) |

## 保留与迁移边界

旧配置、记忆数据库、模型、令牌和参考音频不能因为主线改用 Rust 而直接删除。配置迁移不等于 ChromaDB 或多模态记忆迁移；旧数据是否已迁移，需要分别核验。

历史模块的替代范围、设备验收与删除条件集中在[遗留退役清单](../docs/LEGACY_RETIREMENT.md)。本目录保留不表示应重新启用旧主链；当前测试与历史测试的区别见 [tests 目录说明](../tests/README.md)。
