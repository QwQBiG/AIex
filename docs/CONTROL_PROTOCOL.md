# AIex 本地控制协议

## 目的

桌面 UI、调试工具和未来插件只通过本协议控制 AIex，不直接依赖 LLM、音频、VTS、记忆或安全适配器。服务是唯一组合根和状态所有者。

## 传输与安全

- TCP + UTF-8 JSON Lines：每条请求和响应占一行。
- 只允许绑定 IPv4/IPv6 回环地址；配置非回环地址会被拒绝。
- 每条请求必须携带令牌。令牌从独立文件读取，至少 32 字节，不写入 TOML 或日志。
- 单条消息默认上限 65536 字节；超限连接收到失败响应后关闭。
- `emergency_stop` 会撤销已签发的自动化许可，并尝试打断当前对话。
- Rust 控制客户端对每次请求设置 5 秒总超时，覆盖连接、写入和读取完整响应。超时返回 `connectivity` 错误；命令可能已被服务接收，客户端不会自动重发。
- 桌面端的只读轮询、普通命令和打断/急停分别调度，慢状态请求或普通请求不会占住紧急命令通道。普通等待队列有容量上限；重复紧急操作合并，急停优先。打断前尚未发出的普通消息会退回草稿，已到服务的请求仍按服务结果处理。服务不可用时无法保证命令执行。

默认配置：

```toml
[control]
enabled = false
bind = "127.0.0.1:7878"
token_path = "config/control.token"
max_message_bytes = 65536
```

## 请求

请求具有 UUID、令牌和带类型的命令：

`speech_split` 查询本次服务实际使用的朗读分段状态。`set_speech_split` 使用查询返回的实例和版本设置，`minimum` 为 `null` 时关闭，否则为 4 到 512 的整数。状态只保留在当前服务中；重启恢复启动配置。对话进行中可查询但不能修改，记忆操作期间可能返回忙碌。设置超时或断线后结果可能未知，不自动重发：查询仅反映当时状态，不能证明旧请求不会迟到。若要确认同一实例的状态，客户端可在用户同意后将查询到的当前值和版本原样提交；只有收到同实例、值相同且版本递增的成功应答，才能确认旧版本的请求不再生效。冲突或再次超时后仍需重新核对。

```json
{"request_id":"00000000-0000-0000-0000-000000000001","token":"<redacted>","command":{"type":"status"}}
```

支持的命令：

```json
{"type":"submit","text":"你好"}
{"type":"interrupt","reason":"user barge-in"}
{"type":"status"}
{"type":"speech_split"}
{"type":"set_speech_split","instance_id":"00000000-0000-0000-0000-000000000001","expected_revision":0,"minimum":10}
{"type":"stage"}
{"type":"persona"}
{"type":"memory","request":{"type":"list","profile_id":"default","query":"","kind":null,"offset":0,"limit":12}}
{"type":"automation","request":{"type":"status"}}
{"type":"set_persona","profile":{"profile_id":"default","revision":2,"name":"AIex","system_prompt":"","tone":"warm, concise, and curious","taboos":[],"live_mode":"controlled"}}
{"type":"events","after":42,"limit":256}
{"type":"emergency_stop"}
```

`persona` 读取当前角色快照；`set_persona` 经过版本/字段校验后更新 Runtime 人格，并广播 `persona_changed` 事件。同一 `profile_id` 的编辑保留上下文；不同 ID 会停止旧音频、切换记忆范围并清空短期对话。记忆适配器不支持身份隔离时拒绝切换，不会静默共用旧记忆。活动回合期间切换会失败，避免一半回复使用旧人格、一半回复使用新人格。并发人格更新串行提交，读取人格会等待正在进行的更新完成。`submit` 先等待运行时确认可执行或已排队，再返回 accepted，正文异步生成；队列满时返回 failure。客户端通过 `status` 获取最新只读快照，通过
`events` 从指定序号之后重放有界事件历史。`limit` 必须位于 1 到 1000；事件带单调递增序号，客户端检测到缺口时必须暂停应用后续事件并重新拉取。

桌面端的新手角色面板使用同一协议：先读取 `persona`，本地编辑草稿，再在确认窗口中发送 `set_persona`。当前身份由服务快照同步；应用中的草稿只有收到对应 `set_persona` 成功应答后才被确认，普通轮询不能提前确认切换。开发者可用 `events` 观察角色变更、失败和运行时事件，终端继续保留服务原始日志。

`set_persona` 不改写启动配置，重启仍使用配置中的初始档案。记忆记录保留档案 ID，重启后选择同一档案即可召回。切换失败保留人格和记忆范围；如果失败发生在停止旧音频之后，音频不会自动恢复。版本字段目前只做合法性校验，尚未实现基于旧版本的并发编辑冲突检测。

桌面的[启动组合](SCENE_PACKS.md)通过同一协议在服务连通、人格读取完成后恢复角色；服务本身仍先加载初始档案。新启动服务可自动恢复已选组合，已有服务先由用户确认。资源已准备且对应切换应答成功后，桌面才提交外形；拒绝或结果不明时不会反复自动重试。

`stage` 返回最近的舞台动作摘要，包含遥测 schema、单调序号、动作类型和受限 detail；它只读，不会触发 OBS 或桌面副作用。

## 合成模板回放

0.10 的真实 Windows 操作使用独立 `native_desktop` 命令，下面的 `automation/start_template` 合同保持合成。两种快照分别声明 `execution_mode: "native_windows"` 与 `"synthetic"`，不能从界面显示名称推断执行权限。

`automation` 命令使用同一个请求信封、令牌和响应 UUID，控制服务持有的合成模板会话。以下契约适用于 0.8.0-alpha.1；旧 0.7.1 程序不包含该入口，使用时需一起升级桌面与服务。它只读取明确指定的回放图片并记录 dry-run 动作，不提供真实屏幕或键鼠执行选项。清单准备和执行边界见[模板回放说明](TEMPLATE_GAME.md)。

以下每行都是独立命令，发送时放入请求信封；示例中的实例、会话和版本需替换为实际状态：

```json
{"type":"automation","request":{"type":"status"}}
{"type":"automation","request":{"type":"start_template","instance_id":"00000000-0000-0000-0000-000000000001","expected_revision":4,"operation_id":"00000000-0000-0000-0000-000000000002","manifest":"D:/replay/manifest.json"}}
{"type":"automation","request":{"type":"pause","instance_id":"00000000-0000-0000-0000-000000000001","session_id":"00000000-0000-0000-0000-000000000003","expected_revision":5}}
{"type":"automation","request":{"type":"resume","instance_id":"00000000-0000-0000-0000-000000000001","session_id":"00000000-0000-0000-0000-000000000003","expected_revision":6}}
{"type":"automation","request":{"type":"stop","instance_id":"00000000-0000-0000-0000-000000000001","session_id":"00000000-0000-0000-0000-000000000003","expected_revision":6}}
```

成功响应的 payload 为 `{"type":"automation","data":{...}}`，`data` 直接对应 `AutomationSnapshot`，没有额外的 Box 层：

| 字段 | 含义 |
| --- | --- |
| `schema_version` | 当前为 `1`；不支持的版本拒绝应用 |
| `instance_id` | 与运行时快照相同的服务实例 UUID；重启后重新读取，不沿用旧实例的会话或游标 |
| `revision` | 变更游标，包含控制、暂停归属和静默期限变化；纯帧进度不增加它 |
| `projection_sequence` | 同一实例中所有已发布摘要的顺序，包括帧进度与命令应答；旧轮询不能覆盖更新摘要 |
| `available` | 当前服务是否提供合成回放主机 |
| `session` | 无会话时为 `null`，否则包含稳定 `session_id`、`operation_id`、阶段、控制状态和有界执行摘要 |

`session_id` 由服务为新会话分配；`operation_id` 由客户端为一次开始意图分配，用于识别同一次开始操作。它们与每条请求信封的 `request_id` 分别使用，不能互相替代。

新建 `start_template` 必须使用当前 `instance_id` 和精确的 `expected_revision`。服务先登记 `operation_id` 再加载图片，成功应答中的 `loading` 不代表回放已经完成。对当前 operation 和同一路径的重复请求返回当前状态，不重复启动；同一 operation 指向另一路径会拒绝。manifest 路径最多 4096 个 UTF-8 字节，不得为空或含控制字符；它在服务端解析，跨工作目录使用时建议传绝对路径。清单中的图片和 profile 相对清单所在目录解析。

`pause` 和 `resume` 校验当前实例、会话及精确的变更版本。手动暂停即使已经处于聊天暂停，也会撤销自动恢复权；恢复只在宿主确认聊天、排队话轮、转写和语音都空闲时允许。`stop` 对当前会话是终态，接受不高于当前的旧版本，拒绝未来版本，使停止能胜过并发暂停或聊天。`stopping` 表示已经撤销后续派发、正在排空已开始的步骤；已派发动作不能由停止响应撤回。继续执行需要显式开始新会话。

会话摘要包含 `phase`、`control_mode`、暂停原因、恢复条件、已消耗/最大帧数和动作数、最后结果及受限错误。`chat_deadline_ms` 属于宿主单调时钟，仅供观察，不能当作客户端墙钟时间或恢复权限。`search_incomplete` 是计算预算不足、无法认证完整匹配的终态；可选 `search` 只携带区域和有界计算进度，不携带暂定分数、目标坐标或可重试动作。

完整 `AutomationSnapshot` 序列化上限为 **16 KiB**；摘要不含 RGBA、原始图片、完整回执或审计记录。`revision` 和 `projection_sequence` 是用途不同的整数游标，不能互相替代。自动化摘要不推进聊天事件游标。

超时或响应丢失表示结果未知，客户端保留清单草稿，先查询 `automation/status`，不自动重发 Start、Pause、Resume 或 Stop。相同快照不能证明一个待确认命令尚未执行，刷新后仍可能需要保持“待核对”；只有能与实例、operation、会话或变更版本对应的确认才解除等待。版本冲突时也应刷新，不覆盖较新的控制状态。

桌面的回放状态查询、Pause 和 Stop 各有独立的有界通道，Stop 不排在聊天、记忆、Start、Resume、Status 或 Pause 请求之后；全局 `emergency_stop` 继续使用独立紧急入口。断线或事件历史待同步时禁用普通回放变更；连接有效、会话身份已知时仍可请求停止。通道独立不保证断网时命令已经执行，应以服务确认和后续状态为准。

## 原生窗口与运行设置

```json
{"type":"native_desktop","request":{"type":"status"}}
{"type":"native_desktop","request":{"type":"windows"}}
{"type":"desktop_runtime","request":{"type":"state"}}
{"type":"desktop_runtime","request":{"type":"diagnostics"}}
{"type":"desktop_runtime","request":{"type":"discover_vts"}}
{"type":"desktop_runtime","request":{"type":"check_vts_mappings"}}
{"type":"desktop_runtime","request":{"type":"discover_vts_parameters","offset":0}}
{"type":"desktop_runtime","request":{"type":"discover_vts_parameters_filtered","filter":"mouth","offset":0}}
```

`native_desktop` 返回 `NativeDesktopReply{snapshot,windows}`。窗口扫描只在显式 `windows` 请求时进行，最多 256 项且整份响应受 32 KiB 预算限制；完整授权路径不能截断，超出路径界限的项目不返回。服务启动、状态查询和界面预览不扫描窗口。

- `start_native` 携带服务 `instance_id`、严格 `expected_revision`、`operation_id`、扫描取得的 `window` 和有限 `max_frames/max_actions`。服务重新核对实际进程与完整路径，使用全局配置许可和独立持久审计，不采用合成许可。
- `set_action` 或 `set_goal` 保存单步草稿，二者互斥；它们不立即发输入。`resume` 严格核对实例、会话和版本，并要求对话/播放空闲、目标在前台及几何已确认。
- `capture` 产生有界、短时有效的 `frame_id`，不把图片字节放进控制快照。`execute` 携带当前游标、独立操作 ID、画面 ID 和受限动作；相同 ID 同一意图只查询结果，异意图拒绝。`vision_step` 只接受一个受限动作建议，也经过同一协调器。
- 原生 `pause/stop` 是撤销意图：同实例和会话可用已观察到的版本 `<= current`，未来版本拒绝；停止后暂停不能复活会话。其他变更继续严格 CAS。合成入口的暂停版本合同保持原样。
- 控制超时或断线不撤销独立 owner，也不代表未执行。先查询状态，不重放未知结果；暂停和停止先撤销当前代次，迟到帧、文字或动作不得重新提交。

`desktop_runtime` 的 `apply/reload` 使用实例和版本 CAS。Apply 只更新独立行为文件的主动输出、冷却、免打扰、静音时段、快捷键和字幕设置；Reload 只读取该文件，不声称重新加载模型、设备或整个服务配置。注册失败返回新版本的真实未注册/错误状态，不能把“已保存”当作“已注册”。

`diagnostics` 使用专门的允许字段结构，不拼接运行日志或聊天快照。开发分支的版本 2 报告可包含固定名称的组件及其最近一次已记录的就绪标志；缺少组件表示未知，`false` 可能是未启用或不可用，不包括错误详情，读取报告不会主动检查设备或外部服务。状态可能已经过期，不能据此认定模型已完成推理或设备可用。VTS 热键发现返回有限 ID/名称，不触发热键。输入参数发现每页最多返回 16 项，包含当前值、范围、默认值及来源；每次请求重新读取，参数变化时不同页不保证属于同一快照。主动输出使用 `proactive_started` 事件启用输出代次，不发带伪造用户文本的 `turn_started`，也不写用户历史或记忆。

开发分支的 `check_vts_mappings` 以一次发现结果核对运行中使用的表情映射，返回人物载入状态和五种情绪的 `unconfigured`、`matched`、`missing` 或 `unknown`。显示用的热键 ID 最多 256 个字符，截短时 `id_truncated` 为真，比较仍使用完整 ID。未载入人物时已配置项为 `unknown`；连接失败返回错误。结果是查询时的快照，不修改配置或触发热键；旧版服务不提供此请求。

开发分支的 `discover_vts_parameters_filtered` 先按参数名筛选再分页：`mouth` 匹配 mouth 或 lip，`voice` 匹配 voice，不区分 ASCII 大小写。旧版服务不提供此请求；原有的全部参数查询保持可用。每页重新发现参数，不保证跨页是同一快照。

## 记忆管理

`memory` 命令包装领域层的 `MemoryRequest`，仍使用相同请求 UUID 和令牌。以下契约适用于 0.8.0-alpha.1；实体及媒体请求需要同版服务，旧 0.7.1 仅支持原有基础记忆操作。

开发分支增加 `read_segment`、`backup` 与 `backup_status` 操作，需要支持这些操作的服务；旧版服务不提供此功能。

每行都是独立命令，发送时仍需放入前述请求信封：

```json
{"type":"memory","request":{"type":"list","profile_id":"default","query":"偏好","kind":"persona","offset":0,"limit":12}}
{"type":"memory","request":{"type":"read_segment","profile_id":"default","id":"00000000-0000-0000-0000-000000000002","expected_revision":1,"field":"user","offset":0}}
{"type":"memory","request":{"type":"entities","profile_id":"default","query":"coffee","offset":0,"limit":12}}
{"type":"memory","request":{"type":"evidence","profile_id":"default","key":"preference:coffee","offset":0,"limit":12}}
{"type":"memory","request":{"type":"query_media","profile_id":"default","query":"","modality":"visual","tags":["旅行","风景"],"min_importance":500,"offset":0,"limit":12}}
{"type":"memory","request":{"type":"remember","profile_id":"default","text":"请叫我小林。"}}
{"type":"memory","request":{"type":"correct","profile_id":"default","id":"00000000-0000-0000-0000-000000000002","expected_revision":1,"text":"请叫我小禾。"}}
{"type":"memory","request":{"type":"remember_media","profile_id":"default","text":"旅行照片的说明。","media":{"modality":"visual","tags":["旅行"],"visual_features":{"description":"山边的湖泊","objects":[{"name":"湖泊"}]},"metadata":{"source":"manual-note"},"importance":700}}}
{"type":"memory","request":{"type":"correct_media","profile_id":"default","id":"00000000-0000-0000-0000-000000000003","expected_revision":1,"text":"秋天旅行照片的说明。","media":{"modality":"visual","tags":["旅行"],"visual_features":{"description":"山边的湖泊","objects":[{"name":"湖泊"}]},"metadata":{"source":"manual-note"},"importance":700}}}
{"type":"memory","request":{"type":"forget","profile_id":"default","id":"00000000-0000-0000-0000-000000000002","expected_revision":2}}
```

| 字段与操作 | 契约 |
| --- | --- |
| `profile_id` | 必须与服务当前角色精确匹配；非空且最多 128 个字符 |
| 只读分页 | `list`、`entities`、`evidence`、`query_media` 均使用 `offset` 与 1–100 的 `limit`；查找词或实体键最多 512 个字符 |
| `list` | 正文子串查找；`kind` 可为空或指定分类 |
| `entities` | 查找实体键、名称或候选值，按稳定键聚合来源，返回 `entities` 响应 |
| `evidence` | `key` 非空且精确匹配实体稳定键，返回来源记录的 `page`；不把候选概览当作完整来源列表 |
| `query_media` | 正文子串查找；`modality` 为空表示所有类型，否则精确匹配；多个 `tags` 为 OR，忽略首尾空白和大小写；`min_importance` 为 0–1000 |
| `remember` / `correct` / `remember_media` / `correct_media` | `text` 非空、最多 4096 个字符；新建与更正均进入 `persona` 分类，分别标记 `user_note` / `user_correction` |
| `correct` / `correct_media` / `forget` | 校验记录 ID 与正整数 `expected_revision`；记录不属于当前角色、已删除或版本冲突时拒绝 |

媒体对象包含 `modality`（`text` / `audio` / `visual` / `multimodal`）、`tags`、`importance`、可选 `audio_features` / `visual_features` 和 `metadata`。特征与元数据均为可嵌套的 JSON 对象；没有原始附件上传或自动媒体理解约定。省略字段时，媒体类型默认 `text`，重要程度为 0，标签与元数据为空。

`text` 不带音画特征；`audio` 必须提供音频对象且不带视觉对象；`visual` 必须提供视觉对象且不带音频对象。`multimodal` 兼容旧模块允许的任意特征缺省组合，两种特征都可省略或为 `null`；空对象 `{}` 表示已提供特征对象。媒体序列化整体最多 8192 字节，最多 16 个非空标签，每个最多 64 个字符，重要程度为 0–1000。查询的标签遵循相同限制。普通无媒体记录按 `text`、重要程度 0 参与媒体查询，结果先按重要程度再按更新时间降序排列；正文查询不搜索特征或元数据内部字段。

`correct_media` 接收完整媒体对象，并非字段补丁；客户端应读取该条 `knowledge.media`、复制完整对象，仅修改用户指定的字段后再提交。普通 `correct` 会移除该条原媒体信息。桌面编辑器使用前一种方式，未修改描述和类型时也保留原有缺省特征，不会把 `null` 自动补成空对象；主动换类型会移除不再适用的特征。

记忆记录版本与角色设定版本分别维护。活动回合期间拒绝记忆管理；桌面另外等待播放结束。成功修改在持久写入后清空当前短期上下文并停止旧输出；更正替换该条输入并移除旧回复，遗忘仅删除所选记录。实体候选随正文重新提取，删除记录后不再贡献来源；其他记录、角色设定或派生信息中的重复内容不会自动删除。

成功 payload 的 `type` 为 `memory`，其 `data` 对应 `MemoryReply`，包含两个字段：

- `response`：`{"type":"changed"}`、`{"type":"page","data":{...}}`、`{"type":"entities","data":{...}}` 或开发分支的 `{"type":"segment","data":{...}}`。
- `snapshot`：操作完成后取得的完整 `RuntimeSnapshot` 结构，含 `instance_id` 与 `last_sequence`；文字字段受下述预算限制。

Rust 内部使用 `Box<MemoryReply>`，JSON 不增加额外包装层。`page.data` 包含 `profile_id`、`enabled`、`total`、`offset`、`entries`、`truncated_ids`，以及非空时才输出的 `knowledge`。记录携带 ID、角色 ID、回合 ID、分类、输入/回复、来源、记录版本及创建/更新时间；缺少来源和版本的旧记录默认 `automatic` / `1`。毫秒时间戳通常是 JSON 整数，超出 `u64` 范围时使用十进制字符串。

`knowledge` 是以记录 UUID 字符串为键的对象；每项含 `entities` 数组和可选 `media`。单个候选为 `{kind,key,name,value,confidence}`，`kind` 为 `name` / `preference` / `fact`，`confidence` 是 0–1000 的规则评分。候选从用户正文提取，不从助手回复或自动媒体描述推断用户身份；旧导入文本需要明确的 `user_input`。原始导入元数据和向量不通过这个控制页输出。

`entities.data` 包含 `profile_id`、`enabled`、`total`、`offset` 与 `entities` 数组。每组包含 `key`、`kind`、`name`、`current_value`、`first_mentioned_ms`、`last_updated_ms`、`total_mentions` 和 `evidence`；每条来源为 `{memory_id,revision,source,value,confidence}`。稳定键示例为 `user.name`、`preference:coffee`、`fact:location`。当前值优先取用户笔记或更正，再按更新顺序选择；来源中的矛盾值保留，当前值不等于经核验的事实。每组概览最多 32 条来源，响应预算还可能进一步减少；完整来源记录通过 `evidence` 分页获取。
### 确认修改与聊天游标

客户端应先匹配请求与当前角色，并确认应答来自当前服务实例。只有确认修改成功的 `changed` 应答才用于清空聊天显示；随附快照的序号给出旧对话清理边界，边界以内迟到的文字事件不得重新构造已清空的聊天，较早的快照也不得倒退当前事件序号。服务实例已变化的应答需要重新刷新核对。

**`list`、`entities`、`evidence` 和 `query_media` 的快照不得推进聊天游标或替代尚未收到的正文事件。** 查询只读，不能因为附带的快照序号较新就跳过聊天。常规快照同步仍先补齐事件，缺口按既有恢复规则处理。

超时或结果不明不代表未写入；客户端保留草稿、要求成功刷新后核对，不自动重发修改。明确版本冲突时也应刷新重新选择记录，不能覆盖较新版本。

### 响应大小

记录页和实体页分别使用 **48 KiB 序列化 JSON 预算**，因此实际返回条数可能少于请求的 `limit`。记录页下一页从 `offset + entries.len()` 请求，实体页从 `offset + entities.len()` 请求，不能固定跳过 `limit` 条；`total` 表示全部匹配记录或实体组数。

单条记录过长且无法完整放进空页时，返回片段并将 ID 加入 `truncated_ids`。客户端必须标明“查看片段 / 复制片段”，不能把片段当作完整原文；更正应重新输入完整事实。分页和查看不会截断原始存储。

`read_segment` 按当前角色、记录 ID 和预期版本读取 `user` 或 `assistant` 字段。`offset` 是 UTF-8 字节偏移，必须位于字符边界；响应包含同一身份、版本、字段、偏移、`text` 和 `next_offset`。下一次使用返回的 `next_offset`，为 `null` 时已到结尾。记录被更正或遗忘后旧请求失败；读取不修改记忆或聊天上下文。

`backup` 和 `backup_status` 携带当前 `profile_id` 与由客户端生成的 UUID v4 `backup_id`。备份只包含当前角色的记忆、向量和来源关联，写到服务记忆文件旁的独立 JSONL，不接受客户端路径，不覆盖已有文件；它包含私人内容，依赖目录访问权限，不随角色包分享。操作不修改原记忆或清空聊天上下文；未启用记忆时拒绝备份。

服务以同一实例内最近 64 项状态去重；返回 `running`、`completed`、`failed` 或 `unknown`。完成时含文件路径、记录数和临时链接是否保留。状态可能因实例重启或终态淘汰而变为 `unknown`，不能据此断言文件未创建。备份执行期间协调器拒绝新聊天、角色切换和状态查询；状态查询可能返回忙碌，请稍后查询。超时或断线时保留原 ID 核对，不自动重发备份。仅写入同步后的完整文件才发布；同卷硬链接不支持时会明确失败，不自动移除旧备份。

`status` 与 `MemoryReply.snapshot` 共用 **8 KiB 序列化 JSON 预算**，必要时裁剪 `last_fault` 与 `playback.text` 并用省略号标记，保留事件实例、序号和行为状态。这为默认 65536 字节控制消息中的页、快照与外层封装留出空间；自行调小传输上限时仍需核对能否容纳应答。

事件数组仍受单条控制消息字节上限约束，`events.limit` 只限制事件条数，尚未按序列化字节自动分页。不保证任意长错误、单条正文或一次请求中的全部历史都能完整传输；快照也不是完整聊天记录备份。

## 响应

成功响应：

```json
{"status":"success","request_id":"00000000-0000-0000-0000-000000000001","payload":{"type":"accepted"}}
```

状态响应的 payload 类型为 `snapshot`，包含当前会话状态、活动轮次、完成/打断/故障计数和最后故障。`instance_id` 是当前事件服务实例的 UUID；服务重建时变化，客户端据此重设事件边界，不能仅靠序号大小判断重启。旧响应未提供该字段时仍可读取，退回序号检查，但无法可靠区分序号重合的不同实例。

快照增加 `playback` 字段：`turn_id`、`active`、`mouth_level`（0–1000）、`position_ms`、`duration_ms`。`speech_playback` 事件使用同一个结构传递播放更新；生成结束事件不会清除仍在播放的状态。播放结束或取消时发送空闲快照。桌面约每 50 毫秒读取事件，健康等快照仍约每 2 秒刷新。

新桌面读取缺少 `playback` 的旧服务快照时，默认视为没有音频播放；升级服务时应同步升级客户端，因为旧客户端可能不认识新增事件类型。

失败响应：

```json
{"status":"failure","request_id":null,"error":{"kind":"protocol","message":"invalid control request"}}
```

错误类型与 Rust 领域错误一致：配置、连接、协议、非法状态、安全、不可用和内部错误。客户端不得把 failure 当作 accepted，也不得自动绕过安全错误重试动作。
