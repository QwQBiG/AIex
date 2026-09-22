# 维护工具索引

当前 Rust 主线使用下列 PowerShell 工具。Python 脚本保留供旧实现排查与迁移参考，不是便携桌面的运行依赖。系统职责见[架构说明](../docs/ARCHITECTURE.md)，历史代码处理条件见[遗留退役清单](../docs/LEGACY_RETIREMENT.md)。

## 主线工具

命令示例在仓库根目录的 PowerShell 7 中执行。

| 工具 | 用途与实际参数 |
| --- | --- |
| [check_architecture.ps1](check_architecture.ps1) | 读取核心工作区与独立桌面的 Cargo 元数据，校验内部依赖方向与包登记。无参数执行正常检查；`-ProbeViolation` 在内存中注入违规依赖，预期退出码为 1，不改写 Cargo 文件。 |
| [create_control_token.ps1](create_control_token.ps1) | 生成随机控制令牌并写入 UTF-8 文件。`-Path` 指定文件，默认 `config/control.token`；目标存在时拒绝覆盖，只有明确轮换令牌才使用 `-Force`。 |
| [package_windows.ps1](package_windows.ps1) | 在 Windows 上构建并打包桌面、服务与分发模板，检查可执行文件与版本，生成 ZIP 和 SHA256。支持 `-OutputDirectory`、`-SkipBuild`、`-Offline`；已存在的输出拒绝覆盖。 |

架构检查使用离线、锁定依赖的 Cargo 元数据：

```powershell
pwsh -NoProfile -File tools/check_architecture.ps1
pwsh -NoProfile -File tools/check_architecture.ps1 -ProbeViolation
```

第二条是拒绝路径检查，应退出 1；不能把它当作应成功的常规命令。

独立管理服务需要令牌时，可指定写入位置：

```powershell
pwsh -NoProfile -File tools/create_control_token.ps1 -Path 'config/control.token'
```

令牌属于本机凭据，不应提交到仓库或放入分发包。便携版的首次设置会创建自己的令牌，日常使用无需预先执行本脚本。

打包示例：

```powershell
pwsh -NoProfile -File tools/package_windows.ps1
```

打包要求可用的 Rust 与 Windows 链接器；`-SkipBuild` 仅适用于已完成 release 构建的程序，`-Offline` 要求依赖已在本机缓存。分发内容与安装检查边界见 [Windows 便携版](../docs/PORTABLE_WINDOWS.md)。

## 架构检查的回归入口

[test_architecture.ps1](test_architecture.ps1) 无参数，验证正常登记和违规依赖等情况。它使用测试元数据与独立临时目录，不编辑当前 Cargo 清单，不编译产品代码。

```powershell
pwsh -NoProfile -File tools/test_architecture.ps1
```

[architecture/validation.ps1](architecture/validation.ps1) 是检查与回归共用的内部验证逻辑，不是单独的用户命令。

## 历史 Python 工具

下列用途来自脚本源码；此索引不承诺它们已在当前环境运行通过，也不为旧工具补充未实现的参数。

| 脚本 | 历史用途与边界 |
| --- | --- |
| [run_diagnostics.py](run_diagnostics.py) | 调用旧全双工引擎的诊断模块并输出报告。 |
| [check_gpu_status.py](check_gpu_status.py) | 检查 NVIDIA、PyTorch 与 Ollama 状态，并输出固定的语音性能建议；不实际测量语音服务性能。 |
| [validate_config.py](validate_config.py) | 检查旧 JSON 配置、外形热键与语音服务；不用于当前 Rust TOML 配置。 |
| [measure_performance.py](measure_performance.py) | 调用旧模型与语音管线测量流式响应和播放延迟；可能请求模型与播放声音。 |
| [system_optimizer.py](system_optimizer.py) | 写入旧音频配置、嵌入回退源码和固定状态报告；报告不代表实时检测结果，也不是 Rust 主线的优化入口。 |
| [check_vts_params.py](check_vts_params.py) | 使用旧 VTS 客户端连接并认证，列出嘴部和声音相关参数。 |
| [list_vts_hotkeys.py](list_vts_hotkeys.py) | 连接 VTube Studio，获取当前模型的热键信息。 |

排查当前版本优先使用[整体验收流程](../docs/MANUAL_TEST_PLAN.md)及桌面设置与诊断。旧脚本依赖 [`src/`](../src/README.md) 与历史 Python 环境；运行前应先阅读对应源码，核对配置写入、服务连接和设备操作。
