# 完全 Rust 化：功能合同与实施记录

用户目标是项目内不再保留 Python 源码、脚本、测试或运行/构建依赖，保留可用功能与数据。2026-10-05 的这一轮是第一批可运行迁移增量，**尚未完成零 Python**。用户进一步明确：旧桌面/游戏自动化、模板匹配、高级向量/实体/多模态记忆全部属于必迁移核心能力，不能通过退役它们满足零 Python。后续工作以完整使用流程和可展示功能为优先，必要回归随功能迁移完成。

## 当前身份与入口

本机仓库 `D:\Projects\AIex`；分支 `codex/digital-human-foundation`；开始工作时无未提交改动。GitHub 公开 API 对旧名 AIA 与新名 AIex 返回同一 repository ID `1130089481`，正式地址为 `https://github.com/QwQBiG/AIex`。本地 remote 未改动，也没有推送、发布或部署。

当前产品是 Rust 服务与 eframe/egui 原生桌面；根 workspace 30 个包，桌面使用独立 Cargo 清单。README、桌面启动脚本、打包脚本和 Rust CI 均使用 Cargo/原生程序。Python `main.py` 是历史入口，并非当前产品启动链。

[逐文件清单](PYTHON_INVENTORY.tsv)记录 125 个受 Git 跟踪的 `.py`，共 2,476,752 字节：`src/` 76、`tests/` 40、`tools/` 7、`models/` 1、根入口 1；另有 `requirements.txt`。本轮没有删除这些文件、用户配置、旧数据库、模型、音频或凭据。清单不是“必须照抄 125 个文件”的任务列表；按功能合同整合到现有 Rust 模块。

## 本轮可使用的新能力

### 1. 不连接外部服务的配置校验

```powershell
cargo run -p ai-ex-service --locked -- --config config/ai-ex.example.toml --validate-config
```

检查 TOML 与现有 AppConfig 约束，然后退出；不会初始化模型、读取控制令牌、打开记忆、启动后台连接或访问音频设备。它补齐开发/迁移时的离线检查；现有 `--check` 仍用于用户明确准备好的真实服务健康检查。配置读取与解析上限为 1 MiB，错误只报告位置，不回显原字段内容。

### 2. 旧记忆备份转为当前角色的 Rust 记忆

```powershell
cargo run -p ai-ex-migrate --locked -- --memory-backup `
  --input '备份路径/memory_backup.json.gz' `
  --output '新目录/imported-memory.jsonl' `
  --profile '明确选择的角色ID' --utc-offset-minutes 480
```

`--input`、`--output`、`--profile` 均必需。`480` 只是原备份按 UTC+8 本地时间写入时的示例；按原机器时间选择偏移。含 `Z` 或显式时区的时间不需要该选项；无时区的历史时间不猜测。目标必须是新文件，已有文件或并发创建的文件都拒绝覆盖。不直接覆盖正在使用的记忆库。

在备份副本上执行后，先核对角色 ID 与记录，再让**单独的测试配置**的 `[memory].path` 指向输出文件。Rust MemoryStore 可直接读取该 JSONL，并按角色召回；生产配置合并与桌面导入界面留待后续。合成端到端回归验证 gzip → CLI → JSONL → MemoryStore → 角色隔离 → 文本召回和重复导入拒绝覆盖。

支持旧 `memory_core.py::backup_memories` 的 schema 1.0 JSON/gzip 备份，检查 SHA256、数量、重复 ID、字段和时间。SHA256 按旧代码的 `json.dumps(memories, sort_keys=True, separators=(',', ':'))`：字段排序、紧凑分隔符、ASCII 转义（含 UTF-16 代理对），保留文件中数字的原始字面量，避免 `1e-05` 被改为 `1e-5`。独立固定测试备份使用 .NET SHA256 生成校验值；未执行原 Python 或读取用户备份，不能声称已验证每一份真实历史数据。

导入保留 UUID、正文和时间；`INTERACTION`/`SUMMARY` 转为 conversation，`EVENT` 转为 live_event。原 user_input/ai_response 存在时保留双方正文，否则保留完整 content。旧记录保持 Automatic 来源，不能当作用户确认事实。追加的语义记忆增量会保留非空向量到顶层 `embedding = {space, values}`，并把全部原 metadata 保留在 `legacy_metadata`；有向量时必须提供 `--embedding-space NAME`，不猜测模型身份。最多 8,192 维，拒绝非有限、全零或同一备份维度不一致的向量。空向量按无向量处理。原备份保持不变。工具仍不读取 Chroma SQLite/HNSW 库；独立实体集合与多模态模块没有包含在旧备份中，不能声称从这个备份恢复了它们。

压缩输入、解压正文和输出各上限 32 MiB；记录/数组元素上限 50,000；单条文本上限 128 KiB；嵌套上限 64。损坏/截断 gzip、重复 JSON 键、不支持的类型或未知时间拒绝导入。转换在独立阻塞工作线程执行，文件写完并同步后通过同目录硬链接发布；不支持硬链接的文件系统会报错并保留源数据，不能降级为覆盖写入。

### 3. 语音清洗与分句

Rust `SpeechCleanup` 接入当前 `clean_for_speech`，补齐原默认 Emoji、英文/中文括号舞台说明清理，保留中文标点并合并空白。提供原三项开关的 Rust API。嵌套括号完整清理；不闭合括号保留正文。内联代码沿用已有 Rust 规则不朗读，原 Python 曾保留代码内容，不能把这一差异写成完全相同。

流式分句继续使用原句号/叹号/问号/换行语义，改为只扫描追加后缀。UTF-8 分块、残句、清空和复用有对照回归；不改变消息身份、取消或播放时钟。

### 4. 可选语义召回与显式重建

默认继续使用本地文本检索。需要语义召回时，在单独测试配置中显式增加（模型名称和空间标识按实际部署填写）：

```toml
[memory.embedding]
base_url = "http://127.0.0.1:11434"
model = "your-installed-embedding-model"
space = "your-model-version-and-encoding-v1"
timeout_seconds = 30
```

配置本身不会下载模型。正常服务仅在检索和记录时使用 Ollama `/api/embed`，`truncate=false`，避免服务静默截断文本。`--validate-config` 仍完全离线。`space` 是模型版本、编码方式与预处理共同确定的稳定标识；相同维度不代表相同空间。旧 MiniLM 向量不能因为都是 384 维就与其他模型混查；导入可保留原向量，再显式重建。

```powershell
cargo run -p ai-ex-service --locked -- --config '测试配置.toml' --reindex-memory
```

维护命令只处理 `persona.profile_id` 选中的当前角色，逐条请求配置的 embedding 服务；任意一条失败则整批不发布，原 JSONL 与当前内存记录保持原样。它在模型聊天、音频、控制服务等初始化前退出；不自动在启动时扫描重建，不处理其他角色，不增加记录修订号。此命令实际运行会调用配置的模型，本轮验收只使用本地 HTTP mock。

每个独立 MemoryStore 首次修改取得 `<memory.path>.lock` 的操作系统独占写入锁，持有至最后一个 clone 关闭；正在写入的服务会使其他实例的重建/修改明确失败。首次取得写入权后重新读取磁盘，避免先打开的陈旧快照覆盖新内容。只读实例是打开时的快照，不是跨进程实时订阅。锁文件保留，不要在服务运行时手动删除它。

写入新对话或用户记忆时保存向量；更正会清除旧 metadata 并重新生成对应向量，模型失败则保留新文本、清除旧向量；普通写入的向量请求额外限制两秒，避免必须完成的记忆持久化让停止/退出等待漫长模型超时；显式重建仍按配置的请求超时执行；遗忘/清空同时移除原文和派生向量。没有配置向量模型时，更正也会移除失效向量。列表和控制协议只传输原 MemoryEntry，完整向量与旧 metadata 不占用 48 KiB 的界面分页预算；导出 JSONL 保留二者。

查询只比较空间与维度均匹配的向量。余弦相似度达到 0.45 的候选参与语义召回，混合分数为 `0.8*cosine + 0.2*(命中查询词项数/查询词项数)`；未达到阈值的向量不贡献语义分，原词法命中仍可召回。用户主动记录/更正继续优先，组内按最近更新排序；自动记录按相关分数、更新时间、记录顺序排序。聊天上下文继续排除 live_event。未配置/暂时不可用的模型使用旧词法排序，健康状态说明 lexical fallback；不会用随机/哈希向量伪装语义检索。

词项与归一化向量在读取/写入时缓存；查询只构造一次词项集合，并用有界堆选择 top-k。当前是精确扫描，适合先验证真实角色记忆量的性能，没有宣称具备 Chroma/HNSW 的大规模性能。尚未迁移旧 importance/近期去重排序、实体事实提取与冲突关系、多模态 API，也未内嵌 MiniLM 推理；这些仍是必迁移范围，不通过删除旧代码退役。
## 模块设计与取舍

| 职责 | 实现边界与选择 |
| --- | --- |
| 身份/消息/错误 | domain/protocol；保留版本化类型、AppError 分类，无设备/GUI 依赖 |
| 会话与取消 | core 单一 actor；沿用有界意图/语音队列、背压与代次取消，不引入线程池并行争抢话轮 |
| 模型/语音/外形 | 既有 Ollama/KoboldCpp/DeepSeek、TTS、ASR、audio/capture、VTS/OBS adapter；通过端口隔离外部服务，推理实现可独立部署 |
| 持久数据 | memory 按角色隔离 JSONL；migrate 只处理离线格式转换，不连接外部数据库，不自动确认事实 |
| 桌面 | desktop 只消费控制协议和投影；业务状态不复制到 GUI，保持已有角色/外形/场景流程 |
| 平台动作 | safety/automation/audit 保留 permit、急停、目标范围和审计；真实 Windows 捕获/键鼠 adapter 尚未迁移，默认保持关闭 |
| 开发工具 | Cargo 管理编译/测试/基准，现有 Windows 启动/打包 PowerShell 保留；不新增 Python 工具。是否把全部 PowerShell 逻辑也移入 Rust xtask 属于后续工具整合 |

优先复用现有协议与模块，不重建另一套运行时。迁移器复用已缓存 flate2/ring，新增缓存中的 time 作明确时区解析，不自行编写日期运算或密码学。memory 只作为 migrate 的测试依赖验证输出可用；生产迁移工具仍只使用 domain/config 与格式库。没有新增 unsafe，也没有依靠语言本身宣称高性能或完全安全。

## 功能覆盖与剩余优先级

| 旧范围 | 当前合同与证据 | 下一步 |
| --- | --- | --- |
| 配置/领域/会话/文本 | 当前 Rust 主链、生命周期与文本回归；本轮补离线校验与默认语音清洗 | 为旧配置未映射的用户选项明确替代/提示；随后按依赖整体退役旧入口与测试 |
| LLM/TTS/ASR/VTS/音频 | 当前模型适配器与原生音频 feature 已存在；设备验收不能由 mock 代替 | 补关键协议合同，尤其 VTS 热键枚举/参数诊断与真实设备待用户验收 |
| 记忆 | JSONL 管理/角色隔离；旧备份文本、向量、metadata 保留；可配置 Ollama 语义召回与当前角色原子重建 | 继续实体提取/来源关系、多模态描述与特征查询，原生 Chroma 读取；再退役 Python 记忆模块 |
| GUI/人物/场景 | 当前原生桌面、离线预览、收藏、素材册与场景已取代旧主线 GUI | 验收完整首次设置/重启故事；迁移旧悬浮字幕、全局快捷键等独有核心体验 |
| 视觉/游戏/自动化 | 当前只读视觉、permit/审计、dry-run/replay 与插件协议；旧 main 可接 AgentManager，agent 再接 ActionEngine | 保留旧真实动作/模板匹配核心合同，先 mock+回放闭环，再做受控平台 adapter；不能用删除冒充迁移 |
| 自然行为/优化/热重载旁支 | 技术基线明确它们未进入当前 Rust 主链，旧报告的固定优化数字不继承；仅测试引用不足以证明废弃 | 按真实调用与当前体验需求整合，删除前列出不再需要的合同；不机械重写所有 *_optimized 变体 |
| Python 诊断/测试/依赖 | 工具索引明确为旧实现工具；当前 CI 已使用 Rust | 逐项用 Rust CLI/原生回归替代；最后移除 main.py/requirements.txt，建立零 Python 门禁 |

这些状态是静态调用和当前产品入口的证据，不证明用户从未使用某个旧脚本。完全 Rust 化不能通过先删目录、再把缺失能力都标为“未来规划”实现。

接下来按完整功能切片继续：1）Rust 高级记忆的向量/实体/多模态模型、持久化与旧数据桥接；2）受控桌面捕获与动作适配器，复用 permit/审计和急停；3）Rust 模板匹配与游戏回放；4）桌面集成旧独有核心体验，整体退役旧入口、脚本、测试及依赖。领域/端口与平台实现分离，先合成/mock 对照再实机；同类边缘审计延期，正常回归跟随功能。所有必迁移核心能力仍在目标范围，不再需要就保留与否重复询问用户。最终验收需要 `git ls-files '*.py'` 为零、Python 依赖/命令为零、当前核心与桌面构建/测试/lint通过，并逐项说明旧功能保留或有依据退役、数据迁移和设备未测范围。

## 性能测量

命令（本机已有 LLVM，只作为该次 GNU 编译的参数）：

```powershell
cargo bench -p ai-ex-text --bench sentence_stream --locked --offline -j 1 `
  --config 'build.rustflags=["-C","dlltool=D:/LLVM/bin/llvm-dlltool.exe"]'
```

Windows x86_64 / Windows build 26200；rustc 1.96.0 GNU；CPU 环境报告 AMD Family 25 Model 97 Stepping 2。Release 优化构建，冻结改动前 Rust 分句算法与当前实现，均使用同一输入和结果检查，预热一次、7 次计时取中位数。没有测 Python、模型推理、音频设备、GUI FPS、端到端首音或 p95；用户机器未隔离，数值只作为可复现微基准记录。

| 汉字数 | 每块字数 | 原扫描中位数 μs | 增量中位数 μs | 比值 |
| --- | --- | --- | --- | --- |
| 1,024 | 1 | 1,498.8 | 26.1 | 57.43× |
| 1,024 | 16 | 184.2 | 11.4 | 16.16× |
| 4,096 | 1 | 22,980.0 | 98.9 | 232.36× |
| 4,096 | 16 | 1,442.5 | 20.1 | 71.77× |
| 16,384 | 1 | 371,586.0 | 632.3 | 587.67× |
| 16,384 | 16 | 36,904.8 | 199.9 | 184.62× |

该场景故意直到末尾才有句号，用于暴露历史缓冲区反复扫描；正常短句的比例可能不同。源码基准固定在 `crates/ai-ex-text/benches/sentence_stream.rs`，不是历史报告自报的性能承诺。

### 记忆检索与启动缓存

```powershell
cargo bench -p ai-ex-memory --bench recall --locked --offline -j 1 `
  --config 'build.rustflags=["-C","dlltool=D:/LLVM/bin/llvm-dlltool.exe"]'
```

同一本机环境，Release、单线程运行时、1 次暖身与 7 次中位数、top-k=5。冻结 `92a15d7` 的旧词法上下文检索，与新缓存路径逐条比较完整返回文本和顺序；三个规模全部一致。合成样本含角色与 live_event 过滤、相同时间戳，文件系统缓存已预热。下列时间不含返回对象析构；没有模型、网络或向量精度测量。

| 记录数 | 旧打开 ms | 缓存打开 ms | 旧检索 ms | 缓存检索 ms | 检索比值 |
| --- | --- | --- | --- | --- | --- |
| 100 | 1.054 | 2.506 | 0.901 | 0.100 | 9.00× |
| 1,000 | 2.367 | 16.696 | 10.937 | 1.122 | 9.75× |
| 10,000 | 17.379 | 179.362 | 131.764 | 15.504 | 8.50× |

缓存把重复词项构建成本移到首次加载，启动更慢且需要额外内存；以上是具体取舍，不是端到端对话或语义模型加速承诺。真实模型召回质量、向量维度扩展和更大用户记忆库尚未实测。
## 验证记录

首批迁移（`92a15d7`）实际结果：核心 workspace 全 feature 测试 **250 passed / 1 ignored / 0 failed**；独立桌面 **118 passed / 6 ignored / 0 failed**。忽略项为真实音频设备与可选离屏导出，本轮未额外运行。两套严格 Clippy、两套 cargo fmt、31 包架构检查和 12 项架构回归通过。服务、迁移工具与桌面 debug 构建通过；迁移帮助/版本、配置迁移 → 离线校验 → 拒绝覆盖的 CLI smoke 通过。

默认 GNU 环境最初导致 Release 的 dlltool/assembler 和桌面的 shlwapi 链接失败；已分别用本次构建 LLVM dlltool 参数、仓库 target 下的正确导入库修复并验证。导入库由现有 LLVM 读取安装 Windows DLL 的 380 个命名导出生成，未修改系统 DLL；桌面测试/构建仅本次进程设置 LIBRARY_PATH。临时支持目录 `target/linker-support-ccd63f4f489e4d1c8aac57946a951649` 供本机后续验证保留，不属于分发文件。

真实服务、真实窗口/麦克风/扬声器与真实旧数据库未运行；完整跨设备功能仍需后续验收。自动验收仅使用独立临时目录、合成文本/备份与本机 mock 服务。公开发布由用户本人处理。

### 语义记忆增量验收（2026-10-05）

- 核心：`cargo test --workspace --all-features --locked --offline -j 1 -- --test-threads=1`，**284 passed / 1 ignored / 0 failed**。
- 桌面：独立清单同参数测试，**118 passed / 6 ignored / 0 failed**。忽略项仍为真实音频与可选离屏导出。
- 服务、迁移工具与独立桌面 debug 可执行文件均构建通过；没有启动真实 GUI。
- 两套严格 Clippy `--all-targets -- -D warnings`（核心加 `--all-features`）、两套 fmt、31 包架构检查与 12 项检查脚本回归均通过。
- 新增 34 项回归含：13 项语义/持久化/角色/模型空间/更正遗忘/慢编码器测试，旧 gzip/JSON 备份向量与 metadata 桥接，真实服务子进程连接本地 HTTP mock 的按角色重建，以及父子进程对同文件的写入租约竞争。
- 永久 pending 的编码器在两秒预算后仍保存文本；重建中途失败不发布任何部分向量；竞争写入在请求模型之前拒绝，原 JSONL 字节保持不变。只读复核提出的并发覆盖与长超时问题均已修复并验证。

全部新增用例使用独立合成数据和本地 mock；没有运行真实 embedding 模型、读取/迁移用户记忆、操作 GUI 或真实设备。没有新增 Cargo 依赖，没有删除 Python 文件；实体提取、实体来源关系、多模态与真实平台自动化仍在必迁移范围。仅本地工作，没有 push、PR、发布或部署。
