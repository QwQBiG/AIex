# 测试入口与历史测试

`tests/` 保存 Python 主链的历史测试；当前 Rust 测试位于 [`crates/`](../crates/) 内，由 Cargo 执行。不要把本目录的 Python 测试结果当作当前桌面版的验收结果。

## 当前 Rust 验证

自动检查以 [Rust CI](../.github/workflows/rust.yml) 为准。核心工作区使用[根 Cargo 清单](../Cargo.toml)，桌面使用[独立 Cargo 清单](../crates/ai-ex-desktop/Cargo.toml)，因此两部分都需检查。

在仓库根目录执行：

```powershell
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
cargo test --manifest-path crates/ai-ex-desktop/Cargo.toml --locked
cargo clippy --manifest-path crates/ai-ex-desktop/Cargo.toml --all-targets --locked -- -D warnings
cargo fmt --manifest-path crates/ai-ex-desktop/Cargo.toml -- --check
pwsh -NoProfile -File tools/check_architecture.ps1
```

测试分布在各 crate 的源码测试模块和集成测试目录，例如[运行时取消测试](../crates/ai-ex-core/src/runtime_cancellation_tests.rs)、[记忆管理测试](../crates/ai-ex-memory/src/management_tests.rs)、[服务生命周期集成测试](../crates/ai-ex-service/tests/lifecycle.rs)与[桌面源码](../crates/ai-ex-desktop/src/)。架构检查与其独立回归入口见[工具索引](../tools/README.md)。

自动检查不能替代真实模型、窗口、音频设备与外部服务的实际验收。整包启动、人物与场景、对话、记忆及可选设备的操作步骤见[整体验收流程](../docs/MANUAL_TEST_PLAN.md)；没有准备对应服务或设备的项目应记录为“未测”。

## 本目录的历史内容

| 范围 | 示例入口 |
| --- | --- |
| 配置、领域与对话 | [test_config.py](test_config.py)、[test_domain.py](test_domain.py)、[test_conversation.py](test_conversation.py) |
| 模型、语音与外形 | [test_llm_client.py](test_llm_client.py)、[test_tts_pipeline.py](test_tts_pipeline.py)、[test_tts_player.py](test_tts_player.py)、[test_vts_client.py](test_vts_client.py) |
| 界面与全双工 | [test_gui_controller.py](test_gui_controller.py)、[test_full_duplex_engine.py](test_full_duplex_engine.py)、[test_lipsync_integration.py](test_lipsync_integration.py) |
| 记忆 | [test_memory_core_setup.py](test_memory_core_setup.py)、[test_data_models.py](test_data_models.py)、[test_entity_extractor.py](test_entity_extractor.py) |
| 观察、动作与安全 | [test_action_engine.py](test_action_engine.py)、[test_agent_manager.py](test_agent_manager.py)、[test_vision_client.py](test_vision_client.py)、[test_safety_manager.py](test_safety_manager.py) |
| 历史集成与性能检查 | [test_integration.py](test_integration.py)、[test_agent_integration.py](test_agent_integration.py)、[test_performance_optimizations.py](test_performance_optimizations.py) |
| 历史专项脚本 | [scripts/](scripts/)，含 [test_critical_fixes.py](scripts/test_critical_fixes.py)、[test_streaming_fix.py](scripts/test_streaming_fix.py) 与 [test_hotkeys.py](scripts/test_hotkeys.py) |

这些文件依赖旧 [`src/`](../src/README.md) 实现及其 Python 环境，部分涉及窗口、模型或声音服务。当前 Rust CI 不执行它们；保留文件不代表它们已在当前环境通过，也不构成覆盖率承诺。

保留历史测试用于迁移时比对行为。退役应与对应功能、旧数据和实机验收一起核对，条件见[遗留退役清单](../docs/LEGACY_RETIREMENT.md)；当前系统边界见[架构说明](../docs/ARCHITECTURE.md)。
