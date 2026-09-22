# 项目目录与维护入口

当前产品由 Rust 服务和原生桌面组成。旧 Python 实现、资源与迁移资料仍保留在仓库；查找当前行为时先进入 `crates/`。模块职责和数据流见[架构说明](ARCHITECTURE.md)，使用入口见[项目 README](../README.md)。

## 主线目录

```text
Cargo.toml / Cargo.lock        核心 workspace 与固定依赖
crates/
  ai-ex-domain/               领域类型与错误
  ai-ex-core/                 会话编排、端口与取消边界
  ai-ex-config/               配置与角色、外形、场景清单
  ai-ex-control/              本地控制协议与客户端
  ai-ex-ui-model/             与窗口无关的桌面状态投影
  ai-ex-service/              服务组合根与命令行入口
  ai-ex-desktop/              独立桌面 Cargo 包与锁文件
  ...                        模型、记忆、声音、舞台及其他适配器
config/                      Rust 配置样例、角色与场景示例
config_examples/             回放事件，以及旧 Python JSON 配置
tools/                       架构检查、打包、令牌工具及旧诊断工具
docs/                        当前指南、协议、架构、版本和迁移记录
.github/workflows/rust.yml    核心与桌面质量检查
```

核心 workspace 当前包含 30 个包；桌面包在根清单中显式排除，使用自己的清单和锁文件单独构建。检查、测试和格式化需要同时覆盖这两个入口。桌面仍通过控制客户端连接服务，独立清单不是绕过架构约束的例外。

按职责定位全部包的表格在[架构模块地图](ARCHITECTURE.md#2-模块地图)。构建与验证命令在[工具索引](../tools/README.md)和[整体验收流程](MANUAL_TEST_PLAN.md)，不用从历史报告拼接命令。

## 桌面内部

`crates/ai-ex-desktop/src/main.rs` 负责启动和页面入口。`app` 协调当前会话及已确认的人格、外形；`worker` 负责通信；`appearance` 负责外形；`preview` 负责离线预览；`setup` 负责连接配置；`ui/theme.rs` 提供各页面共用的视觉样式。

共享样式只依赖窗口绘制基础，不依赖聊天状态、模型或设备。欢迎页、设置页、预览和记忆面板直接使用共享 UI 模块。`ui/test_support.rs` 仅在测试时提供离屏绘制、控件定位和输入事件辅助，页面测试不依赖其他页面的测试模块。外形素材位于该包的 `assets/`，预览示例位于 `examples/`；调整渲染文件位置时需要同时检查示例和资源嵌入路径。

同一功能的实现与测试在对应 Rust 模块附近维护。部分桌面模块仍用 `#[path]` 将平铺文件组成子模块；目录整理应保持已有模块所有权和运行边界，不能只移动文件而遗漏示例、测试与文档引用。

## 文档与历史内容

| 位置 | 当前用途 |
| --- | --- |
| [docs/README.md](README.md) | 当前文档导航 |
| `docs/releases/` | 已发布版本的交付范围与验证记录；后续未发布改动不回写为旧版修复 |
| `docs/protocol/` | 协议样例与跨模块契约资料 |
| [src/](../src/README.md)、[tests/](../tests/README.md)、根 `main.py` 和 `requirements.txt` | 历史 Python 源码、测试及依赖参考，不是便携版启动路径 |
| `config_examples/*.json` | 旧配置参考；同目录的 JSONL 回放文件仍供 Rust 工具使用 |
| 根目录的历史报告 | 迁移背景，不作为当前功能或测试通过的证据 |

旧代码和资源的移除条件见[退役清单](LEGACY_RETIREMENT.md)。目录标为历史，不表示其数据已迁移或可以删除。

## 资源、私有数据与生成物

| 位置 | 边界 |
| --- | --- |
| `crates/ai-ex-desktop/assets/` | 当前程序内嵌人物素材，随桌面代码版本化 |
| `assets/`、`models/` | 旧路线与可选功能资源；是否使用由具体配置决定，不整体装入便携包 |
| `memory_db/`、根 `config.json` | 可能含历史记忆与本机配置，保留原文件，不按“目录整洁”自动清理 |
| `config/ai-ex.local.toml`、`config/control.token`、根 `token.json` | 本机配置与令牌，按现有忽略规则处理，不随角色或场景分享 |
| `target/`、`crates/ai-ex-desktop/target/` | 两个构建目录，也可保存临时验证证据，不提交生成物 |
| `dist/` | Windows 便携包输出，发布脚本仅装入明确列出的分发文件 |
| `logs/`、Python 缓存 | 可由运行重新创建，不把“曾清理”当作当前不存在的证明 |

根目录的历史资源并非全部被 Git 忽略。判断能否分享时以实际跟踪状态和内容为准，不能只凭目录名称。此次结构说明不改变已有数据、模型或旧资源的存放位置。

## 结构变更的检查范围

新增 Cargo 包需要明确所属 workspace、实际清单路径和允许依赖；同名包不能借不同本地路径绕过依赖边界。检查器同时核对 `crates/` 直接子目录里的包清单是否登记。

移动模块时保留存储键、协议版本和人物标识，检查所有模块声明、资源路径、示例及文档链接，再执行相关测试。界面代码的目录调整还需验证启动与关键操作；离屏结果不能替代真实设备验收。

Git 换行规则集中在根 `.gitattributes`：Rust、TOML、Markdown 等使用 LF，PowerShell 和 Windows 启动脚本使用 CRLF。Rust 格式继续使用 `cargo fmt`。
