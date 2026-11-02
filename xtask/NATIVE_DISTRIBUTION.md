# 原生发行工具

所有入口使用 `cargo xtask`，输出必须是尚不存在的新路径。工具只读取显式输入，按大小与 SHA256 核对文件；不扫描个人目录、不下载模型、不执行嵌套源码归档。

## 构建顺序

1. 完成源码、两份锁文件、原生 worker、发行工具和固定许可材料，提交为 `SourceCommit`。该提交必须包含 `xtask`、`native-workers` 及 `docs/native-distribution`。
2. 从这个 40 位提交生成 Git ZIP，排除派生目录文件及已跟踪的个人配置、记忆目录：

   ```powershell
   git archive --format=zip --output="$projectArchive" "$sourceCommit" -- . ':(exclude)config/native-catalog.json' ':(exclude)config.json' ':(exclude)memory_db'
   ```

   ZIP 保留 Git 写入的提交注释。源码工具核对注释、必要源码/锁文件/工具，以及固定分发文档的原始字节；不要求包含生成后的目录文件。上述排除只作用于发行归档，不修改原始配置、记忆或 Git 历史。
3. 准备 Sources 输入，执行 `cargo xtask native-sources --spec <sources-input.json> --output <new-sources.zip>`。
4. 以同一源码构建三类原生 worker。准备运行文件输入，将上一步源码 ZIP 的大小和 SHA256 写入 `corresponding_sources`，执行 `cargo xtask native-bundle --spec <bundle-input.json> --output <new-runtime.zip> --manifest <new-runtime.json>`。
5. 以实际 Runtime ZIP 和 RuntimeManifest 执行 `cargo xtask native-catalog --distribution docs/native-distribution --runtime-manifest <new-runtime.json> --runtime-zip <new-runtime.zip> --output config/native-catalog.json`。已有文件需要由发布人员先审查并选择新的生成位置，不由工具覆盖。
6. 用生成的目录编译服务和桌面，完成验收，再形成发行提交。除该派生目录和发行材料外，发布源码应与 `SourceCommit` 一致。

RuntimeManifest、Runtime ZIP 与生成的模型目录属于 Sources ZIP 之外的派生产物。不得把最终目录再次放进源码 ZIP 或构建材料：目录引用 Runtime SHA，而 Runtime 包含源码 ZIP，这会形成相互依赖。此流程固定输入及对应源码关系，不承诺 ZIP 位级可重复。

## Sources 输入

JSON 的顶层字段为：

- `schema_version`：`1`。
- `distribution_directory`：包含固定 `sources.json`、`rust-workers.json` 等材料的目录。
- `project_commit`：40 位提交。
- `project_archive`：项目 Git ZIP 的文件记录，ZIP 内源码从仓库根目录开始，不能带额外前缀。
- `archives`：覆盖固定 `sources.json` 和 `rust-workers.json` 的完整记录列表，数量由这两份清单的实际条目决定；每个原生源码归档及 registry `.crate` 仍分别核对来源、摘要和必要大小。依赖升级后必须同步更新完整输入列表，不能只修改数量。
- `build_materials`：明确的构建文件记录，最多 64 条。必须保留 Ollama 对应的三份 `llvm-mingw-20240619-build-*.sh` 脚本，目的路径是 `build/<原文件名>`。其他项目材料可以按原相对位置放在 `build/` 下。

每条记录包含 `source`（本机文件路径）、`path`（ZIP 内相对路径）、`bytes`、`sha256`、`source_url`（没有独立上游地址时为 `null`）。原生归档的目的路径在 `native/` 下；registry 归档为 `rust/<crate>-<version>.crate`。项目归档可用 `project/source.zip`。

工具保留归档原字节，附加 `source-inputs.json`，只记录目的路径、摘要和来源，不把本机输入路径写入发行内容。现有输出、缺失输入、摘要错误、重复目的路径、缺失源码、提交注释不匹配、固定文档不同或误含生成目录都会失败。

## Runtime 输入增量

原有显式 worker、DLL、Ollama 和许可材料字段保持兼容。正式包增加可选的 `corresponding_sources` 对象，包含 `source`（已完成的 Sources ZIP 路径）、`bytes`、`sha256`；后两者均使用该文件的实际计算结果。

源码 ZIP 进入 `sources/corresponding-sources.zip`，在 RuntimeManifest 中声明为 `SourceArchive`。安装器只保存这个文件，不递归展开，也不作为推理进程或 DLL 使用。开发包可省略此字段；公开分发前需核对正式对应源码与许可材料。
