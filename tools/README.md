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

## Rust 离线迁移与检查

新增 `ai-ex-service --validate-config` 离线验证和 `ai-ex-migrate --memory-backup` 旧备份文本/向量/元数据迁移与 `ai-ex-service --reindex-memory` 当前角色索引重建；使用步骤、格式与数据边界见[完全 Rust 化记录](../docs/RUST_ONLY_MIGRATION.md)。语句分块的 Release 对照基准在 `crates/ai-ex-text/benches/sentence_stream.rs`，记忆检索与启动缓存对照在 `crates/ai-ex-memory/benches/recall.rs`；均由 Cargo 执行，不需要 Python。

### 显式多模态快照导入

当前源码的 `ai-ex-migrate --multimodal-memory` 可将旧 `MultimodalMemory` dataclass 的显式 JSON 数组转换为独立 Rust JSONL。旧模块没有磁盘导出接口；此模式需要已经准备好的快照，不会连接 Python、读取 Chroma，也不能从旧主记忆 gzip 备份找回其中未保存的媒体对象。此功能尚未包含在已发布的 0.7.1-alpha.1 包中。

以下为合成示例，可保存到独立测试目录的 `multimodal-snapshot.json`。输入是一个普通 UTF-8 JSON 数组，不是逐行 JSON 或 gzip：

```json
[
  {
    "memory_id": "mm_0123456789abcdef_1791200000",
    "modality": "visual",
    "text_content": "旅行照片的说明。",
    "audio_features": null,
    "visual_features": {
      "description": "山边的湖泊",
      "objects": [{"name": "湖泊", "details": {"season": "autumn"}}]
    },
    "timestamp": 1791200000.125,
    "importance_score": 0.7,
    "access_count": 3,
    "tags": ["旅行"],
    "metadata": {"source": "manual-fixture"}
  }
]
```

在仓库根目录构建过迁移程序后，使用独立输入和未存在的输出路径：

```powershell
.\target\debug\ai-ex-migrate.exe --multimodal-memory --input .\test-data\multimodal-snapshot.json --output .\test-data\multimodal-import.jsonl --profile memory-import-test
```

工具不改写输入、不覆盖已有输出，也不自动合并正在使用的记忆文件。可用独立测试配置指向生成的 JSONL，并选择相同 `profile_id` 核对；不要把示例直接写入日常记忆路径。该模式与 `--memory-backup` 互斥，不接受 `--utc-offset-minutes` 或 `--embedding-space`。

| 输入字段 | 转换与校验 |
| --- | --- |
| `memory_id` | 必填；UUID 直接保留，旧 `mm_<16 位十六进制>_<整数秒>` ID 映射为稳定 UUID；重复 ID 拒绝 |
| `modality` | 必填，`text` / `audio` / `visual` / `multimodal`；特征对象的组合规则见[控制协议](../docs/CONTROL_PROTOCOL.md#记忆管理) |
| `text_content` | 最多 4096 个字符；文本类型须非空，其他类型缺少正文时使用对应观察占位描述 |
| `audio_features` / `visual_features` / `metadata` | 保留嵌套 JSON 对象；两种特征可用 `null` 表示缺省，元数据默认空对象 |
| `timestamp` | 必填，非负 epoch 秒数，可有小数；转换为毫秒，截去不足毫秒的部分，不接受本地日期字符串 |
| `importance_score` | 0–1，默认 0.5；四舍五入映射到 `importance` 的 0–1000 |
| `tags` | 默认空数组，最多 16 个非空标签，每个最多 64 个字符 |
| `access_count` | 非负整数，默认 0；作为原始快照数据保留，不代表 Rust 已实现访问频次更新或衰减 |

每条记录的 `knowledge.media` 保存可查询和编辑的媒体信息，完整原始快照另存到 `legacy_metadata`，包括原 ID、访问计数和时间；不会向原媒体 `metadata` 注入保留字段。导入来源统一为 `automatic`，不会因旧元数据而升级成确认笔记，也不从媒体描述提取用户姓名或偏好。

输入和输出分别最多 32 MiB，输入最多 50,000 条记录；每条媒体对象序列化最多 8192 字节。无效字段、重复 JSON 键、过深嵌套和超限数据会拒绝整个导入。原始图像、音频文件及其识别模型不在此格式或工具范围内。
