# AF-V2 验收证据（第一验收项已通过；余项见 §6 后记）

> 首记日期：2026-08-15；后记日期：2026-08-19
>
> 范围：architecture.md 22 `AF-V2：DAYU200 ArkForge production cutover`
>
> **2026-08-18，第一验收项 `real DAYU200 full flash pass` 通过。**
> `flash.dayu200` 端到端 `succeeded`：ArkDeck 签发 permit，`arkforged` 写入
> 全部九个分区、自发复位、postflight 验证身份与构建(设备答
> `OpenHarmony-7.0.0.37`)。job `job-a4b7d539571082b1958ebaaf2c14bd2c`——
> **用的是当日的 fixed-tool 执行面**；同日起步的 NRU-001..004 随后把执行面
> 换成原生 RockUSB 并移除 vendor 运行时，2026-08-19 纯原生二进制全量复验
> 亦 `succeeded`(`job-b00e006a1fbe9d6de388efab4138b9a2`)。两次运行与换轨
> 过程见 [首过与原生换轨](runs/2026-08-19-dayu200-green-flash-and-native-cutover.md)。
>
> **本文仍不整体宣布「AF-V2 已验收」**：逐项状态见 §6 后记——真机 crash 半、
> 掉电、多设备仍未验证，ProductionVerified 的成熟度提升是另一个决定(AD-025)。
>
> §1–§5 是 2026-08-15 的原文，按当日事实记录(当日设备写入次数为 0，
> 且执行面还是后来被 NRU-004 退役的 fixed-tool 路径)。保留它是同一个理由：
> 一份等到全绿才写的验收文档，会让中途的部分结论无处安放。
>
> 复现(当日)：`cargo test --workspace --offline`(441 tests，全绿)。
> 真机部分见 [2026-08-15 彩排](runs/2026-08-15-dayu200-flash-rehearsal.md)。

---

## 1. 一句话结论

ArkForge 侧的刷写机制**完整且在真机上验证到写入前的最后一步**。
缺的是一张 StepPermit，而本仓刻意签发不了——架构守卫禁止 `crates/arkforged`
引用签发函数（`the_daemon_never_mints_a_permit`）。

2026-08-15 已定：**ArkDeck 做 authority**，即 architecture.md 22 节 AF-V2 的原意。
ArkForge 这侧要交的东西已经全部交付，提案在
[docs/openspec/chg-arkdeck-arkforge-authority](../openspec/chg-arkdeck-arkforge-authority/proposal.md)。

---

## 2. 生产代码逐条

| architecture.md 22 要求 | 状态 | 交付 |
|---|---|---|
| ArkForge durable engine | ✅ | `arkforge-engine::{journal,durable,recovery,superseding}` |
| ArkDeck adapter | 🟡 ArkForge 半完成 | `adapters/arkforge-arkdeck-adapter::{lib,control}`；Swift 半属 ArkDeck 仓 |
| StepPermit（含 8.6 完整性与重传信任模型） | ✅ | `arkforge-engine::step`；交叉验证向量 `docs/openspec/…/permit-vectors.md` |
| ManagedDeviceControlPort | ✅ | typed 动作 + Provider 侧显式拒绝 + 发布的映射表 + daemon 侧 API 13 |
| controller execution/admission surface | ✅ | API 6/7/8/12/13 全部实现;`crates/arkforged/src/jobs.rs` |
| execution readiness（机器可读） | ✅ | 两个常驻事实(配对 + 工具绑定)在握手里报出;工具摘要**强制**与 `--rkdeveloptool-sha256` 比对，不符拒绝启动;绑定后还要**证明能跑**(device-free 探测 + 5 秒超时)，AD-015 的 quarantine 情形已实测复现并诊断(AD-022);计划的 toolchain 摘要不符拒为 `TOOLCHAIN_DIGEST_MISMATCH` |
| dispatch（执行侧接线） | ✅ 软件层 | `crates/arkforged/src/dispatch.rs`;服务锁之外运行，十一条端到端测试用脚本化 tool port 跑完整个计划（九条 `wlx` + 读域三态）。**未在真机上跑过** |
| Rockchip fixed-tool Provider | ✅ | `arkforge-provider::rockchip_execute` |
| generic Runtime integration | ⛔ | ArkDeck 仓 |
| generic UI | ⛔ | ArkDeck 仓 |
| compatibility alias | ⛔ | 换实现之后另开 change |
| legacy decoder | ⛔ | 同上 |
| arkforged signing/entitlement/packaging 契约 | ✅ | [AFD-0003](../decisions/AFD-0003-arkforged-signing-packaging.md);`crates/arkforged/src/packaging.rs`、`packaging/macos/`。两个二进制 entitlement 字典皆空并在绑定前强制(AD-007 的留白已对齐);release 签名形状由 `--require-release-signing` 强制。**公证未做过**——nested code 随容器公证，属 ArkDeck 侧 |

### 2.1 durable engine

- **journal**：hash 链 + schema version + sequence + previous digest + record digest
  + **fsync policy**。策略是记录种类的函数，不是可调项；声明了比自身种类更弱策略的
  记录按篡改处理（`a_record_that_downgrades_its_own_durability_is_refused`）。
- **撕裂尾部**：`every_torn_tail_replays_as_a_prefix_or_is_refused` 穷举了真实 journal
  的**每一个**字节截断位点，断言每一次要么作为更短前缀被接受、要么被拒绝，
  且截断后文件长度 + 报告的撕裂字节数 = 截断前长度（每个字节都有交代）。
- **13.3 崩溃处置表**：由 journal 推导而非由调用方回忆
  （`arkforge-engine::recovery::CrashDisposition`）。
  `no_disposition_permits_a_new_external_effect` 断言七种处置无一允许新的外部效果。
- **边界**：durability 只声明到进程死亡为止，掉电未验证（AD-017，architecture.md 13.2.1）。
  ledger 检查断言这条保持 open。

### 2.2 StepPermit 单次使用

顺序由类型强制，逐个 by value 消费：

~~~text
admit_step → AdmittedStep → begin_dispatch → DispatchInFlight
           → record_receipt → CompletedStep → checkpoint
~~~

四个 token 都没有公开构造函数。调用方无法二次派发、无法为没开始的派发记回执、
无法给没有回执的步骤打 checkpoint——不是靠自觉，是靠拿不到那个值。

跨重启的单次使用由 durable ledger 保证：
`a_consumed_permit_is_refused_after_a_restart`、
`a_permit_caught_mid_dispatch_is_refused_as_unknown_rather_than_retried`、
`one_permit_cannot_produce_two_intents`。

### 2.3 Rockchip fixed-tool Provider

封闭命令面 `ld` / `ppt` / `wlx` / `rl` / `rd`。argv 只在
`RockUsbCommand::argv` 一处构造，调用方无法提供。

刻意缺席的两类：

- `db` / `ul` / `gpt` —— Maskrom 阶段命令。本 Provider 只声明 Loader 模式适用，
  遇到不适用的设备就阻塞，不去够相邻的东西。
- `wl` —— 扇区寻址写。`wlx` 由设备自己的表解析地址，而那张表刚被上一步证明与计划一致。
  扇区寻址的后备会让写入落在没有任何观测确认过的地址上。
  计划里原本带着 `fallbackCommand: wl` 字段——执行器永远不会发它，
  于是那是计划声称而执行器没有的能力，已删。

---

## 3. 验收逐条

### 3.1 ⛔ real DAYU200 full flash pass

**未通过。设备写入次数 0。**

原因不是能力缺失，是 authority 归属：一次写入需要 StepPermit，
一张 permit 需要 authority 签发。见 §1。

已经证明的部分（[彩排证据](runs/2026-08-15-dayu200-flash-rehearsal.md)）：

- 九条 `wlx` 全部降解成真实 argv；
- 每条通过三项前置校验：Profile 允许、设备自身分区表一致、镜像写前 revalidate；
- 九个镜像真实落盘（4,017,485,774 字节，86.8 MiB/s），SHA-256 全部与 ArkDeck 钉值一致；
- 一条 `rd` 同样被扣留。

### 3.2 ✅ exact identity

两种人格都被 Profile 的实测 USB 身份认出，`identityStrength` 均为 `serialAndTopology`：

~~~text
hdc-normal      0x2207:0x5000  "HDC Device"
rockusb-loader  0x2207:0x350a  "USB download gadget"
~~~

**multi-device 未验**：本环境只有一块板子。多板的 exact 绑定没有硬件可测，
不作任何声称。

### 3.3 ✅ nine partitions/userdata（软件层已派发，真机未派发）

真机彩排：九个目标全部降解、前置校验通过、镜像 revalidate 通过，**未派发**。
`system` 恰好铺满它的 4,194,304 扇区，其余八条有余量。

软件层：`a_job_dispatches_every_step_and_reaches_a_verdict_on_each` 用脚本化
tool port 把九条 `wlx` 真的发了出去，按 Profile 声明顺序，`ppt` 在前、`rd` 在后。
那是一个脚本，不是设备——**这条不构成真机通过**。

### 3.4 ✅ read-domain-aware verification（readback/typed-skip）

真机实测，读面为 **windowed**（sector 1 读到真实数据，sector 19955712 读到 uniform 0xCC）：

| 判定 | 数量 | 目标 |
|---|---:|---|
| Verified | 1 | `uboot`（读窗内，且板上内容与归档逐字节相同） |
| Failed | 2 | `resource`、`boot_linux`（读窗内，读到真实内容，但板子跑的是 7.0.0.37） |
| TypedSkip | 6 | `ramdisk` 起，全部在读窗外 |

读窗边界落在 40960 与 237568 之间，与 AD-006 记录的 65536 相容——
且这次由一条与 AF-V1 capture 完全不同的代码路径独立测出（AD-019）。

`TypedSkip` 不计入任何 verified 强度，这一条在类型层保证
（`VerificationOutcome::verified_strength` 对非 Verified 返回 `None`）。

### 3.5 🟡 build postflight

期望值已就位：从真实 `system.img` 提取到 `OpenHarmony-7.0.0.36`
（AD-016，位于第 320,762,067 字节）。归档文件名写的是 7.0.0.35，
build log 也写 7.0.0.35——这正是期望值必须来自被写入的镜像的原因。

**比对未执行**：需要先写入。

### 3.6 🟡 rebind 瞬态容忍与 normal 别名真机复验

**瞬态容忍 ✅**（AD-020）：

| 方向 | 空窗 | 单次采样最多匹配 | serial | topology |
|---|---:|---:|---|---|
| normal → loader | 3,725 ms | 1 | 变 | 变 |
| loader → normal | **15,579 ms** | 1 | 变 | 变 |

回 normal 的空窗 15.6 秒；任何更短的 deadline 都会把健康的板子判成没回来。
`serialPolicy` 与 `topologyPolicy` 两条 `may-change` 各被独立复现一次。

**normal 别名 ⛔ 未验**：`normal` 是 hdc 的词汇，不是 ioreg 的。
USB transport 走 VID/PID → Profile → mode，从头到尾没见过别名要重命名的那个字符串。
要验它得走 `ManagedDeviceControlPort`，而那一侧是 authority 的。

### 3.7 🟡 crash/cancel/fault

- **crash 语义 ✅（软件层）**：13.3 的每一行都有对应测试；撕裂尾部穷举验证。
- **cancel 语义 ✅（软件层）**：状态机断言 permit 之前可 `CancelledSafe`、
  intent 落盘之后不可（`cancellation_before_a_permit_is_safe_but_not_after_dispatch`）；
  `wlx` 声明为不可中断（`only_a_write_is_non_interruptible`）。
- **真机 ⛔**：在写入中途杀掉 `arkforged` 需要先有写入。

### 3.8 🟡 outcomeUnknown no replay

**软件层 ✅**：

- 状态机没有从 `OutcomeUnknown` 回到任何可派发状态的边
  （`an_unknown_outcome_never_returns_to_a_dispatching_state`）；
- `ActionDisposition::permits_redispatch` 对四个变体全部返回 false；
- 重启后遇到消费中断的 permit 判为 `OutcomeUnknown` 并拒绝，不重放。

**真机 ⛔**：同 3.7。

### 3.9 🟡 eligible complete-overwrite recovery

只读半 ✅（`arkforge-engine::superseding`）：

- `possible_effects` —— 保守并集；边界不可界定时 `Unbounded`；
- `reconcile` —— 四态判定，含「仍然未知」；读窗外的观测判为 `Indeterminate`
  而不是失败；
- `assess_superseding_recovery` —— `Unbounded` 在查覆盖之前就否决；
  超出已发布覆盖的效果判为**不合格**而非尽力而为。

**当前所有 Profile 都不合格**：`profiles/dayu200.yaml` 的
`recovery.supportsCompleteOverwrite: false`。这是今天的诚实答案，
要变必须先发布并 review 一份覆盖声明。

recovery **plan 的物化 ⛔**：需要真实写入产生的 possible effect set。

### 3.10 ⛔ ArkDeck production lowering 无 Rockchip command/address

ArkDeck 仓变更。提案 §1 列出了要删的两个 case 及其全部实现，
`control.rs` 的归属表断言只有 `flashPartitions` 与 `verifyFlashReadback` 被移交。

---

## 4. 本次纠正的自身错误

七条，全部是我写的东西在真机或真实归档上失败：

| # | 错误 | 症状 | 证据 |
|---:|---|---|---|
| 1 | `BUILD_FACT_SCAN_BYTES = 64 MiB`，注释称事实「在镜像头部」 | 真实归档一条构建事实都提不出 | AD-016 |
| 2 | 值语法在第一个空格截断 | `const.product.name` 会被钉成 `"OpenHarmony` | AD-016 |
| 3 | `ppt` 解析器要四列带 `0x` | 真机三列裸十六进制无 size 列，零行命中 | AD-018 |
| 4 | 布局摘要拿 Profile 九目标比设备十五行 | 永远不可能相等 | AD-018 §3.1 |
| 5 | 计划把 readback 排在 characterize 之前 | 正是 AD-006 冤案的成因 | 彩排 §4.1 |
| 6 | 彩排工具派发了 `rd` | 自称只读的工具重启了设备（未生效纯属运气） | 彩排 §6.1 |
| 7 | `normal` 别名检查是空的 | 绿色运行会被读成「别名也验过了」 | 彩排 §7bis |

第 1、3、5 条都有同一个形状：**我写的夹具通过了，真实的东西没有**。

---

## 5. 边界与不声称

- **掉电**：未验证。AD-017 记为 open。
- **multi-device**：单板环境，未验。
- **ArkDeck 侧任何东西**：本仓不能代它验收。
- **maturity**：`RK-M02` 仍为 `hardwareGated`。彩排产出的是 **PlanAssessment**，
  不是 Executable plan。任何一次通过只发布它自己那一个组合——maturity 是组合键。
- **simulation / plan-only 不记 real hardware pass**（ledger §5）：
  本文每一条 ✅ 都注明了是软件层还是真机。

---

## 6. 后记（2026-08-19）：全绿之后的逐项状态

通过在前、换轨在后：第一次真机写入(2026-08-18 `job-a4b7d539…`)走的正是
§2 描述的 fixed-tool 路径——那是它第一次也是最后一批写入。同日 16:20 起
NRU-001..004 落地原生 RockUSB(`NativeRockUsbPort` + 七个语义动作)、默认切换、
移除 vendor 运行时；2026-08-19 纯原生二进制全量复验 `succeeded`
(`job-b00e006a…`，其回执自证 daemon 摘要)。所以 §2 表格从"现行机制描述"
变成了"当时已交付什么"的历史记录，现行机制见
[首过与原生换轨](runs/2026-08-19-dayu200-green-flash-and-native-cutover.md)。

| §3 验收项 | 2026-08-15 | 现状 |
|---|---|---|
| 3.1 real DAYU200 full flash pass | ⛔ 写入 0 | **✅ 2026-08-18 通过**(fixed-tool 面；08-19 原生面复验同绿，[证据](runs/2026-08-19-dayu200-green-flash-and-native-cutover.md)) |
| 3.2 exact identity | ✅(单板) | ✅ 不变；multi-device 仍未验 |
| 3.3 nine partitions/userdata | 软件层 | **✅ 真机派发**，九目标全部写入并逐一摘要比对 |
| 3.4 read-domain-aware verification | ✅ 真机 | ✅ 两次全刷逐分区同形(2 Verified / 1 结构性 Failed / 6 TypedSkip，见换轨证据 §4) |
| 3.5 build postflight | 🟡 比对未执行 | **✅ 已执行**：设备答出被写入归档声明的 `OpenHarmony-7.0.0.37`(期望值来自镜像本身，与 AD-016 的原则一致) |
| 3.6 rebind / normal 别名 | 🟡 | ✅ 瞬态容忍不变；绿刷的委托 postflight 以 bound-HDC 别名重认(`verification: exact-published-profile-and-bound-hdc`)——别名验证发生在 authority 侧，本仓照旧不代它验收 |
| 3.7 crash/cancel/fault(真机半) | ⛔ | **仍 ⛔**：写入中途 SIGKILL 未在真机做过 |
| 3.8 outcomeUnknown no replay(真机半) | ⛔ | **仍 ⛔**：同上 |
| 3.9 complete-overwrite recovery | 🟡 只读半 | 🟡 不变：Profile 仍声明 `supportsCompleteOverwrite: false`，plan 物化待覆盖声明 |
| 3.10 ArkDeck lowering 无 Rockchip command/address | ⛔ | ✅ ArkDeck 侧 chg-2026-059 已落地(该仓 contract tests 守卫)；本仓适配层归属表照旧断言仅两个 case 移交 |

ProductionVerified 的提升不随本次通过自动发生——那需要维护者按 AD-025
对着 evidence set 另做决定。
