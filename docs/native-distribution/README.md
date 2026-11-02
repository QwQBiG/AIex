# Windows CPU 原生分发输入

本目录记录 M0 与 0.9 开发候选使用的 Windows CPU 原生组件、固定来源及随附材料。0.9 尚未发布；产品接入、安装向导、干净环境和最终程序包的验收仍按[迁移路线](../NATIVE_MIGRATION.md)执行。

## 固定范围

- [运行文件](runtime-files.json)：Sherpa/ORT 的实际 DLL、固定模型与运行库来源索引。
- [Ollama CPU 文件](ollama-cpu-files.json)：32 个固定路径、大小和 SHA256；其中 28 个 EXE/DLL 已重新核对为 `Ollama Inc.` 的有效 Authenticode 签名。
- [源码输入](sources.json)：已取得并计算摘要的原生源码归档；Sherpa 的依赖归档另外匹配其固定 CMake 预期摘要。
- [许可材料](materials.json)：本地许可和 notices 的路径、来源、字节数及 SHA256。原文保留版权信息；文本统一为 UTF-8、LF。
- [模型声明](models.json)：权重及转换包声明的许可证、固定修订和文件清单来源。它不把 Rust 接口库的许可证当作模型许可证。
- [Rust 工作进程](rust-workers.json)：三类实际工作进程与共用模块的锁定依赖、registry 校验值、作用范围和原始许可材料。

模型权重、运行库和源码归档不进入 Git。正式分发时，打包工具必须按这些文件清单核对实际内容，再放入所需的许可、notices、对应源码及构建材料；新增或替换运行文件后重新生成清单并复核。

## Ollama 的 CPU 子集

运行输入定义为 `v0.40.11` 官方资产 `672866806` 内的明确 CPU 文件集合。数据经 HTTPS 取得，逐条核对 ZIP 本地/中央目录的名称、大小和 CRC，并固定每个展开文件的 SHA256；程序文件再核对发布者签名。CRC 用于条目一致性，发布者签名和固定摘要用于确认程序身份及后续安装内容。

没有下载并验证整个包含 GPU 的 1,586,356,606 字节 ZIP，因此 `whole_archive_verified` 保持 `false`。不使用该归档的整体 SHA 代表子集已经通过校验，也不宣称它是完整官方安装包。CPU 输入不含 CUDA、ROCm、Vulkan 运行库；GPU 包需要单独的文件、许可和实机验收。

四份官方原始 notices 与 Ollama 根许可证一同保留。固定发布工作流使用 `llvm-mingw-20240619`；其构建脚本指定 LLVM `18.1.8` 与 MinGW-w64 `7c9cfe6708cafc83c14a2654308b2db62b126eae`。本目录补齐单独运行 DLL 对应的 LLVM、libc++、libunwind、OpenMP、winpthreads 条款，不用未使用 GPU 依赖的材料代替它们。

## Sherpa、ORT 与中文前端

Sherpa `v1.13.8` 对应 `11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf`。原始运行归档已匹配官方 SHA256；CMake 和发布工作流提供 Windows x64、shared、MT、Release、启用 TTS、关闭 PortAudio 的构建来源。

实际 ORT DLL 报告 `1.28.2`，构建提交短号 `33ca962` 已解析为 `33ca9628233dc8f002435e868d4c2e9f82766ca1`；该提交的 MIT 原文与完整 `ThirdPartyNotices.txt` 均保留。Qwen 与 BGE 工作进程使用独立 DLL 目录，不能因为版本号相同而省略实际文件核对。

Sherpa 启用 TTS 时静态引入 eSpeak，固定源码为 `ed530aa113046142eb5115cf2fc9157854d0ffe1`。其源码 ZIP 已匹配 CMake 预期 SHA，GPL-3.0-or-later、BSD2、Unicode/UCD 和 Apache 原文均在材料中。分发该组合不能只附 Sherpa 的 Apache 文本：必须同时提供适用许可、版权、对应源码和构建脚本；进程隔离用于 DLL 和故障隔离，不替代这些材料。

其余原生依赖包括 kaldi-native-fbank/kissfft、kaldi-decoder/kaldifst、Eigen、OpenFst、simple-sentencepiece、nlohmann/json、piper-phonemize/uni-algo 和 hclust-cpp；具体归档与条款见清单。编译条件决定最终进入程序的文件，实际发布包仍需逐一核对，不将测试、转换或开发工具作为运行依赖带入。

Kokoro 包含词典和 eSpeak 数据。CppJieba 对得上的词典以其固定 Git blob 和 MIT 文本保留来源；Kokoro 自带的扩展用户词典不冒充上游默认小词典。模型包中的转换词典和词表继续保留发布者声明及固定文件摘要。

## Rust 工作进程的材料

`rust-workers.json` 依据独立的 `native-workers/Cargo.lock` 和针对 `x86_64-pc-windows-gnu` 的 `cargo metadata --locked --offline` 生成，覆盖 BGE、Sherpa、Qwen 三类工作进程，以及 worker-support、inference、domain 和 model-catalog。当前 0.9 冻结输入的 247 个 registry 源码归档均在本地重新计算 SHA256，与锁文件逐一匹配；清单记录锁文件本身的摘要。

依赖图中共有 169 个运行依赖包，包含 7 个自有包；163 个包参与构建工具或过程宏，部分包同时属于两者，数量不能相加。`runtime` 沿普通依赖边计算，过程宏及其依赖归入构建工具；`build_or_proc_macro` 沿构建和过程宏工具依赖计算，开发依赖另外标记。本次选定图没有单独的开发依赖。Sherpa 的 Windows UTF-8 manifest 构建引入 `embed-resource` 及其工具依赖；它们不成为需要用户安装的运行 DLL。

原 `.crate` 中的许可、copyright、notices 与嵌套第三方条款保存在 `licenses/rust/<crate>-<version>/`，保留原文件的相对位置。当前 470 份 Rust 材料均核对字节数和摘要，新增九个构建依赖的 16 份许可从已校验的原始归档提取。`ring` 包括 BoringSSL、ISC、once_cell 和 fiat 材料；`onig_sys` 还包括 Oniguruma 的 `COPYING`，不能只保留 Rust 包装层许可。

四个上游归档没有独立 LICENSE：audio-core、npyz、wavekat-core、wavekat-tts。清单保留其完整原始 README 和包清单中的许可声明，并附相应标准条款；npyz 原文中的两位版权人和 WaveKat 原文版权均保留。audio-core 在其双许可中选择 Apache-2.0，不将作者字段改写为版权声明。wavekat-tts 另附与其 crate VCS 提交一致、此前已取得的上游许可 notice。这些上游打包遗漏仍列在 `upstream_packaging_notes`，没有伪造原包包含 LICENSE。

自有包沿用 Cargo 中的 MIT 声明，正文见项目根目录的 [LICENSE](../../LICENSE)。清单以仓库根目录为基准记录该文件的相对路径、字节数和 SHA256，与第三方材料分别核对；源码和程序包交付时一同保留。

## 发布时检查

1. 对实际安装目录逐文件核对清单；文件缺失、大小/摘要不同或签名来源不符时不标记就绪。
2. 将对应许可、版权、notices 和需要提供的源码/构建材料随包交付；模型与程序的材料分别处理。
3. 明确运行组件与源码材料的用途。源码归档可能含上游转换脚本，它们不属于本机官方运行链，也不能由安装程序执行。
4. 在未安装 Python 的 Windows 环境验证便携运行和源码构建，并完成离线主要流程；本目录中的材料取证不替代这些实际测试。
