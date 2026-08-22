---
id: CHG-2026-CLI-arkforge-workflow-first-cli
revision: 1
status: draft
class: capability
core_change_level: major
owner: TBD
platforms: [macos, windows]
---

# ArkForge Workflow-first CLI：一步刷机与复合查询面

> 本 change 修订 CHG-2026-CLI-arkforge-agent-native-cli 的**工作流包装**，
> 不修改其 authority supervisor、StepPermit、journal、no-replay、rescue 域分离、
> mechanics/authority 双执行门中的任何一条语义。所有变化都发生在 `arkforge`
> 前端的命令切分、信息推断与输出契约层。

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

而检视安全模型后可确认：**上述步骤中只有一个动作真正必须由操作者完成——对
具名数据影响表示同意**。其余全部可由 CLI 代劳而不弱化任何不变量，因为
plan 之前的每一步都是只读或仅写主机存储，plan 最终封入的是精确 observation、
精确 content hash 与精确 effect 集，与这些输入如何被收集无关。

## Decision

按三条原则重切整个命令面：

1. **必要信息学说**：只有四类信息允许要求操作者提供——
   (a) 刷写内容（固件文件/artifact）；(b) 真歧义下的设备选择；
   (c) 对破坏性 effect 的具名同意；(d) 一次性授权物（HDC 绑定、campaign）。
   其余一切必须推断；推断失败输出列出候选与消歧参数的 typed refusal，
   而不是要求输入。
2. **复合输出**：每条命令一次返回该决策点所需的全部信息（内嵌被引用资源的
   摘要而非仅 ID），错误信封携带失败前已完成阶段的事实。Agent 的主任务路径
   从 ~10 次调用降到 2 次。
3. **按决策边界切颗粒**：只有真实的决策/effect 边界保留独立命令
   （破坏性执行、任务取消、救援域、daemon 生命周期）；仅为搬运数据而存在的
   命令并入复合命令或富化查询。

### 必要信息与推断清单

| 信息 | 分类 | 交互模式（TTY） | 结构化/脚本模式 |
|---|---|---|---|
| 固件内容 | 必要 | 无参时列表选择（CAS 已导入 + cwd 已知格式文件） | `--file` / `--artifact` |
| 设备（多台候选） | 必要（真歧义） | 编号选择器 | `--target <选择器>` / `--device <observation-id>` |
| 破坏性同意 | 必要 | plan 摘要确认屏 | `--ack <token>...` 精确覆盖 |
| HDC / campaign | 必要（一次性） | `arkforge config set` | 同左 |
| profile | 推断：固件格式 ∩ USB 模式身份 ∩ probe 确认，交集恰一 | `--profile` 仅作覆盖 | 同左 |
| intent | 推断：组合合法 intent 恰一时默认 | `--intent` 仅作覆盖 | 同左 |
| 设备（单台候选） | 推断：唯一匹配即绑定 | — | — |
| runtime-dir | 推断：平台默认目录 | `--runtime-dir` 仅作覆盖 | 同左 |
| daemon 生命周期 | 推断：需要时自动拉起（`--no-auto-start` 退出） | — | — |
| plan/apply 间一切搬运 | 推断：run 同进程闭合；plan 输出内嵌完整 apply 命令行 | — | — |

推断永不放宽歧义拒绝：零台、多台、交集为空或多于一个，一律 typed refusal
（交互模式下等价物是"问一次选择"，且仅限 (a)(b)(c) 三类必要信息）。

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
│   ├── list / inspect / read / plan / apply
├── daemon
│   ├── run / start / stop    # status 并入顶层 status
├── config
│   ├── show / set            # HDC 绑定、campaign、附加 profile-file 等一次性项
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
| Agent | 最多 10 次调用 | **2 次**（`flash plan --file … --output json` → `apply … --ack …`；可选 +1 `status`） |
| CI 脚本 | 同 Agent | 2 次（`--ack` 显式，永不提示） |

## Required semantic boundaries（保持不变）

- `apply`（以及 `flash run` 的执行段）仍要求 sealed plan、精确
  `--expect-plan-sha256`（run 同进程内部闭合）与 required_acknowledgements
  的精确覆盖；`UNEXPECTED_ACKNOWLEDGEMENT` 与 effect 漂移拒绝原样保留。
  宽泛 `--yes` / `--force` 仍不存在。
- 交互确认屏是 acknowledgement 的**交互式签发**而非豁免：journal 记录 token
  与来源（`interactive-tty` / `argv`）。
- 设备自动绑定是"歧义必须拒绝"原则的接续：唯一匹配才绑定，plan 仍封精确
  observation，mutation 前身份复验不变。
- rescue 仍为显式域、独立 plan/receipt/evidence，永不被 normal flash 自动选择，
  且本 change 刻意不为其做推断与交互流线化——处于救援场景时，显式与细颗粒
  本身就是功能。
- mechanics maturity 与 authority support 双门、HardwareCampaign 语义、
  DAYU600 不可执行（18 条证据门）均不受影响；`flash run` 对识别为 DAYU600
  的设备照常给出 typed 不可执行结果。

## 对 CHG-2026-CLI-arkforge-agent-native-cli 的条款修订

1. "No command prompts for input" → 收窄为：**非 TTY、`--output json|jsonl`、
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
   中披露识别依据与身份强度。
5. help 契约新增 `--all` 全树输出；`arkforge.command-help/v1` 逐 leaf 结构不变。

## Out of scope

- 不开放 DAYU600 execute；不触碰 18 条证据门。
- 不修改 IPC 协议、supervisor 配对、permit codec、journal 格式。
- 不引入第三方 Rust 运行时依赖：交互层 v1 为编号列表 + 行提示
  （无 raw-mode 全屏 TUI）；全屏 TUI 若做，是后续独立 change 且须自研终端层。
- 不提供远程 daemon、多租户、任意 argv 透传。

## Safety and rollback

- 本 change 为前端重组：回滚即恢复旧命令树，不影响 `arkforged` journal、
  已 sealed plan 与 ArkDeck authority。
- 每阶段交付沿用 unreleased migration 规矩：canonical handler、human/JSON
  help、测试与仓内调用方同车，旧 leaf 同提交删除，无新旧并存期。
