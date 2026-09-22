# 文档索引

当前主线是 Rust 数字伙伴工作室。新用户从[项目 README](../README.md)开始，准备反馈时使用[整体验收流程](MANUAL_TEST_PLAN.md)。

## 使用与验收

| 文档 | 内容 |
| --- | --- |
| [Windows 便携版](PORTABLE_WINDOWS.md) | 解压双击、首次体验、数据位置与更新 |
| [整体验收流程](MANUAL_TEST_PLAN.md) | 独立测试配置、操作顺序、预期结果、故障记录 |
| [桌面使用](DESKTOP_USER_GUIDE.md) | 首次设置、托管/独立服务、诊断与重连 |
| [记忆工作台](MEMORY_WORKSPACE.md) | 查看来源、确认笔记、更正与单条遗忘，以及实际边界 |
| [模型后端](MODEL_BACKENDS.md) | 模型地址、凭据、配置与健康检查 |
| [声音与表达](SPEECH_PRESENTATION.md) | 语音、字幕、口型、取消与已知限制 |
| [当前版本说明](releases/0.7.1-alpha.1.md) | 共享界面模块、依赖检查和目录索引的维护更新 |

## 角色与自由组合

- [角色收藏](CHARACTER_LIBRARY.md)：新建、独立复制、版本与来源。
- [角色包](CHARACTER_PACKS.md)：人格清单、预览应用与分享。
- [图片外形包](APPEARANCE_PACKS.md)：内置插画与自定义图片的区别、状态映射、素材校验与导入。
- [场景组合](SCENE_PACKS.md)：角色与外形保存、资源携带和启动恢复。
- [人格与记忆](PERSONA_MEMORY.md)：身份分区、分类记录与召回边界。

## 架构与扩展

- [项目目录](PROJECT_LAYOUT.md)：主线源码、共享模块、历史参考、资源与生成物的归属。
- [当前架构](ARCHITECTURE.md)：实际模块、状态所有权、依赖方向和扩展边界。
- [数字人演进路线](DIGITAL_HUMAN_ROADMAP.md)：后续载体、行为、关系与组合能力。
- [本地控制协议](CONTROL_PROTOCOL.md) · [插件协议](PLUGIN_PROTOCOL.md)
- [Bilibili 连接](BILIBILI_CONNECTOR.md) · [离线直播模拟](SIMULATED_LIVE.md)
- [开发约定](../CONTRIBUTING.md) · [架构检查脚本](../tools/check_architecture.ps1)

## 历史与迁移资料

[技术基线](CORE_TECHNICAL_BASELINE.md)、[Python 退役计划](LEGACY_RETIREMENT.md)、[0.7 一号人物与素材工作室](releases/0.7.0-alpha.1.md)、[0.6 二次元工作室](releases/0.6.0-alpha.1.md)、[0.5 记忆工作台](releases/0.5.0-alpha.1.md)、[0.4 人物工作室](releases/0.4.0-alpha.1.md)和[早期工作室说明](releases/0.2.0-alpha.1.md)保留历史进展。

[旧用户指南](ai_vtuber_user_guide_zh.md)、[旧安装说明](setup_guide.md)和[旧全双工指南](full_duplex_user_guide.md)主要针对 Python 路线，其中的 config.json、Python 启动方式及旧功能描述不能直接用于当前 Rust 验收。
