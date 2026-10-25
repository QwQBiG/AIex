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

`display_name`、`description`、`vlm_prompts`、`default_templates` 是 profile 字段，`action_cooldowns` 可省略。提示词和冷却时间保留在 profile 中；独立 CLI 入口不会调用 VLM，也不把 `action_cooldowns` 自动换算为反射循环间隔。循环间隔由 manifest 的 `interval_ms` 明确控制。

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
| `max_work` | 单次搜索累计默认 `250000000`；必须为正，合计精扫像素对数与上界查询费用；每个开始的 16 块查询收取 48 单位。无法继续核验时报告 `search_incomplete`，不采用暂定命中；详见[有界增量精确搜索](#有界增量精确搜索) |
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

`max_frames` 约束捕获，`max_actions` 单独约束实际或可能已执行的指针动作。一帧点击通常消耗一次捕获和两次指针动作；若只剩一个指针动作，移动后即停止，不能把未发生的点击记作完成。这两类预算耗尽进入 `budget_exhausted`，后续不再捕获。适配器在执行中报错时，动作是否已经发生无法由错误证明，计数保守计入并停止；不自动重试。

`max_work` 约束一次匹配搜索的累计计算工作，分片不会重新获得额度。无法在原预算内完成核验时，报告 `search_incomplete` 并停止当前会话，CLI 输出已有报告后返回非零退出码；它与帧/动作预算耗尽不同，也不能解释为已经证明未命中。诊断保留原预算、精扫和筛选费用，不提供未认证的坐标或分数；继续需要显式开始新会话，状态查询或 Resume 不会接续旧搜索。

移动失败、急停、坐标/匹配失败或任何完成审计未持久化，都使控制器进入 `failed`，阻止后续点击和捕获。错误包含失败阶段、可用的执行前/执行中分类、动作 ID 与已取得的 receipt 摘要；完成审计失败的 receipt 明确保留 `completion_audit_persisted=false`。即使适配器已返回成功，也不会把缺少完成审计的点击报告为 `completed`。

stdout 输出完整 JSON：顶层含 `execution_mode="dry_run"`、输入/筛选计数、最终 outcome、`steps`、`failure`、`audit`。每步含状态、匹配分数/矩形、实际搜索区域、连续未命中数、累计帧/指针动作数、绝对/归一化坐标和该步 receipts，不包含原始 RGBA。失败那一步的部分报告位于 `failure.report`，此前正常步骤保留在 `steps`。运行失败会在输出报告后返回非零退出码。

指定 `report` 时，使用新文件创建和同步写入；已有文件在执行前拒绝，并发创建也不能覆盖。执行后的报告先输出 stdout，再尝试写文件；目录不可写或写入失败仍保留 stdout 中的执行证据，并返回非零退出码。输入校验失败或预检查发现已有报告时，尚未执行动作，不会生成执行报告。报告审计属于本次有界离线回放，不写入真实服务的审计库。

## 可暂停的 Rust 会话

需要从另一个任务暂停反射时，使用 `ReflexSession::new(controller)` 包住控制器，并通过 `session.control()` 取得可克隆的 `ReflexControl`。`session.step(...)` 返回 `ReflexSessionReport`：正常推进为 `advanced`，暂停、停止或旧代次失效为 `interrupted`；后者保留当前步已取得的 receipts、计数与类型化原因。`session.state()` 是执行进度，暂停/停止模式从 control snapshot 读取。

`pause_for_chat()` 暂停、`resume()` 显式恢复、`invalidate_target()` 使旧工作失效，都会推进相关代次；`stop()` 对这个 control session 永久生效，恢复需要新的 session 与 controller。已暂停时重复暂停、运行时重复恢复是幂等操作。`invalidate_target()` 不会替换 matcher 或 target；这个 wrapper 的目标固定，geometry 在每步开始时提供。

同一步只取得一次租约，跨越捕获、匹配、移动和点击的所有等待。coordinator 在请求审计前与授权审计后、真正派发前检查同一个租约；捕获/移动返回后也检查它。即使暂停与恢复已经完成，旧代次仍不能继续点击。暂停/恢复发生在两步之间时，下一步会立即清除旧追踪 ROI；保留总 misses、帧/动作预算和捕获间隔，下一次到期必须重新 capture。

已派发的动作可能继续完成，不能把逻辑暂停说成撤回原生输入。正常暂停等待当前动作及审计返回，保留真实回执和消耗。完成审计失败、适配器失败或控制拒绝审计未持久化仍然进入 `failed`。已经成功且完成审计返回的 `click_once` 保持 `completed`，迟到的暂停不会让它恢复后再点击。

直接取消或丢弃已开始的 step future 会使 controller 立即进入 `failed`，后续禁止续跑。派发前取消不增加未派发动作的计数；派发后取消保守计入一个可能已执行的捕获/指针动作。取消不伪造完成 receipt 或审计，当前 future 的完整报告也无法再返回。需要保留完整证据时，调用 `control.stop()` 并继续等待 step 结束。未 poll 的 future 尚未开始，不改变状态或审计。

### 聊天静默恢复策略

`ChatPausePolicy::new(session.control(), ChatPauseOptions::default())` 提供独立的同步策略，默认开启、静默超时 30,000 ms；自定义超时至少 5,000 ms。宿主在接纳聊天时、进入长等待之前调用 `on_chat(now_ms)`，定期调用 `poll(now_ms, ResumeEligibility::...)`。两者使用同一单调时钟域，允许相同时间；倒退时间或聊天 deadline 溢出会在修改策略或 control 之前报错。没有后台 timer、自动睡眠或真实聊天 hook。

策略只持有自己从 Running 原子取得的暂停凭证。重复聊天延长 deadline，不另起代次；到达 deadline 时，只有宿主明确给出 `Idle` 才能条件恢复，`Busy` 和 `Unknown` 保留暂停和原 deadline。宿主必须把已排队话轮、会生成动作或聊天输出的 VLM、语音输出与事件缺口纳入空闲判断；单个 TurnFinished 或漏收事件不能证明空闲。禁用策略不暂停、不恢复，也不记录聊天活动。

手动 `pause_for_chat()` 会撤销自动恢复权，即使本来已暂停、公开 snapshot 没有变化。手动恢复、目标失效和 stop 同样撤权；策略不能接管既有手动暂停或其他策略的暂停，轮询也不会重新暂停已经外部恢复的会话。只有后续明确的新聊天可重新取得 Running 会话的暂停。条件恢复在同一 control 锁内核对私有凭证、session、代次和暂停模式，封闭检查后发生手动操作的竞争；凭证不能由 JSON snapshot 重建，也不进入报告或 Debug。丢弃策略不会自动恢复会话。

`ChatPauseReport` 给出 outcome、观察时间、最后启用的聊天时间、当前自有 deadline、eligibility 和可选 control snapshot。`claimed`、`extended`、`waiting`、`held_busy`、`held_unknown`、`resumed`、`unowned`、`superseded`、`disabled` 区分策略结果；snapshot 只是瞬时观察，执行仍须检查不可复活的代次租约。

SDK 继续提供独立合成时序接口；服务与桌面现在通过下一节的合成回放入口接入同一套控制。现有 raw `run_reflex_step` API 保留兼容，其动作取消也使用同一失败保护。真实 Windows 捕获/键鼠及任意战略动作循环仍未接入；下一节的可选观察循环只建议同一目标的搜索区域。

## 服务与桌面的合成回放

使用当前 Rust 服务与桌面二进制，在设置页的“合成模板回放”中填入同格式 manifest 路径，建议使用绝对路径，然后选择“开始回放”。图片/profile 内的相对路径仍相对于 manifest 解析；GUI 不会修改输入路径。该入口只构造 `DryRunAutomationPort`，真实自动化保持默认关闭也能使用，不会读取屏幕、发送键鼠或启动进程。

服务独立持有会话，加载清单后按单调时钟自动处理下一张合成帧。清单时间戳仍负责预筛选来源；暂停后的实际步时间使用宿主时钟，不把旧时间戳当作当前时间。帧、geometry 和消耗同步推进，暂停或派发前拒绝不跳过图片；源耗尽为 `input_exhausted`，不会循环复用旧帧。宿主忽略清单中的 CLI-only `report` 字段，不检查或写入该报告文件；完整文件报告继续使用独立 `--template-game`。

输入聊天会立即暂停会话；静默默认 30 秒，只有已接纳及排队的话轮、语音合成/播放都结束，才允许自动恢复自己的暂停。stdin、控制请求、自动直播回应和转写提交共用占用包装器；后台等待在登记后才调度，客户端断开或丢弃等待不会提前宣称空闲。语音路径从检测到说话开始持有占用，跨越转写等待，再同步交接到聊天；空转写、错误或输入结束释放占用。清单加载期间发生的聊天也保留原静默期限。

“暂停回放”或“保持暂停”是手动操作，即使已经因聊天暂停也能撤销自动恢复权。“恢复回放”只在明确空闲时可用，不会绕过排队模型、转写或语音。停止为当前会话的终态；继续需要显式开始新会话。急停同时撤销合成端口派发并保持到服务重启。替换会话、停止和关闭服务都会撤销旧控制，已经开始的 step 继续排空，不通过取消 future 隐藏已执行的动作。

桌面只显示服务确认的状态、消耗和结果。暂停、停止、状态查询分别有独立有界通道，不排在慢聊天/记忆操作之后。断线或历史待同步时保留草稿、禁用普通变更；超时结果视为未知，只查询状态，不自动重发或重建会话。相同状态不能证明尚未处理的变更不会发生，界面会继续保留未知状态；连接有效的已知会话仍可请求停止，全局急停保留现有独立入口。

控制协议新增 `Automation { request }` 与有界 `AutomationSnapshot`。Start 携带服务 instance、revision、稳定 operation ID 与显式 manifest；相同当前 operation/路径重试不重复启动。Pause/Resume 核对 instance/session/revision，旧会话不能影响新会话；同一会话的 Stop 接受不高于当前的旧 revision，使它能胜过并发暂停/聊天。变更 revision 与进度 projection sequence 分开，旧轮询不能覆盖更新确认。摘要不含 RGBA、完整回执或审计；原始执行证据在宿主内存中有界保留。

宿主最多跟踪 128 个未完成输入、两个执行 owner 和两个内部归档，重任务加载/执行串行。计数失信或语音消费者丢失按 Unknown 处理，不能当空闲。现有 `--vision-image/--vision-prompt` 仍是独立一次性入口；下述可选观察循环与模板计算并行，只建议同一目标的 ROI，不是任意动作生产者。真实麦克风/扬声器及真实游戏仍未验收。

## 可选战略观察循环

服务会话的 manifest 可以显式加入下面的 `strategy` 对象，并使用已有、明确启用的 `vision` 配置。省略该对象时不创建分析 worker，也不会因回放发出 VLM 请求。配置的提供器负责观察合成图片，真实模型效果仍需另外验收；本轮回归只使用可控假提供器和本机 HTTP mock。独立 `--template-game` CLI 拒绝这个选项，不会悄悄把同步回放变成模型调用。

```json
{
  "strategy": {
    "prompt": "指出当前目标所在的搜索区域",
    "interval_ms": 1000,
    "timeout_ms": 5000,
    "max_age_ms": 5000,
    "max_requests": 16
  }
}
```

`prompt` 必需、非空、最多 2048 字符；其余字段如上默认。间隔范围为 `100..=60000 ms`、请求超时 `1..=30000 ms`、观察有效期 `1..=60000 ms`，每会话请求上限 `1..=64`。模型仅能返回 `{"type":"keep"}` 或 `{"type":"focus_region","region":{"x":0,"y":0,"width":100,"height":100}}`；额外字段、动作、代码围栏、浮点/负数、空矩形、溢出与超出来源图像的区域均拒绝。文本上限为 16 KiB。现有 Ollama 适配器在解析前按 chunk 限制成功 HTTP 正文为 256 KiB、错误正文为 8 KiB，错误展示最多 512 字符；声明长度超限立即拒绝，取消和超时不留下后台读取任务。

快循环仍是唯一动作 owner。慢 worker 只接收成功且完成审计持久化的捕获副本，最多一个分析在途、一个最新帧邮箱和一个待应用建议；新观察覆盖未开始的旧观察，不积压队列。输入副本最多 4194304 像素，观察 PNG 最长边缩至 640、最多 8 MiB；建议坐标始终使用提示中给出的**原图 frame-local 像素**。较大来源不进入该观察 worker，精确模板匹配本身不做下采样。

建议绑定服务/会话/operation、原控制代次、请求序号、已审计来源帧、时间、尺寸与 geometry。只有完整快步骤结束、下一步到期且 Running/Idle 时，宿主才在同一仲裁锁内核验并应用。Focus 与 manifest 原固定 ROI 相交，不能扩大原范围；清除旧追踪并推进代次，保留目标、matcher、预算、misses 和节拍。建议到期、来源尺寸/geometry 改变或聊天/手动控制改变代次后，下个可执行步骤恢复原 ROI；旧结果不会取得新代次权限。

Keep 不改变控制代次；两种建议都没有暂停恢复、换目标或直接输入权限。聊天暂停不会等待慢分析结束，也不会因为结果返回而恢复。Stop、替换、EOF 和关闭撤销结果，取消纯分析 future 并等待 worker；已经开始的快步骤及有限的 PNG 编码始终排空。当前没有把旧 Python 的 click/keypress/drag 任意战略动作合同标记为完成。

## 当前验证范围

默认预算增量搜索检查点：核心完整回归 **575 通过、0 失败、2 忽略**；独立桌面 **156 通过、0 失败、6 忽略**。新增回归覆盖整数上界、跨分片预算、暂停/取消、搜索未完成终态及旧快照兼容；实际服务控制六组全部通过。两套严格 Clippy、两套 fmt、两套 debug 构建、31 包架构检查及 12 项架构回归通过。完整记录见[默认预算增量验收](RUST_ONLY_MIGRATION.md#默认预算增量精确搜索验收2026-10-20)。

此前全高清内层与战略观察检查点的核心完整回归为 **552 通过、0 失败、2 忽略**；其中自动化 127、服务 87、视觉适配器 13 项全部通过。核心和桌面严格 Clippy、两套 fmt、两套 debug 构建、31 包架构检查及 12 项架构回归通过。该检查点未改桌面源，桌面测试沿用下方前一检查点的 151/6，不重复计入核心总数。新增用例包含独立标量逐位等价、慢提供器与旧票据隔离、实际本地服务/假 VLM 和 HTTP 正文边界；完整记录见[迁移验收](RUST_ONLY_MIGRATION.md#全高清内层与战略观察仲裁验收2026-10-20)。
2026-10-20 服务/桌面合成回放检查点：核心 workspace 全 feature 回归 **508 通过、0 失败、2 忽略**；其中服务全部 feature 的 **70 项**全部通过。独立桌面 **151 通过、0 失败、6 忽略**；两套严格 Clippy、两套格式检查、服务和桌面 debug 可执行文件构建、31 包架构检查及 12 项架构脚本回归全部通过。核心忽略项为真实音频输出与固定原生 Chroma 临时库；桌面忽略项为六项可选离屏评审图片导出。

该服务/桌面检查点新增用例覆盖真实服务子进程经控制协议操作合成会话、延迟模型/ASR、排队与取消后的占用、手动暂停、旧 revision 停止、急停、加载期间静默期限及关闭排空。桌面状态/队列用例无窗口执行，未启动真实 GUI。新增测试只使用独立临时合成数据、假端口及本机 mock；没有读取用户记忆库或调用真实模型、屏幕、键鼠、麦克风、扬声器。

首个离线回放检查点确认 `ai-ex-automation` 的 46 项测试和 `ai-ex-service` 的 46 项测试通过。其中 12 项 `reflex_workflow` 集成回归覆盖顺序与审计、单次完成、间隔/hover、预算、移动/点击失败、急停、审计失败、追踪回退、Lost 后恢复、单调时间和帧缓冲所有权；8 组服务 CLI 测试验证实际 Cookie Clicker profile/模板、PNG/JPEG 解码、坐标、完整九条审计、帧调度、报告防覆盖和无配置离线运行。2026-10-20 本检查点的全部 feature workspace 回归共 400 项通过、0 失败、2 项忽略；忽略项是需要真实输出设备的音频测试与等待固定原生二进制的 Chroma 临时库测试。workspace 严格 Clippy（全部 target/feature、`-D warnings`）、格式与 31 个包的架构检查、12 项架构回归也已通过。

随后受控会话检查点通过 `ai-ex-automation` 全部 80 项测试（原 46 项，加 8 项派发 guard、7 项控制状态、19 组会话/取消工作流），以及全部 feature 下的 8 组服务模板 CLI 回归。workspace 全部 target/feature 严格 Clippy 和格式检查通过。新增交错测试使用有界 channel 握手，无实际输入；覆盖两步之间的代次变化、Requested/Authorized/Completed/Rejected/Failed 审计取消、端口取消及 raw/受控单次点击防重复。

聊天静默策略检查点再新增 29 项测试：13 项所有权/竞争/溢出/poison/Debug 回归、13 项注入时间策略回归、3 组实际 ReflexSession/coordinator 桥接流程。自动化包现有 109 项全部通过。2026-10-20 该 SDK 检查点的核心 workspace 全 feature 回归共 **463 通过、0 失败、2 忽略**；两项忽略仍为真实音频输出与固定原生 Chroma 临时库验收。workspace 全 target/feature 严格 Clippy（`-D warnings`）和格式检查通过。测试使用合成图像、独立测试数据和本地 mock，无真实键鼠、GUI、模型或用户旧库调用。

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

### 全高清精确内层测量

同一固定 seed、1920×1080 RGBA 画面、100×100 独立纹理模板、阈值 0.999；Release、单任务串行，每组一次预热、三次计时取中位数。近似命中位于最后合法候选，最后一个 RGB 像素扰动 +3，避免提前完美命中的捷径；未命中也完整排名。只把最大值为 2,601,000,000 的点积累加从 `u64` 改为 `u32`，末尾转回 `u64`，分数和工作预算算法未改。

| 合成背景 / 结果 | 旧内层 ms | 当前内层 ms | 比值 |
| --- | ---: | ---: | ---: |
| 随机 / 近似命中 | 11930.793 | 5825.659 | 2.048× |
| 随机 / 未命中 | 12255.160 | 6018.626 | 2.036× |
| 平滑重复 / 近似命中 | 12168.150 | 5952.949 | 2.044× |
| 平滑重复 / 未命中 | 12102.191 | 5823.451 | 2.078× |

四组重复结果、命中边界和全帧/精确 ROI 对照一致；八组独立 `u64` 标量参考测试逐位比较分数、边界、阈值及预算结果。计时包含灰度和积分图预处理，不含 fixture/模板创建、模型、捕获、审计或输入；机器未隔离，不能解释为真实游戏 FPS。精确 ROI 只是已知正确区域的对照，也不证明自动定位已解决。

复现（本机 GNU 工具链仍仅在该构建进程使用既有 LLVM dlltool 参数）：

```powershell
cargo run -p ai-ex-automation --example template_full_hd_bench --release --locked --offline -j 1 -- --samples 3
```

上述历史基准逐窗口精确计算：100×100 模板在 1920×1080 非恒定画面且没有提前完美命中时，共需 17,864,010,000 次像素对乘法，旧默认 250,000,000 预算只能完成前 25,000 个窗口（约 13.7 行）。后续增量在同一预算内增加保守上界认证，具体合同如下；本表保留为精确内层的历史对照，不代表当前筛选路径耗时。

### 有界增量精确搜索

`TemplateMatcher::begin_search` 固定借用一帧和有效 ROI，`advance` 在同一 job 中分片计算灰度、积分图、块上界和点积。每次调用保留累计计数，不重新获取预算。小搜索网格、无法等分为 4×4 块的模板，或足以负担全部精扫的预算，继续使用直接精确路径；有预算限制的大搜索使用 16 块整数 Cauchy 上界、最多 32 个种子和第二遍完整认证。模板不下采样，ROI 不扩大。

普通达到阈值的种子只是暂定最佳。其余候选必须精确计算或被保守上界严格排除，才可返回完成结果；等分数仍按最早行序选取。完整未命中也必须确定全局最佳分数。只有首个已确定最早的完美相关，或完整认证，才可提前结束。预算不足时返回 `Incomplete(WorkBudgetExhausted)`，不提供暂定坐标、分数或可执行动作。

`max_match_work` 默认仍为 250,000,000，单位现在明确为**精扫像素对数加上界查询费用**：每个开始的 16 块查询收取 48 个固定工作单位，两遍筛选及种子精扫共用这一总额。`exact_pixel_pairs` 只累计实际执行的点积，`bound_work` 单独记录固定查询费用，二者之和为 `work`，始终不超过原 `max_work`。这是一项保守加权工作预算，不能解释成 CPU 指令数或毫秒；灰度/积分图预处理由分片与来源/内存上限约束，不算点积费用。无法负担下一次完整精扫或查询时可以在尚有少量余额时终止，不借用后续调用额度。

来源与候选数均最多 16,777,216；搜索临时缓冲区预估上限为 320 MiB，种子堆和缓存最多 32 个候选。运行器每 16,384 个分片操作让出一次执行机会，并检查原始控制 lease；暂停、停止、换代或丢弃搜索 future 后，不允许旧帧继续取得动作权限。捕获已完成的计费不因搜索取消而重复增加。

服务/CLI 使用 `search_incomplete`，桌面显示“搜索未完成”，并列出核验进度、区域、精扫/筛选费用和原预算。该结果是停止终态，与普通 `missed` 及帧/动作 `budget_exhausted` 区分；相同 operation 重试或状态查询不会复用余额继续计算，Resume 也不能恢复。需要用户明确开始新会话或重新审视配置。控制快照强制 `Finished + Stopped`，不接受宣称仍可恢复的矛盾搜索诊断。

默认 250m 工作预算的本轮实测（同样一次预热、三次取中位数，release、单任务串行）：

| 合成背景 / 画面 | 全帧终态 | 全帧工作量 | 全帧中位 ms | 200×200 ROI 中位 ms |
| --- | --- | ---: | ---: | ---: |
| 随机 / 近似命中 | Complete · CertifiedBounds | 171,514,352 | 1094.489 | 19.975 |
| 随机 / 无目标 | Incomplete · WorkBudgetExhausted | 249,990,272 | 358.152 | 16.555 |
| 平滑重复 / 近似命中 | Complete · CertifiedBounds | 171,514,352 | 642.297 | 12.536 |
| 平滑重复 / 无目标 | Incomplete · WorkBudgetExhausted | 249,990,272 | 642.510 | 34.454 |

两个完整认证均核验 1,786,401 个候选，只做 2 次精确核（20,000 像素对），另有 171,494,352 筛选费用；正确边界为 `(1820,980,100,100)`，分数为 `0.999999856238`。两个不完整搜索只核验 16,346 个候选，不声称已经证明“未命中”。四组 ROI 均完成 Exhaustive，并与独立参考的最佳分数和坐标相同。全部预热/测量的终态、费用、调用次数和分片单调性重复一致。

计时为 begin_search 和各次 advance 调用耗时之和，不含 fixture、标量参考、打印和分片断言；每个完整认证 3,962 次 advance，每个不完整搜索 12,101 次。机器未隔离，随机近似命中单次结果为 687.818–1172.033 ms，不作低延迟或真实控制帧率保证。不同终态承担的证明工作不同，不能把更快的 Incomplete 当作完成搜索的加速比。

复现固定全高清夹具及独立 ROI 对照：

```powershell
cargo run -p ai-ex-automation --example template_budget_bench --release --locked --offline -j 1 -- --samples 3
```

该基准使用与历史全高清测量相同的四组固定图片，默认预算、一次预热、三次测量，并校验每次分片的累计计数和重复终态。200×200 ROI 由独立标量实现检查最佳分数及坐标；全帧未认证结果不输出临时位置。真实屏幕、键鼠、模型和游戏 FPS 仍需后续验收；较差纹理或普通未命中可能在固定预算内无法完成认证。

复现相关合成回归可使用：

```powershell
cargo test -p ai-ex-automation --test reflex_workflow --locked --offline -j 1
cargo test -p ai-ex-automation --test reflex_session --locked --offline -j 1 -- --test-threads=1
cargo test -p ai-ex-automation --test chat_pause --locked --offline -j 1 -- --test-threads=1
cargo test -p ai-ex-service --test template_game --locked --offline -j 1
cargo test -p ai-ex-service --all-features --locked --offline -j 1 -- --test-threads=1
cargo test --workspace --all-features --locked --offline -j 1 -- --test-threads=1
cargo test --manifest-path crates/ai-ex-desktop/Cargo.toml --locked --offline -j 1 -- --test-threads=1
```

这些测试不构成真实游戏、真实窗口/键鼠、模型或端到端帧率验收。旧真实动作、GUI 操作与剩余 Python 合同仍在迁移范围；本次没有通过删除旧实现缩减该范围。总体状态见 [Rust 迁移记录](RUST_ONLY_MIGRATION.md)。
