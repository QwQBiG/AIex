# 维护工具索引

当前原生模型验证与发行使用 Rust `cargo xtask`；便携外层打包、架构和令牌入口仍保留下面的 PowerShell 工具。旧 Python 脚本只供历史排查与迁移参考，不是新的本地模型运行依赖。全部工程工具退役/收口仍属于 1.0 门槛，不能以原生入口可用推断仓库已零 Python。系统职责见[架构说明](../docs/ARCHITECTURE.md)，旧文件条件见[遗留退役清单](../docs/LEGACY_RETIREMENT.md)。

## 主线工具

命令示例在仓库根目录的 PowerShell 7 中执行。

### 构建空间与缓存

日常使用 `./tools/dev.ps1 -Workspace core -Task check`，桌面将 `core` 换为 `desktop`；原生工作进程用 `native`。该入口固定复用根 `target` 和 `crates/ai-ex-desktop/target`，不创建日期化构建目录，构建前默认要求至少 10 GiB 可用空间。`-Plan` 只查看命令和目标，`-Offline` 使用已有依赖，`-Task verify` 顺序执行格式、测试及严格 Clippy。

频繁修改时用 `-Package ai-ex-core` 这类单包检查；`-Task test -Package ai-ex-service -TestFilter native_desktop` 只运行匹配测试。过滤器只允许用于 test，verify 保持完整验证。统一入口使用目标目录中的 `.development.lock` 持有进程锁，核心和原生互斥，桌面可以独立构建；任务结束或进程退出后锁释放，遗留空文件无需删除。裸 Cargo 仍使用 Cargo 自己的锁，不参与这个入口的互斥。

项目的 dev/test 使用 `debug=1`，保留行号及模块调试信息，减少类型和变量调试信息；需要完整调试时，仅为该次进程设置 `CARGO_PROFILE_DEV_DEBUG=2` 或 `CARGO_PROFILE_TEST_DEBUG=2`。日常 check/build 沿用增量编译，一次性 test/clippy/verify 及 release 默认设置 `CARGO_INCREMENTAL=0`；频繁修改测试时可显式加 `-KeepIncremental`。工具链、目标和编译参数保持一致，有利于复用已编译依赖；本入口不改全局环境、链接器或 release 优化/断言策略。配置含义见 [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html#debug)。

`./tools/manage_build_cache.ps1` 默认只查看五个已登记的增量缓存目录；停止所有 Cargo 构建后加 `-Apply` 才清理。工具先核对全部目录的仓库范围、忽略状态、链接、文件类型及锁，遇到源码、只读文件或活跃构建会拒绝；不清理模型、`deps`、成品、报告或临时验证源码。不要对混合存放这些内容的 `target` 直接执行整目录删除或无差别 `cargo clean`。首次清理后的编译会较慢，后续性能与空间应按实际使用权衡。

普通用户从 `AIex.exe` 的“准备本地伙伴”完成模型准备，无需执行本页命令。原生控制、下载导入及声音向导已随 0.9 发布；实际操作见[整体验收流程](../docs/MANUAL_TEST_PLAN.md)。

| Rust 入口 | 作用与文档 |
| --- | --- |
| `cargo xtask native-verify` | 固定模型文件的大小、SHA256 与路径验证；见[文件验证](../docs/NATIVE_WORKERS.md#文件校验) |
| `cargo xtask native-probe` | 显式启动自有 worker、请求/取消/退出及文件音频检查；见[进程探针](../docs/NATIVE_WORKERS.md#stdio-探针)，不会打开音频设备 |
| `cargo xtask native-sources` | 明确输入的原生源码、registry 源包、项目 Git 源码与必要构建材料归档 |
| `cargo xtask native-bundle` | 固定 worker、DLL、Ollama、许可和对应源码形成 Runtime ZIP 与独立 RuntimeManifest |
| `cargo xtask native-catalog` | 用实际 Runtime ZIP/Manifest 和固定模型材料生成分发目录，不使用假摘要或浮动输入 |
| `cargo xtask architecture` | 验证三个 Rust 工作区的内部依赖边界；`--probe-violation` 检查拒绝路径 |
| `cargo xtask cache-inventory --project .` | 只读检查五个构建缓存目录；不删除文件，也不保证检查后可安全清理 |
| `cargo xtask control-token --path <新文件>` | 创建控制令牌并拒绝覆盖；只有明确追加 `--force` 才轮换已有令牌 |
| `cargo xtask portable-inspect <便携包 ZIP>` | 只读检查历史版本目录结构和 ZIP 条目；完整运行验证仍使用下面的便携包验证器 |
| `cargo xtask portable-verify --archive <便携包 ZIP> --evidence-directory <现有目录>` | 在新证据目录中验证解压、摘要、无窗口启动、错误路径和离线回放 |
| `cargo xtask zero-python --project . --distribution <已解压的便携包目录>` | 检查跟踪文件、Cargo 绑定、明确启动命令、分发文件与 PE 静态导入，列出剩余问题与检查限制 |

原生发行工具的参数、JSON 输入及严格构建顺序统一见[原生发行工具](../xtask/NATIVE_DISTRIBUTION.md)。工具读取显式输入、拒绝覆盖已有输出，不扫描私人模型，不执行或递归展开对应源码归档。源码与最终派生目录有明确分层，避免 Runtime SHA 与源码包循环引用。

零 Python 检查当前会因保留的旧文件而失败；它不分析运行时动态加载、嵌入解释器、改名程序、嵌套压缩包或远程服务，也不能替代未安装 Python 的干净 Windows 验收。

| 工具 | 用途与实际参数 |
| --- | --- |
| [check_architecture.ps1](check_architecture.ps1) | 读取核心工作区与独立桌面的 Cargo 元数据，校验内部依赖方向与包登记。无参数执行正常检查；`-ProbeViolation` 在内存中注入违规依赖，预期退出码为 1，不改写 Cargo 文件。 |
| [create_control_token.ps1](create_control_token.ps1) | 通过 dev 入口构建 Rust 工具，再生成随机控制令牌。`-Path` 指定文件，默认调用目录下的 `config/control.token`；目标存在时拒绝覆盖，只有明确轮换令牌才使用 `-Force`。 |
| [package_windows.ps1](package_windows.ps1) | 通过 dev 入口构建后打包桌面、服务、迁移器与平台助手，检查版本并生成 ZIP/SHA256。支持 `-OutputDirectory`、`-SkipBuild`、`-Offline`、`-Jobs`、`-MinimumFreeGiB`；使用两套固定目标，已有输出拒绝覆盖。 |
| [test_development.ps1](test_development.ps1) | 用隔离 PowerShell 进程和假 Cargo 验证目录复用、增量策略、单包筛选、互斥及失败后环境恢复，不编译应用或调用设备。 |
| [verify_windows_portable.ps1](verify_windows_portable.ps1) | 验证 ZIP 文件、版本、中文/空格路径与无窗口运行；兼容旧版布局，0.10 要求平台助手和专用画布入口在内的十文件包。`-ArchivePath` 必需；`-EvidenceDirectory` 指定证据父目录，默认 `target`。入口先通过 `dev.ps1` 构建 Rust 工具，每次创建独立子目录并输出 JSON。 |

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

将 `$archivePath` 设为打包结果中的 ZIP 路径后，可单独验证已有包：

```powershell
pwsh -NoProfile -File tools/verify_windows_portable.ps1 -ArchivePath $archivePath
```

验证入口需要 Rust 构建环境；已有构建结果时，也可直接运行 `target/debug/xtask.exe portable-verify --archive <便携包 ZIP> --evidence-directory <现有目录>`，无需调用 Cargo。验证在中文和空格目录中解压，只检查公开分发模板与离线回放；缺少服务程序应明确报错。证据、解压目录与 JSON 报告保留在唯一子目录，不覆盖旧证据，也不清理用户文件。真实窗口、模型和设备仍按整体验收流程测试。

## 架构检查的回归入口

[test_architecture.ps1](test_architecture.ps1) 无参数，通过开发入口运行 `xtask` 的架构单元测试，验证正常登记和违规依赖等情况。它使用测试元数据与独立临时目录，不编辑当前 Cargo 清单；首次运行可能构建 `xtask` 及其 Rust 依赖。

```powershell
pwsh -NoProfile -File tools/test_architecture.ps1
```

[architecture/validation.ps1](architecture/validation.ps1) 保留为历史验证逻辑，当前架构检查与回归均使用 Rust 实现，不再调用该文件。

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

旧 JSON 配置可先执行 `ai-ex-migrate --input OLD.json --preview`，在终端检查转换后的 TOML 和未映射字段提示；预览不会写文件。结果可能包含个人配置，请勿直接分享。确认后以 `--output NEW.toml` 代替 `--preview`，迁移器只写新文件并拒绝覆盖。未映射字段只报告名称，不自动迁移，需要自行核对。

原生 BGE 重建使用 `ai-ex-service.exe --config <native-bootstrap.toml> --native-config <native-v1.toml> --reindex-memory`。先关闭使用这些文件的服务并备份记忆；只重建配置中当前人物的索引，不修改文本与来源。BGE 必须已准备，工具有界等待冷加载；失败不提交半套向量。日常写入仍保留两秒预算，冷启动或模型不可用时先保存文本并使用词法召回，需要时再显式重建。

便携包同时提供 `ai-ex-migrate.exe`。在解压目录运行迁移器即可导入数据，无需安装 Rust；迁移文档中的 `cargo run -p ai-ex-migrate --locked --` 可替换为 `.\ai-ex-migrate.exe`，保留后续参数，并明确选择输入、全新输出和角色 ID。

### 显式多模态快照导入

`ai-ex-migrate --multimodal-memory` 可将旧 `MultimodalMemory` dataclass 的显式 JSON 数组转换为独立 Rust JSONL，0.8 便携包已提供迁移器入口。旧模块没有磁盘导出接口；此模式需要已经准备好的快照，不会连接 Python、读取 Chroma，也不能从旧主记忆 gzip 备份找回其中未保存的媒体对象。

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

### Chroma v2 快照桥接

`ai-ex-migrate --chroma-copy-export` 用固定的原生服务打开明确指定的旧库工作副本并导出快照；`--chroma-export` 也可从指定的只读 v2 API 导出。`--chroma-snapshot` 将支持的主记忆、独立实体候选和声明的来源关联转换为新 JSONL，并保留原始归档。原生 SQLite/HNSW 合成库的复制、导出和导入已验证，真实旧库仍需用户用关闭后的副本验收。详细操作和边界见 [Chroma 桥接说明](../docs/CHROMA_BRIDGE.md)；工具不会自动打开现用数据库。
