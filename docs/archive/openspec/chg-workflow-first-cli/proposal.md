---
id: CHG-2026-CLI-arkforge-workflow-first-cli
revision: 2
status: draft
class: capability
core_change_level: major
owner: TBD
platforms: [macos, windows]
---

# ArkForge Workflow-first CLI：一步刷机与复合查询面

> 本 change 修订 CHG-2026-CLI-arkforge-agent-native-cli 的**工作流包装**，
> 不修改其 authority supervisor、StepPermit、`arkforged` execution journal、
> no-replay、rescue 域分离、mechanics/authority 双执行门中的任何一条语义。
> 所有变化都发生在 `arkforge` 前端的命令切分、信息推断与输出契约层；
> 为审计交互确认，CLI authority 新增独立的 approval record，不改写
> mechanics journal/receipt codec。

## Why

现行命令面是按**数据边界**切的：每个内部资源（observation、artifact、
assessment、plan、job）各占一条命令，操作者和 Agent 充当各阶段之间的管道。
后果可以量化：

- **人类**：README 教学路径 7 条命令；实际最小路径 5 条；其间要人工搬运
  observation-id、64 位 hex artifact-id、plan-id、plan sha256 与 ack token。
- **Agent**：完整刷一次机最多 10 次调用（doctor → daemon start → device list →
  device probe → artifact import → artifact inspect → flash assess → flash plan →
  flash apply → job watch），其中 6 次仅为获取下一次调用的输入；
  `help` 每次只返回一个节点，了解全树还需逐路径反复调用。
- **推断缺位**：`--intent` 只接受一个合法值仍必填；profile 在固件格式与 USB
  身份都能约束的情况下仍必填；单台在场设备仍要求人工抄 observation-id；
  daemon 必须手动 start。

而检视安全模型后可确认：**唯一个在每次破坏性执行中都不可被推断的动作，
是对具名数据影响表示同意**。内容尚未给出、设备/型号存在真歧义、或当次
campaign 需具名时，操作者仍必须提供对应必要信息。除这些决策点外，
plan 前各阶段可由 CLI 代劳：它们只读或仅写主机存储，plan 最终仍封入精确
observation、content hash 与 effect 集。

## Decision

按三条原则重切整个命令面：

1. **必要信息学说**：只有四类信息允许要求操作者提供——
   (a) 刷写内容（固件文件/artifact）；(b) 真歧义下的目标身份决策
   （多设备选择，或弱物理身份下的 profile/型号断言）；
   (c) 对破坏性 effect 的具名同意；(d) 授权输入（可复用的 exact HDC 绑定、
   每次受控验收显式给出的 campaign）。
   其余一切必须推断；推断失败输出列出候选与消歧参数的 typed refusal，
   而不是要求输入。
2. **复合输出**：每条命令一次返回该决策点所需的全部信息（内嵌被引用资源的
   摘要而非仅 ID），错误信封携带失败前已完成阶段的事实。Agent 的主任务路径
   在强身份或调用方已持有精确目标事实时从 ~10 次降到 2 次；
   弱 Loader 身份下额外保留 1 次显式身份决策，不为凑路径数而自动猜型号。
3. **按决策边界切颗粒**：只有真实的决策/effect 边界保留独立命令
   （破坏性执行、任务取消、救援域、daemon 生命周期）；仅为搬运数据而存在的
   命令并入复合命令或富化查询。

### 必要信息与推断清单

| 信息 | 分类 | 交互模式（TTY） | 结构化/脚本模式 |
|---|---|---|---|
| 固件内容 | 必要 | 无参时列表选择（CAS 已导入 + cwd 已知格式文件） | `--file` / `--artifact` |
| 设备（多台候选） | 必要（真歧义） | 编号选择器 | `--target <选择器>` / `--device <observation-id>` |
| 弱物理身份下的 profile/型号 | 必要（开放世界歧义） | 输入 profile 声明的型号全称；身份强度不因人工输入升级 | 显式 `--profile` + 精确 `--device`，否则 `IDENTITY_CONFIRMATION_REQUIRED` |
| 破坏性同意 | 必要 | plan 摘要确认屏 | `--ack <token>...` 精确覆盖 |
| HDC | 必要（一次性配置） | `arkforge config set` | 同左 |
| campaign | 必要（每次受控验收显式开启） | 当次命令给 `--hardware-campaign` | 同左；永不持久化为默认值 |
| profile（强身份） | 推断：固件格式 ∩ USB 模式身份 ∩ 可证明型号的 probe 事实，交集恰一 | `--profile` 可显式覆盖 | 同左 |
| intent | 推断：组合合法 intent 恰一时默认 | `--intent` 仅作覆盖 | 同左 |
| 设备（单台候选） | 推断：唯一匹配即绑定 | — | — |
| runtime-dir | 推断：平台默认目录 | `--runtime-dir` 仅作覆盖 | 同左 |
| daemon 生命周期 | 推断：需要时自动拉起（`--no-auto-start` 退出） | — | — |
| plan/apply 间一切搬运 | 推断：run 同进程闭合；plan 输出内嵌完整 apply 命令行 | — | — |

推断永不放宽歧义拒绝：零台、多台、交集为空或多于一个，以及
“已注册 profile 中恰一”但物理型号仍无法证明的开放世界歧义，一律不得
静默升级为强身份。交互模式可就 (a)(b)(c) 三类当次必要信息询问；
结构化模式必须通过显式参数消歧。

## Command surface

~~~text
arkforge                      # = arkforge status
├── status                    # 聚合快照：host + runtime + devices(已识别) + artifacts + jobs + blockers
├── flash
│   ├── run [FILE]            # 一步刷机（bare `arkforge flash` 即 run）
│   └── plan [--assess-only]  # 复合 staging：import + 识别 + assess + 封 plan，一次调用
├── apply                     # 执行 sealed plan（normal flash 与 recovery plan 共用的同意动词）
├── watch [--job <id>]        # 默认最近活动 job
├── cancel                    # --job --expect-sequence
├── device
│   ├── list [--deep]         # 富化：识别、候选 profile、身份强度（probe 并入 --deep）
│   └── wait
├── artifact
│   ├── import                # 复合输出：manifest + 兼容 profile + 在场匹配设备
│   ├── list
│   └── show                  # 离线富化（原 inspect 并入）
├── job
│   ├── list
│   ├── show                  # 内嵌事件尾、收据、恢复指引（原 recovery guide 并入）
│   ├── reconcile
│   └── recover               # 复合恢复计划（原 recovery plan），产物经顶层 apply 执行
├── rescue                    # 救援域保持显式与细颗粒，刻意不流线化
│   └── list / inspect / read / plan / apply
├── daemon
│   └── run / start / stop    # status 并入顶层 status
├── config
│   ├── show / set / unset    # HDC 原子绑定/清除
│   └── add / remove          # 附加 profile-file（绝对路径 + digest）
├── signing verify
├── completion
└── help [PATH...] [--all]    # --all 或 JSON 无路径时一次输出整棵树
~~~

被移除的 leaf 与去向（unreleased，无兼容别名，同提交删除）：

| 旧 leaf | 去向 |
|---|---|
| `doctor` | `status`（host/blocker 段） |
| `daemon status` | `status`（runtime 段） |
| `device show` | `device list --device <id>`（过滤） |
| `device probe` | `device list --deep` |
| `artifact inspect` | `artifact show`（离线富化）；`artifact import` 输出已内嵌 |
| `flash assess` | `flash plan --assess-only`（plan 失败分支本就返回 assessment） |
| `flash apply` | 顶层 `apply` |
| `job watch` / `job cancel` | 顶层 `watch` / `cancel` |
| `job recovery guide` | `job show` 内嵌 |
| `job recovery plan` | `job recover` |

## 路径长度对比（正常刷写 DAYU200）

| 角色 | 现行 | 本 change 后 |
|---|---:|---:|
| 人类（TTY） | 5–7 条命令 + 4 次 ID 搬运 | **1 条**（`arkforge flash fw.tar.gz` → 确认屏） |
| Agent（强身份/已知精确目标） | 最多 10 次调用 | **2 次**（`flash plan --file … --output json` → `apply … --ack …`） |
| Agent（弱 Loader 身份） | 最多 10 次调用 | **3 次**（`device list` → 显式 profile/device 的 plan → apply） |
| CI 脚本（profile/device/ack 已钉死） | 同 Agent | **1 次** `flash run`；需分阶段 review 时为 2 次 plan → apply |

## Required semantic boundaries（保持不变）

- `apply`（以及 `flash run` 的执行段）仍要求 sealed plan、精确
  `--expect-plan-sha256`（run 同进程内部闭合）与 required_acknowledgements
  的精确覆盖；`UNEXPECTED_ACKNOWLEDGEMENT` 与 effect 漂移拒绝原样保留。
  宽泛 `--yes` / `--force` 仍不存在。
- 交互确认屏是 acknowledgement 的**交互式接受**而非豁免：
  独立 CLI authority approval record 记录 exact plan/token 与来源
  （`interactive-tty` / `argv`）；`arkforged` journal/receipt 不改。
- 设备自动绑定是"歧义必须拒绝"原则的接续：唯一匹配才绑定，plan 仍封精确
  observation，mutation 前身份复验不变。
- 唯一的已注册 profile 不等于唯一物理型号：Loader/Maskrom 下若仅有
  VID/PID、mode 与 `DEVICE_INFO Mode=...`，交互路径每次要求输入型号全称，
  非交互路径必须显式给 `--profile` 与精确 `--device`。人工断言不提升
  `identification.strength`。
- HardwareCampaign 仍按 AFD-0004 每次具名开启；`config` 不接受 campaign 键。
  已运行 runtime 的 campaign 与当次显式值不一致时 typed refusal，不自动重启。
- rescue 仍为显式域、独立 plan/receipt/evidence，永不被 normal flash 自动选择，
  且本 change 刻意不为其做推断与交互流线化——处于救援场景时，显式与细颗粒
  本身就是功能。
- mechanics maturity 与 authority support 双门、HardwareCampaign 语义、
  DAYU600 不可执行（18 条证据门）均不受影响；`flash run` 对识别为 DAYU600
  的设备照常给出 typed 不可执行结果。

## 对 CHG-2026-CLI-arkforge-agent-native-cli 的条款修订

1. "No command prompts for input" → 收窄为：**stdin/stdout/stderr 任一不是 TTY、
   `--output json|jsonl`、
   `--no-input` 下永不提示**；human TTY 模式对三类必要信息允许一次性询问。
   同一调用在 CI/Agent 中的确定性承诺不变且更精确。
2. "`flash plan` never accepts a firmware path" → 保留于该条款原意（禁止跳过
   内容寻址），`flash plan/run` 的 `--file` 为**隐式 import 复合**：字节仍先
   进 CAS，hash 仍封入 plan，输出报告 artifact_id。
3. "所有 destructive 操作采用 plan → apply（两条命令）" → 重述为不变量：
   **无 sealed plan、无精确 token 覆盖、effect 漂移不拒绝，则无破坏性执行**。
   `flash run` 按构造满足（同进程 plan+apply+确认屏/`--ack`）。
4. "`device list` never chooses a default / `flash plan` requires `--device`" →
   重述为：**歧义永不默认**；唯一匹配可自动绑定，绑定结果必须在确认屏/输出
   中披露识别依据与身份强度；弱物理身份不得因“在已知 profile 集合中唯一”
   而静默当作已识别型号。
5. help 契约新增 `arkforge.command-help-index/v1`，其 `commands` 按 path 字典序
   内嵌 `arkforge.command-help/v1` leaf；leaf 现有字段含义不变，加性新增
   `runtime_effect` 与 `facts_projections`。

## Out of scope

- 不开放 DAYU600 execute；不触碰 18 条证据门。
- 不修改 IPC 协议、supervisor 配对、permit codec、`arkforged` execution journal/
  receipt 格式。CLI authority approval record 是本 change 新增的前端审计域，
  不输入 mechanics evidence。
- 不引入第三方 Rust 运行时依赖：交互层 v1 为编号列表 + 行提示
  （无 raw-mode 全屏 TUI）；全屏 TUI 若做，是后续独立 change 且须自研终端层。
- 不提供远程 daemon、多租户、任意 argv 透传。

## Safety and rollback

- 本 change 为前端重组：回滚即恢复旧命令树，不影响 `arkforged` journal、
  已 sealed plan 与 ArkDeck authority。
- 若回滚时已存在 CLI authority approval record，它们仅作审计证据保留；
  旧版不读取也不将其解释为 permit/receipt。
- 每阶段交付沿用 unreleased migration 规矩：canonical handler、human/JSON
  help、测试与仓内调用方同车，旧 leaf 同提交删除，无新旧并存期。
