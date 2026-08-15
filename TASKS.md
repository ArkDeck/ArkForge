# ArkForge 实施任务台账

> 状态总览(2026-08-15)：AF-V1 完成；AF-V3 软件半完成；AF-V2 除写入外全部完成。任务定义正本是 [docs/architecture.md](docs/architecture.md) 第 22 节；本文件是执行台账，若两处出入，以 architecture.md 为准并回改本文件。

| 任务 | 内容 | 依赖 | 状态 |
|---|---|---|---|
| AF-V1 | ArkForge Core + DAYU200 read-only parity | — | ✅ 完成([验收证据](docs/evidence/AF-V1-acceptance.md)) |
| AF-V2 | DAYU200 ArkForge production cutover | AF-V1 | 🟡 除写入外全部完成([验收证据](docs/evidence/AF-V2-acceptance.md))；写入待 ArkDeck 侧实现 permit 签发 |
| AF-V3 | DAYU600 evidence + plan-only | AF-V1(共用 API 面) | 🟡 软件半完成([验收证据](docs/evidence/AF-V3-acceptance.md))；证据半需真机 |
| AF-V4 | DAYU600 production execute | AF-V3 + AF-V2(engine) + 17.5 证据门全 PASS | ⛔ 阻塞：18 条证据门 0 条 PASS |

## 全局规矩(摘自 architecture.md 7.4 / 21)

- 新 Operation、Provider、integration/device profile 属 ArkDeck 明确要求 review 的变更类型，必须与真实产品能力同车交付；
- Stage A(AF-V1)在本仓交付，不产生 ArkDeck docs-only PR；
- 不转换 active legacy job、不重解释 legacy plan digest、backend 只能在 external intent 前选择；
- ArkForge unknown 不能自动切回 legacy；migration cutover 必须真实 DAYU200 pass；
- legacy Swift Rockchip lowering 的删除门见 architecture.md 21.3。

---

## AF-V1：ArkForge Core + DAYU200 read-only parity

**目标**

~~~text
artifact import
→ inspect
→ profile validation
→ discover/probe
→ public/private plan materialization
→ plan/effect parity
~~~

**生产代码**

- [x] Rust workspace(八个边界 crate，见 architecture.md 4.2)
- [x] neutral Authority API
- [x] Artifact/CAS
- [x] DAYU200 parser/profile
- [x] Rockchip read-only probe
- [x] PlanAssessment/FlashPlan
- [x] deterministic digest(RFC 8949 CBOR + SHA-256)
- [x] projection validator
- [x] daemon read-only API
- [x] golden transcript 库(GJ-4 campaign receipts ECAMP-96EFFF15 / ECAMP-31E041BC 为种子)

**验收**

- [x] Core 不依赖 ArkDeck/vendor(依赖图守卫为主，词法扫描为辅)
- [x] current DAYU200 archive facts parity(设备侧真机验证 15/15；厂商归档字节级比对待归档到位)
- [x] unknown member/partition fail closed
- [x] private action digest 覆盖
- [x] startExecution disabled(类型层 / 服务层 / 线上 UDS 三层)
- [x] unit/fuzz/transcript tests(277 tests + 18000 变异输入)
- [x] Profile 含 readDomain 与 per-target 验证强度，与 AD-006 一致(2026-08-14 真机复现，AD-009)
- [x] DAYU200 整包 CAS 导入在声明预算内(实测 3.07 s / 227 MiB/s，预算 60 s)
- [x] 无设备 mutation

**额外交付**：`adapters/arkforge-arkdeck-adapter` 的 published step 映射表(5.4)、
`proto/arkforge.proto`、`arkforge-cli` 只读诊断。

**边界**：见[验收证据](docs/evidence/AF-V1-acceptance.md)第 4 节。

## AF-V2：DAYU200 ArkForge production cutover

> **状态(2026-08-15)**：硬件已到位，ArkForge 侧「除写入外的全部」已在真机上跑通，
> 见 [2026-08-15 彩排](docs/evidence/runs/2026-08-15-dayu200-flash-rehearsal.md)：
> 真实归档导入、真实计划物化、九个镜像真实落盘并 revalidate、设备自身分区表实测比对、
> 读域实测(独立复现 AD-006)、九个目标的 readback 三态判定全部来自真机。
> 九条 `wlx` 与一条 `rd` 被降解成真实 argv、逐项前置校验、然后**不派发**。设备写入次数 0。
>
> **剩下的不是代码，是一个决定：谁做 authority。** 一次写入需要 StepPermit，
> 一张 permit 需要 authority 签发，而本仓刻意做不到——架构守卫禁止 `crates/arkforged`
> 引用签发函数(8.6)。两条路都要拍板：(a) ArkDeck 做 authority(22 节 AF-V2 原意，
> 需 ArkDeck 侧 OpenSpec + maintainer review)；(b) 本仓新增 bench authority crate
> (需在 4.3 crate 边界图与架构守卫允许表里各加一行)。
>
> 另有一条独立的门 `RK-M02`：maturity 现为 `hardwareGated`——「AF-V2 要求先有一次
> 真机全量刷写通过」。它现在正按设计挡着，彩排产出的是 PlanAssessment 而非 Executable plan。

**目标**

~~~text
inspect
→ probe
→ plan
→ authorize
→ execute
→ verify
→ reconcile
→ complete-overwrite recovery when eligible
~~~

**生产代码**

- [x] ArkForge durable engine(journal 落盘 + fsync policy 随 record kind 固定；
      撕裂尾部穷举复原；13.3 崩溃处置表由 journal 推导)
- [~] ArkDeck adapter — ArkForge 半已完成(step 映射表 + 控制动作映射表 + ArkDeck
      provider action 归属表 + permit 交叉验证向量)；Swift 半是 ArkDeck 仓变更，
      提案已写好待贴：[docs/openspec/chg-arkdeck-arkforge-authority](docs/openspec/chg-arkdeck-arkforge-authority/proposal.md)
- [x] StepPermit(含 8.6 完整性与重传信任模型；single-use 跨重启由 durable ledger 保证，
      顺序由类型强制：admit → begin_dispatch → record_receipt → checkpoint，逐个 by value 消费)
- [x] ManagedDeviceControlPort(typed 动作 + Provider 侧「这属于 authority」的显式拒绝)
- [x] Rockchip fixed-tool Provider(封闭命令面 `ld`/`ppt`/`wlx`/`rl`/`rd`；
      argv 只在 Provider 内降解；读域三态判定；真机实测见彩排证据)
- [ ] generic Runtime integration — **ArkDeck 仓变更，需拍板**
- [ ] generic UI
- [ ] compatibility alias(flash.dayu200 → generic adapter，不保留 Rockchip lowering)
- [ ] legacy decoder
- [ ] arkforged signing/entitlement/packaging 契约(对齐 #1299 体系与运行时校验器语义，AD-007)

**验收**

- [ ] real DAYU200 full flash pass — **已定：ArkDeck 做 authority**(2026-08-15)。
      除写入外全部已在真机跑通;写入待 ArkDeck 侧 OpenSpec 合入并实现 permit 签发。
      台架/真机的任何一次通过只发布它自己那一个 maturity 组合
- [x] exact identity(两种人格均 `serialAndTopology`；locationID 跨模式变化第二次复现)
      / multi-device — 单板环境，多板未验
- [x] nine partitions/userdata — 九个目标全部降解、前置校验通过、镜像 revalidate 通过(未派发)
- [x] read-domain-aware verification(readback/typed-skip)— 真机实测：1 Verified / 2 Failed /
      6 TypedSkip，读窗边界与 AD-006 相容(AD-019)；+ build postflight — 期望值已从
      真实 `system.img` 提取(`OpenHarmony-7.0.0.36`，AD-016)，比对待写入后执行
- [~] rebind 瞬态容忍 — ✅ 真机实测两个方向的空窗(3,725 ms / 15,579 ms)、
      serial 与 topology 双双变化、窗口内始终唯一匹配(AD-020)；
      normal 别名 — 未验：它是 hdc 的词汇，需走 ManagedDeviceControlPort，那一侧是 authority 的
- [~] crash/cancel/fault — 软件层 ✅(13.3 逐行测试、撕裂尾部穷举、`wlx` 不可中断)；
      真机版需要先有写入
- [~] outcomeUnknown no replay — 软件层 ✅(状态机无回边、四个 disposition 均拒绝重派、
      重启后消费中断的 permit 判 unknown)；真机版需要先有写入
- [~] eligible complete-overwrite recovery — 只读半 ✅(possible effects / reconcile 四态 /
      eligibility 判定，`arkforge-engine::superseding`)；当前所有 Profile 均判为不合格，
      因为没有已发布的覆盖声明。recovery plan 物化需真实写入产生的 effect set
- [ ] ArkDeck production lowering 无 Rockchip command/address

## AF-V3：DAYU600 evidence + plan-only

> **状态(2026-08-14)**：软件半已完成，见[验收证据](docs/evidence/AF-V3-acceptance.md)。
> 证据半未做——`descriptor/transcript capture` 需真实 DAYU600；17.5 十八条门
> 0 条 PASS(第 18 条记为 HELD，见 [ledger](docs/evidence/ledger.md))。

**目标**

~~~text
PAC inspect
→ USB discover/probe(只读证据取得后)
→ profile candidate
→ PlanAssessment
→ evidence requirements
→ start unavailable
~~~

**验收**

- [x] bluetool static evidence 纳入 ledger(AD-004，含「仅静态」边界小节；ledger 可机器检查)
- [x] PAC parser ResearchOnly(结构观测器；confidence 恒为 ResearchOnly，任何输入不可改变)
- [x] exact unknown list(UNI-U01..U12，12 条；parser 与 ledger 双向一致)
- [ ] descriptor/transcript capture — **需真实 DAYU600**；本仓 transcript 为 synthetic
- [x] wrong device tests(6 个方向，含「共享模式名不构成认领」)
- [x] parser fuzz(3600 变异 + 1500 confidence 不变量)
- [x] startExecution 无 bypass(类型/Profile/Maturity/API 四层)
- [x] 未把 plan-only 记为真机刷写通过(provenance 分级 + 十八门 0 PASS 断言)

**生产代码**

- [x] `arkforge-artifact::pac` 结构观测器
- [x] `profiles/dayu600.yaml` 研究 profile(无可写目标)
- [x] `arkforge-provider::unisoc` 仅出 PlanAssessment
- [x] `docs/evidence/ledger.md` 证据账本(AD-001..007 / UNI-U01..U12 / 17.5 十八门)
- [x] daemon 按 profile 声明的 artifact format 分派 provider，DAYU600 走同一 API

## AF-V4：DAYU600 production execute

> **阻塞原因(2026-08-14)**：17.5 的十八条证据门 0 条 PASS。第 1 条(PAC format/version)
> 到第 8 条(每个 destructive step 的断连结果)都需要 PAC 样本、官方工具观察与合法授权的
> USB capture；第 16 条需真实 DAYU600 验收。任一条未过即不得出现 executable planID
> (25.17)。

**前置条件**

architecture.md 17.5 的十八条证据门全部 PASS。

**验收**

- [ ] ArkDeck 代码不增加 DAYU600/Unisoc/PAC/FDL 分支
- [ ] exact target
- [ ] PAC/FDL/partition/effect 完整
- [ ] typed protocol receipt
- [ ] fault/cancel/outcomeUnknown
- [ ] recovery coverage
- [ ] real DAYU600 Golden Journey
- [ ] platform support 仅按实测声明
