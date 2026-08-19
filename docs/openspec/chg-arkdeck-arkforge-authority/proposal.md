---
id: CHG-YYYY-NNN-arkdeck-arkforge-authority
revision: 1
status: draft # 维护者 review + merge 本 proposal PR 后才生效
class: capability
core_change_level: none
owner: lvye
core_baseline: CORE-3.0.0
platforms: [macos]
---

# ArkDeck 保留 authority，把 Rockchip lowering 交给 ArkForge

> **NRU-004 超越声明（2026-08-19，ArkForge main `c049a11`）**：本稿能力表中的
> 「Rockchip fixed-tool Provider ✅ 封闭命令面 `ld`/`ppt`/`wlx`/`rl`/`rd`」与
> §4「`rd` 由 ArkForge 通过它自己的 fixed-tool port 发出」均已退役——执行面现为
> `arkforged` 内建原生 RockUSB（`NativeRockUsbPort` + 七个语义动作），`rd` 对应
> `reset-device` 语义动作。「设备写入次数 0」也已过时：2026-08-18 `flash.dayu200`
> 全绿（`job-a4b7d539571082b1958ebaaf2c14bd2c`）。本提案的 authority 分界
> 本身不受影响，仍是现行事实。

> 恰四类声明：本 change 不新增 published operation——`flash.dayu200` 保持原样。
> 它改变的是该 operation 的**实现归属**：ArkDeck 继续做 authority 与 HDC owner，
> 停止在自己进程里降解 Rockchip 命令与扇区地址，改为向 `arkforged` 签发 StepPermit。
> 因为触及设备执行栈与 provider action 集合，按 `AGENTS.md` 控制平面条款走
> OpenSpec + 维护者 PR review/merge。

## §19 治理循环四问

1. **对应的真实安全风险**：destructive 的 Rockchip 写入路径目前有**一份**实现，
   在 ArkDeck 进程内；它同时持有 authority、HDC 所有权和 `wlx`/`rl` 的 argv 与扇区
   地址。这让「谁批准了这次写入」和「谁执行了这次写入」落在同一个信任域里，
   审计时无法把两者分开。`RockchipRuntimeActionHost` 里 `flashPartitions` 与
   `verifyFlashReadback` 两个 case 是这份耦合的落点。
2. **为什么不能直接通过 Runtime 缺陷修复**：这不是缺陷，是边界。把执行侧移出去需要
   一个 provider action 集合的收缩和一条新的 permit 信道，两者都是 Repo-plane 变化。
3. **推进哪个 Golden Journey**：GJ-4。今天 DAYU200 刷写只能由 ArkDeck 自己降解；
   本 change 之后同一条 Journey 由 ArkDeck 批准、由 ArkForge 执行，
   而 ArkDeck 的生产 lowering 里不再出现任何 Rockchip 命令或地址。
4. **为什么不会产生后续治理连锁**：本 proposal 合入即批准；只创建一个垂直实现任务。
   ArkForge 侧的对应实现已经完成并在真机上验证到写入前的最后一步
   （见下「ArkForge 侧现状」），本 change 不为它再开治理项。

## Why（根因）

`RockchipRuntimeActionHost` 今天做四件事：拥有 HDC、决定 authority、
降解 `rkdeveloptool` 命令、解释设备读回。前两件只有 ArkDeck 能做；后两件是设备机制，
与 ArkDeck 的产品语义无关，而且已经被证明**难以在产品代码里做对**：

- 2026-08-04 的九次「写入未落盘」判定全部是冤案。它们来自 `rl` 在读窗之外返回的
  uniform filler，而写入实际已落盘并可启动（ArkForge AD-006）。修复链 PR #1066–#1070
  是在产品代码里补 read-domain 语义——这类知识每一条都要在 ArkDeck 里重学一遍。
- 同一份知识在 ArkForge 里已经是可测试的机制：读域三态判定
  （Verified / TypedSkip / Failed）、erased-medium filler 的独立分类、
  「TypedSkip 不计入任何 verified 强度」的类型级保证。

`architecture.md`（ArkForge）9.1 已经写明这条分工：ArkDeck 拥有 HDC endpoint、
server ownership、connectKey、target binding；ArkForge 只能通过 typed
`ManagedDeviceControlPort` 请求语义动作。本 change 把这条分工落到代码上。

## ArkForge 侧现状（本 change 的前置事实，非本 change 的工作）

ArkForge 已经完成并在真实 DAYU200 上验证到写入前的最后一步：

| 能力 | 状态 |
|---|---|
| 真实归档导入 + 17 成员逐值 parity | ✅ 与本仓 `RockchipFlashProfile.dayu200` 钉值逐项一致，机器检查 |
| 构建事实提取 | ✅ 从真实 `system.img` 提取 `OpenHarmony-7.0.0.36`——与本仓 2026-08-04 在刷好的板子上实测到的答案一致 |
| durable engine | ✅ journal 落盘 + fsync policy 随 record kind 固定；撕裂尾部穷举复原；13.3 崩溃处置表由 journal 推导 |
| StepPermit 单次使用 | ✅ 跨进程重启由 durable ledger 保证；顺序由类型强制，逐个 by value 消费 |
| Rockchip fixed-tool Provider | ✅ 封闭命令面 `ld`/`ppt`/`wlx`/`rl`/`rd`；argv 只在 Provider 内降解 |
| 读域三态判定 | ✅ 真机实测：1 Verified / 2 Failed / 6 TypedSkip，读窗边界与 AD-006 相容 |
| 九个镜像落盘 + 写入前 revalidate | ✅ 4,017,485,774 字节，九个 SHA-256 全部与本仓钉值一致 |
| 九条 `wlx` 真实 argv 降解与前置校验 | ✅ 全部通过，**全部未派发**——设备写入次数 0 |

缺的只有一样：一张 permit。ArkForge 刻意签发不了——它的架构守卫禁止
`crates/arkforged` 引用签发函数。这正是本 change 要补上的那一半。

## What changes

### 1. `RockchipProviderAction` 收缩两个 case

删除（不是绕过）：

- `case flashPartitions(RockchipRuntimeFlashBundle)`
- `case verifyFlashReadback(RockchipRuntimeFlashBundle)`

以及它们在 `RockchipRuntimeActionHost` 里的实现、`rkdeveloptool` 的
`wlx`/`rl` argv 构造、`RockchipPinnedPartitionTable` 的扇区跨度守卫、
`RockchipWriteProgressParser`、`characterizeMediumReadDomain`。

> 留一份「以防万一」的 lowering 等于对同一条 destructive 路径保留两份实现，
> 这正是 ArkForge `architecture.md` 21.3 明令禁止的。

保留的十一个 case 全部是 HDC 侧的，只有 ArkDeck 能做。完整归属表见
`adapters/arkforge-arkdeck-adapter/src/control.rs`（ArkForge 仓），
其中每一个 baseline case 都被分类为 keptByAuthority / keptInternal /
delegatedToArkForge，并有测试断言三类之和穷尽 baseline。

### 2. ArkDeck 实现 `ExecutionAuthority`

`flash.dayu200` 的 Runtime dispatch 改为：

~~~text
RuntimeJobEngine
  → arkforged materializePlan（已有的只读 API）
  → 对每个 public step：现有的 authorization/confirmation 判定不变
  → 签发 StepPermit（HMAC over canonical CBOR body，keyed by pairing secret）
  → arkforged 执行该 step，返回 ActionReceiptSummary
  → RuntimeJobEngine 记 journal、驱动 UI
~~~

permit 的字段与签名体由 ArkForge `arkforge-authority-api` 定义；
本 change 在 Swift 侧实现同一套 canonical CBOR 编码与 HMAC-SHA256。
**重传必须重放已存字节**，不得确定性重新推导——两份字节不同的「同一张」permit
正是完整性标签要消除的歧义。

### 3. ArkDeck 实现 `ManagedDeviceControlPort`

四个语义动作，映射到保留下来的 provider action：

| 语义动作 | ArkDeck action 序列 | 语义成功 |
|---|---|---|
| `EnterUpdater` | `observeHDCNormalUSB` → `enterLoader` → `waitForHDCDisconnect` → `waitForLoader` → `rebindLoader` | 命令被接受 **且** 绑定身份断开 **且** 恰好一台设备以 Loader 重新绑定 |
| `RebootToNormal` | `waitForBoundHDCReconnect` | 原绑定目标以相同 stable identity 回到 normal |
| `ReadProductFacts` | `verifyBoundBuild` | 绑定目标答出 `const.product.model` |
| `ReadBuildFacts` | `verifyBoundBuild` | 绑定目标答出 `const.ohos.fullname` |

`EnterUpdater` 是四次观测而不是一条命令。只映射 `enterLoader` 会让
「命令被接受」被记成「设备已进入 Loader」。

回执里**不得**出现：`connectKey`、hdc 可执行路径、hdc endpoint、argv、shell、
server lifecycle 动作。ArkForge 侧有断言，ArkDeck 侧需对应的 secret-scan 测试。

### 4. `rd` 的归属

`rebootToNormal` 是唯一一个设备半边是 Rockchip 命令的控制动作——
Loader 模式下设备没有 HDC 可说话。`rd` 由 ArkForge 通过它自己的 fixed-tool port 发出；
ArkDeck 出的是只有它能出的那一半：盯住那台**确切绑定**的设备回来。

### 5. maturity 与证据

ArkForge 的 `RK-M02` 目前是 `hardwareGated`——「AF-V2 要求先有一次真机全量刷写通过」。
本 change 的实现 PR 完成那一次刷写后，**只发布这一个组合**
（ArkDeck authority + 本机平台 + 该 toolchain digest + 该 evidence set）为
ProductionVerified。maturity 是组合键，一次通过不解锁别的组合。

## Out of scope

- `compatibility alias`（`flash.dayu200` → generic adapter）与 legacy decoder：
  本 change 之后 `flash.dayu200` 的实现已经换掉，别名与 legacy 解码另开 change。
- generic UI：本 change 不改 UI，Runtime 事件形状保持不变。
- `arkforged` 的签名/entitlement/打包契约：对齐 #1299 体系，另开 change（ArkForge AD-007）。
- DAYU600 / Unisoc：ArkForge `architecture.md` 17.5 十八条证据门 0 条 PASS，
  本 change 不涉及。

## Safety, privacy, compatibility and rollback

- **安全**：destructive 路径从「一个进程既批准又执行」变成「ArkDeck 批准、
  ArkForge 执行、permit 单次使用且跨崩溃有效」。写入前的三方一致
  （Profile allowlist、设备自身分区表、artifact manifest）由 ArkForge 强制，
  任一不符在 spawn 之前拒绝。
- **隐私**：permit 与回执都不携带 HDC 路径、connectKey 或 argv。
- **兼容**：`flash.dayu200` 的 operation 契约、step 集合、UI 事件形状不变；
  变的是谁执行。已有 journal 不需要迁移。
- **回滚**：本 change 是删除 + 委派。回滚 = revert 实现 PR，
  `RockchipRuntimeActionHost` 的两个 case 与其实现随之回来。
  没有需要清理的持久状态；ArkForge 的 journal 是它自己的目录。
- **不回滚的部分**：即便回滚，2026-08-04 的读域教训仍然成立——
  ArkDeck 的 `characterizeMediumReadDomain` 不能因为本 change 被删掉而被忘记。
  实现 PR 需在 `docs/` 留一条指回 ArkForge AD-006/AD-019 的索引。

## 强制重复与新任务自检（PRODUCT-LOOP §5/§17）

本 change 不新增治理框架、不创建 readiness-only PR，
只创建一个垂直实现任务 `TASK-AFA-001`，其实现 PR 同车交付：
Swift 侧 permit 签发与控制端口、两个 case 的删除、契约测试、
真实 DAYU200 全量刷写与 build postflight、Task done、verification 结论。
