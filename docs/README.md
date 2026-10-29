# 文档索引

当前主线是 Rust 数字伙伴工作室。新用户从[项目 README](../README.md)开始，准备反馈时使用[整体验收流程](MANUAL_TEST_PLAN.md)。

## 使用与验收

| 文档 | 内容 |
| --- | --- |
| [Windows 便携版](PORTABLE_WINDOWS.md) | 解压双击、首次体验、数据位置与更新 |
| [整体验收流程](MANUAL_TEST_PLAN.md) | 独立测试配置、操作顺序、预期结果、故障记录 |
| [桌面使用](DESKTOP_USER_GUIDE.md) | 首次设置、托管/独立服务、诊断与重连 |
| [记忆工作台](MEMORY_WORKSPACE.md) | 确认笔记、实体候选与来源核对、媒体及标签筛选、更正与单条遗忘的数据边界 |
| [模型后端](MODEL_BACKENDS.md) | 模型地址、凭据、配置与健康检查 |
| [声音与表达](SPEECH_PRESENTATION.md) | 语音、字幕、口型、取消与已知限制 |
| [当前版本说明](releases/0.8.0-alpha.1.md) | 语义与来源记忆、显式迁移维护、服务和桌面的合成回放 |
| [模板回放](TEMPLATE_GAME.md) | 合成清单、暂停/停止、聊天语音仲裁、可选区域观察与有界搜索 |

## 角色与自由组合

- [角色收藏](CHARACTER_LIBRARY.md)：新建、独立复制、版本与来源。
- [角色包](CHARACTER_PACKS.md)：人格清单、预览应用与分享。
- [图片外形包](APPEARANCE_PACKS.md)：内置插画与自定义图片的区别、状态映射、素材校验与导入。
- [场景组合](SCENE_PACKS.md)：角色与外形保存、资源携带和启动恢复。
- [人格与记忆](PERSONA_MEMORY.md)：身份分区、分类记录、实体来源、结构化媒体与召回边界。

## 架构与扩展

- [项目目录](PROJECT_LAYOUT.md)：主线源码、共享模块、历史参考、资源与生成物的归属。
- [当前架构](ARCHITECTURE.md)：实际模块、状态所有权、依赖方向和扩展边界。
- [数字人演进路线](DIGITAL_HUMAN_ROADMAP.md)：后续载体、行为、关系与组合能力。
- [本地控制协议](CONTROL_PROTOCOL.md) · [插件协议](PLUGIN_PROTOCOL.md)
- [Bilibili 连接](BILIBILI_CONNECTOR.md) · [离线直播模拟](SIMULATED_LIVE.md)
- [开发约定](../CONTRIBUTING.md) · [架构检查脚本](../tools/check_architecture.ps1)

## 历史与迁移资料

- [原生模型与零 Python 迁移](NATIVE_MIGRATION.md)：M0 阻塞门槛、固定后端路线、分版交付、能力覆盖矩阵与验证证据格式。
- [M0 原生验证证据](NATIVE_M0_EVIDENCE.md)：本机已执行的模型、进程与工程检查，以及尚未通过的门槛。
- [迁移工具与显式多模态快照](../tools/README.md#显式多模态快照导入)：独立输出、dataclass JSON 数组格式与数据限制。
- [Chroma 桥接](CHROMA_BRIDGE.md)：显式只读快照、离线导入及原生库验收边界。
- [完全 Rust 化进展](RUST_ONLY_MIGRATION.md)：离线校验、旧备份与向量迁移、可选语义召回、实体及多模态切片、原子重建、实测基准及剩余迁移合同。

[技术基线](CORE_TECHNICAL_BASELINE.md)、[Python 退役计划](LEGACY_RETIREMENT.md)、[0.7.1 架构与维护整理](releases/0.7.1-alpha.1.md)、[0.7 一号人物与素材工作室](releases/0.7.0-alpha.1.md)、[0.6 二次元工作室](releases/0.6.0-alpha.1.md)、[0.5 记忆工作台](releases/0.5.0-alpha.1.md)、[0.4 人物工作室](releases/0.4.0-alpha.1.md)和[早期工作室说明](releases/0.2.0-alpha.1.md)保留历史进展。

[旧用户指南](ai_vtuber_user_guide_zh.md)、[旧安装说明](setup_guide.md)和[旧全双工指南](full_duplex_user_guide.md)主要针对 Python 路线，其中的 config.json、Python 启动方式及旧功能描述不能直接用于当前 Rust 验收。
