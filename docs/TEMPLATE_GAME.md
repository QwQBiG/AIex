# Rust 模板匹配与受控游戏回放

当前源码提供独立的 `--template-game MANIFEST.json`：读取指定的 PNG/JPEG 帧，匹配游戏 profile 中选定的模板，通过现有 `AutomationCoordinator` 完成捕获、移动和点击的 **dry-run**，输出可核对的 JSON 报告。它在加载服务配置前分流，无需 `--config`，不会初始化模型、记忆、控制令牌、音频或窗口连接。

本入口验证离线图像与动作合同。实际帧来自清单指定的图片文件，动作由 `DryRunAutomationPort` 记录；真实 Windows 屏幕捕获、键鼠 adapter 和桌面操作入口仍待迁移。50 ms 是输入时间戳的默认最小间隔，没有实时等待或 20 Hz 的实际运行保证。

## 准备一个独立回放目录

以下目录和图片均由使用者明确准备；测试使用非恒定的合成图像，不读取正在运行的游戏窗口。

```text
replay/
  manifest.json
  game/
    profile.json
    templates/
      target.png
  frames/
    000.png
    050.jpg
```

`game/profile.json` 沿用既有 `assets/games/<game>/profile.json` 格式。例如：

```json
{
  "display_name": "Synthetic template replay",
  "description": "Explicit offline image fixture",
  "vlm_prompts": [],
  "default_templates": {
    "primary_target": "target.png"
  },
  "action_cooldowns": {}
}
```

也可显式选择已有 [Cookie Clicker profile](../assets/games/cookie-clicker/profile.json)。`target` 必须精确匹配 `default_templates` 的键；不按文件名猜测别名。模板从 **profile 所在目录的 `templates/` 子目录**读取，文件名必须是单一 PNG/JPG/JPEG 名称；路径穿越、设备名和解析到该目录外的链接均拒绝。

`display_name`、`description`、`vlm_prompts`、`default_templates` 是 profile 字段，`action_cooldowns` 可省略。提示词和冷却时间保留在 profile 中；本入口不会调用 VLM，也不把 `action_cooldowns` 自动换算为反射循环间隔。循环间隔由 manifest 的 `interval_ms` 明确控制。

## Manifest schema 1

将下面内容保存为 `replay/manifest.json`。图片需要自行准备；示例坐标适用于图像原点位于桌面物理坐标 `(-1800, 100)`、图像像素对应两倍物理像素的捕获副本。

```json
{
  "schema_version": 1,
  "profile": "game/profile.json",
  "target": "primary_target",
  "mode": "click_repeat",
  "desktop": {
    "left": -1920,
    "top": 0,
    "width": 3840,
    "height": 2160
  },
  "threshold": 0.9,
  "max_work": 250000000,
  "interval_ms": 50,
  "max_frames": 2,
  "max_actions": 4,
  "search_region": {
    "x": 0,
    "y": 0,
    "width": 640,
    "height": 360
  },
  "frames": [
    {
      "image": "frames/000.png",
      "timestamp_ms": 0,
      "origin_x": -1800,
      "origin_y": 100,
      "scale_x": 2.0,
      "scale_y": 2.0
    },
    {
      "image": "frames/050.jpg",
      "timestamp_ms": 50,
      "origin_x": -1800,
      "origin_y": 100,
      "scale_x": 2.0,
      "scale_y": 2.0
    }
  ],
  "report": "report.json"
}
```

`schema_version`、`profile`、`desktop` 和非空 `frames` 必需。manifest 与每帧条目中的未知字段拒绝解析。`profile`、帧的 `image` 和 `report` 相对 **manifest 所在目录**解析，也接受绝对路径；它们不随进程工作目录重新解释。`report` 可省略，报告仍输出到 stdout；指定报告路径的父目录须已存在。

| 字段 | 默认值与约束 |
| --- | --- |
| `schema_version` | 必须为 `1` |
| `target` | `primary_target`；必须存在于 profile，且满足控制器的非空、有界目标约束 |
| `mode` | `click_once`；可选 `click_repeat`、`hover` |
| `desktop` | 物理桌面矩形；`left/top` 为有符号坐标，`width/height` 必须为正 |
| `threshold` | `0.7`；有限数，范围 `[0,1]` |
| `max_work` | 每次搜索 `250000000`；必须为正，表示灰度像素对乘法预算；耗尽即报错，不采用未完成搜索的暂定命中 |
| `interval_ms` | `50`；CLI 接受 `1..=60000` |
| `max_frames` | 预筛选后可用帧数；显式值范围 `1..=256` |
| `max_actions` | 预筛选后帧数的两倍；显式值范围 `1..=4096`，只计移动和点击 |
| `search_region` | 省略时搜索整帧；使用图像局部坐标，宽高必须为正且起点在帧内，右下边界裁剪到图像 |
| `frames[].timestamp_ms` | 必需；无符号离线时间，整个输入必须严格递增 |
| `frames[].origin_x/origin_y` | 各为 `0`；图像左上角对应的物理桌面坐标 |
| `frames[].scale_x/scale_y` | 各为 `1.0`；每个图像像素对应的物理像素数，必须有限且为正 |

模板宽、高分别限制为 `20..=200` 像素，必须含有非恒定灰度细节。匹配使用灰度归一化相关性，常量窗口按未命中处理；模板大于有效搜索区域也按未命中处理。几何缩放参数只用于映射坐标，不会把模板或图片自动缩放到不同分辨率。

## 执行与坐标

在仓库根目录使用已具备依赖和链接器的 Rust 环境：

```powershell
cargo run -p ai-ex-service --locked --offline -j 1 -- --template-game 'replay/manifest.json'
```

已有可执行文件也可直接运行；绝对清单路径可以从其他工作目录调用，无需服务配置文件：

```powershell
& 'D:\path\to\ai-ex-service.exe' --template-game 'D:\my-test\replay\manifest.json'
```

每个实际处理的帧都经过同一个 coordinator：`CaptureScreen → 模板匹配 → MovePointer → Click`。捕获请求从 dry-run 帧队列取出一个已加载的图片；`hover` 省略点击。三个动作分别申请对应 permit，保留 requested/authorized/completed 或拒绝/失败审计。

返回的匹配矩形已包含搜索区域偏移。取其中心 `(cx, cy)` 后，只做一次物理坐标映射：`absolute_x = origin_x + cx * scale_x`，`absolute_y = origin_y + cy * scale_y`。再按 `normalized_x = (absolute_x - desktop.left)/(desktop.width - 1)` 和高度对应算式归一化；某轴只有一个物理像素时，该轴归一化值为 `0`。匹配坐标超出桌面即失败；转换为 `MovePointer` 的 `f32` 前检查有限性及 `[0,1]` 范围。负桌面原点和 DPI/下采样比例通过这些显式字段表示，不重复加 ROI 偏移。

## 模式、间隔与追踪

- `click_once`：命中后移动并左键点击，只有点击成功且完成审计持久化成功才进入 `completed`；完成后不再捕获或执行动作。
- `click_repeat`：每个到期帧命中后移动并左键点击，直到输入结束或预算耗尽。
- `hover`：每个到期帧命中后只移动指针，继续处理后续帧。

CLI 在解码前按 `interval_ms` 预筛选：保留首帧，后续帧距离上一个保留帧达到间隔才保留，图片、时间戳和 geometry 一起筛选。例如时间 `0/10/60 ms`、间隔 `50 ms` 时，仅加载并处理 `0/60 ms` 两帧。所有原输入时间仍须严格递增。报告中的 `skipped_frames` 只统计间隔筛选跳过的帧，不包含完成后尚未处理的帧。

控制器 API 同样按上次实际捕获时间判断间隔；过密调用返回 `waiting`，不消费帧、不推进上次捕获时间。本入口没有 `sleep`，时间戳决定逻辑推进，不代表按墙钟速度播放。

命中后使用目标中心附近至多 `200×200` 的追踪区域，并与用户固定 `search_region` 相交。连续三次追踪未命中后清除追踪区域，下一帧回到原固定区域或整帧搜索。连续十次未命中报告 `lost`，不产生指针动作，也不会因为 `lost` 自动终止；后续帧仍可重新命中。命中会清零未命中计数。

## 预算与失败报告

CLI 最多接受 256 个输入帧；manifest 和 profile 各限 1 MiB。每张图片输入限 16 MiB、单边不超过 8192 像素且总像素不超过 16777216。预筛选后帧的 RGBA 数据合计限 128 MiB；此数不包括解码中间缓冲和匹配工作区。输出 JSON 报告限 8 MiB。

`max_frames` 约束捕获，`max_actions` 单独约束实际或可能已执行的指针动作。一帧点击通常消耗一次捕获和两次指针动作；若只剩一个指针动作，移动后即停止，不能把未发生的点击记作完成。预算耗尽进入 `budget_exhausted`，后续不再捕获。适配器在执行中报错时，动作是否已经发生无法由错误证明，计数保守计入并停止；不自动重试。

移动失败、急停、坐标/匹配失败或任何完成审计未持久化，都使控制器进入 `failed`，阻止后续点击和捕获。错误包含失败阶段、可用的执行前/执行中分类、动作 ID 与已取得的 receipt 摘要；完成审计失败的 receipt 明确保留 `completion_audit_persisted=false`。即使适配器已返回成功，也不会把缺少完成审计的点击报告为 `completed`。

stdout 输出完整 JSON：顶层含 `execution_mode="dry_run"`、输入/筛选计数、最终 outcome、`steps`、`failure`、`audit`。每步含状态、匹配分数/矩形、实际搜索区域、连续未命中数、累计帧/指针动作数、绝对/归一化坐标和该步 receipts，不包含原始 RGBA。失败那一步的部分报告位于 `failure.report`，此前正常步骤保留在 `steps`。运行失败会在输出报告后返回非零退出码。

指定 `report` 时，使用新文件创建和同步写入；已有文件在执行前拒绝，并发创建也不能覆盖。执行后的报告先输出 stdout，再尝试写文件；目录不可写或写入失败仍保留 stdout 中的执行证据，并返回非零退出码。输入校验失败或预检查发现已有报告时，尚未执行动作，不会生成执行报告。报告审计属于本次有界离线回放，不写入真实服务的审计库。

## 可暂停的 Rust 会话

需要从另一个任务暂停反射时，使用 `ReflexSession::new(controller)` 包住控制器，并通过 `session.control()` 取得可克隆的 `ReflexControl`。`session.step(...)` 返回 `ReflexSessionReport`：正常推进为 `advanced`，暂停、停止或旧代次失效为 `interrupted`；后者保留当前步已取得的 receipts、计数与类型化原因。`session.state()` 是执行进度，暂停/停止模式从 control snapshot 读取。

`pause_for_chat()` 暂停、`resume()` 显式恢复、`invalidate_target()` 使旧工作失效，都会推进相关代次；`stop()` 对这个 control session 永久生效，恢复需要新的 session 与 controller。已暂停时重复暂停、运行时重复恢复是幂等操作。`invalidate_target()` 不会替换 matcher 或 target；这个 wrapper 的目标固定，geometry 在每步开始时提供。

同一步只取得一次租约，跨越捕获、匹配、移动和点击的所有等待。coordinator 在请求审计前与授权审计后、真正派发前检查同一个租约；捕获/移动返回后也检查它。即使暂停与恢复已经完成，旧代次仍不能继续点击。暂停/恢复发生在两步之间时，下一步会立即清除旧追踪 ROI；保留总 misses、帧/动作预算和捕获间隔，下一次到期必须重新 capture。

已派发的动作可能继续完成，不能把逻辑暂停说成撤回原生输入。正常暂停等待当前动作及审计返回，保留真实回执和消耗。完成审计失败、适配器失败或控制拒绝审计未持久化仍然进入 `failed`。已经成功且完成审计返回的 `click_once` 保持 `completed`，迟到的暂停不会让它恢复后再点击。

直接取消或丢弃已开始的 step future 会使 controller 立即进入 `failed`，后续禁止续跑。派发前取消不增加未派发动作的计数；派发后取消保守计入一个可能已执行的捕获/指针动作。取消不伪造完成 receipt 或审计，当前 future 的完整报告也无法再返回。需要保留完整证据时，调用 `control.stop()` 并继续等待 step 结束。未 poll 的 future 尚未开始，不改变状态或审计。

当前提供的是纯 Rust SDK 与合成时序验收；服务尚未持有与真实聊天共同运行的 reflex session。30 秒聊天静默恢复、VLM/队列仲裁及 GUI 接线仍待后续，不能把本 API 写成已启用的聊天或真实游戏功能。现有 raw `run_reflex_step` API 保留兼容，其动作取消也使用同一失败保护。

## 当前验证范围

首个离线回放检查点确认 `ai-ex-automation` 的 46 项测试和 `ai-ex-service` 的 46 项测试通过。其中 12 项 `reflex_workflow` 集成回归覆盖顺序与审计、单次完成、间隔/hover、预算、移动/点击失败、急停、审计失败、追踪回退、Lost 后恢复、单调时间和帧缓冲所有权；8 组服务 CLI 测试验证实际 Cookie Clicker profile/模板、PNG/JPEG 解码、坐标、完整九条审计、帧调度、报告防覆盖和无配置离线运行。2026-10-20 本检查点的全部 feature workspace 回归共 400 项通过、0 失败、2 项忽略；忽略项是需要真实输出设备的音频测试与等待固定原生二进制的 Chroma 临时库测试。workspace 严格 Clippy（全部 target/feature、`-D warnings`）、格式与 31 个包的架构检查、12 项架构回归也已通过。

随后受控会话检查点通过 `ai-ex-automation` 全部 80 项测试（原 46 项，加 8 项派发 guard、7 项控制状态、19 组会话/取消工作流），以及全部 feature 下的 8 组服务模板 CLI 回归。workspace 全部 target/feature 严格 Clippy 和格式检查通过。新增交错测试使用有界 channel 握手，无实际输入；覆盖两步之间的代次变化、Requested/Authorized/Completed/Rejected/Failed 审计取消、端口取消及 raw/受控单次点击防重复。
合成微基准使用固定 seed `0x41494558`、release 构建、一次预热和七次测量中位数；每次断言矩形/分数相同，并用恰好够用及少一次乘法的预算验证实际工作量：

| 模板 / 输入 | 搜索区域 | 像素对乘法 | 中位耗时 |
| --- | --- | ---: | ---: |
| 20×20 / 96×72 | 整帧 | 1,382,800 | 1.182 ms |
| 同上 | 28×28 ROI | 16,400 | 0.043 ms |
| 100×100 / 128×116 | 整帧 | 3,690,000 | 2.134 ms |
| 同上 | 108×108 ROI | 410,000 | 0.291 ms |

计时包含当前帧的灰度和积分图计算，不含 fixture、模板构造、捕获、审计或输入；这些是小型合成场景，不能外推真实桌面吞吐量。复现：

```powershell
cargo run -p ai-ex-automation --example template_bench --release --locked --offline -j 1
```

本机 GNU 工具链首次 release 构建缺少 `dlltool.exe`，本轮仅在构建进程的 `RUSTFLAGS` 追加 `-C dlltool=D:\LLVM\bin\llvm-dlltool.exe`，使用已经安装的 LLVM 完成构建；没有修改系统或仓库工具链配置。

当前逐窗口精确 NCC 的全帧搜索有明确限制：100×100 模板在 1920×1080 非恒定画面且没有提前完美命中时，共需 17,864,010,000 次像素对乘法，默认 250,000,000 预算只能完成前 25,000 个窗口（约 13.7 行）。普通达到阈值的候选不会提前结束全局最优搜索；预算耗尽会报告 Match 失败并停止，不能当作普通未命中或已完成全高清验收。200×200 ROI 的同尺寸模板最坏需要 102,010,000 次，在默认预算内。当前应使用明确的搜索区域和适当预算；自动快速全屏获取与真实设备帧率仍须进一步实现和验收。

复现相关合成回归可使用：

```powershell
cargo test -p ai-ex-automation --test reflex_workflow --locked --offline -j 1
cargo test -p ai-ex-automation --test reflex_session --locked --offline -j 1 -- --test-threads=1
cargo test -p ai-ex-service --test template_game --locked --offline -j 1
```

这些测试不构成真实游戏、真实窗口/键鼠、模型或端到端帧率验收。旧真实动作、GUI 操作与剩余 Python 合同仍在迁移范围；本次没有通过删除旧实现缩减该范围。总体状态见 [Rust 迁移记录](RUST_ONLY_MIGRATION.md)。