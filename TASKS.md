# ArkForge 实施任务台账

> 状态总览(2026-08-14)：AF-V1 完成。任务定义正本是 [docs/architecture.md](docs/architecture.md) 第 22 节；本文件是执行台账，若两处出入，以 architecture.md 为准并回改本文件。

| 任务 | 内容 | 依赖 | 状态 |
|---|---|---|---|
| AF-V1 | ArkForge Core + DAYU200 read-only parity | — | ✅ 完成([验收证据](docs/evidence/AF-V1-acceptance.md)) |
| AF-V2 | DAYU200 ArkForge production cutover | AF-V1 | ⛔ 阻塞：需真实 DAYU200 硬件 + ArkDeck 仓变更 |
| AF-V3 | DAYU600 evidence + plan-only | AF-V1(共用 API 面) | ⬜ 未开工(软件半可做，证据半需真机) |
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
- [x] current DAYU200 archive facts parity(结构等价；厂商归档字节级比对待归档到位)
- [x] unknown member/partition fail closed
- [x] private action digest 覆盖
- [x] startExecution disabled(类型层 / 服务层 / 线上 UDS 三层)
- [x] unit/fuzz/transcript tests(277 tests + 18000 变异输入)
- [x] Profile 含 readDomain 与 per-target 验证强度，与 AD-006 一致
- [x] DAYU200 整包 CAS 导入在声明预算内(实测 3.07 s / 227 MiB/s，预算 60 s)
- [x] 无设备 mutation

**额外交付**：`adapters/arkforge-arkdeck-adapter` 的 published step 映射表(5.4)、
`proto/arkforge.proto`、`arkforge-cli` 只读诊断。

**边界**：见[验收证据](docs/evidence/AF-V1-acceptance.md)第 4 节。

## AF-V2：DAYU200 ArkForge production cutover

> **阻塞原因(2026-08-14)**：验收首条即 `real DAYU200 full flash pass`，本环境无该硬件；
> 且 `ArkDeck adapter` / `generic Runtime integration` / `generic UI` /
> `compatibility alias` 属 ArkDeck 仓变更，按 7.4 必须走 OpenSpec + 维护者 PR review
> 并与真实产品能力同车交付。在硬件与该仓授权到位前不应开工——先写代码再补真机，
> 正是 21.1「migration cutover 必须真实 DAYU200 pass」要防的顺序。

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

- [ ] ArkForge durable engine
- [ ] ArkDeck adapter
- [ ] StepPermit(含 8.6 完整性与重传信任模型)
- [ ] ManagedDeviceControlPort
- [ ] Rockchip fixed-tool Provider
- [ ] generic Runtime integration
- [ ] generic UI
- [ ] compatibility alias(flash.dayu200 → generic adapter，不保留 Rockchip lowering)
- [ ] legacy decoder
- [ ] arkforged signing/entitlement/packaging 契约(对齐 #1299 体系与运行时校验器语义，AD-007)

**验收**

- [ ] real DAYU200 full flash pass
- [ ] exact identity/multi-device
- [ ] nine partitions/userdata
- [ ] read-domain-aware verification(readback/typed-skip)+ build postflight
- [ ] rebind 瞬态容忍与 normal 别名真机复验
- [ ] crash/cancel/fault
- [ ] outcomeUnknown no replay
- [ ] eligible complete-overwrite recovery
- [ ] ArkDeck production lowering 无 Rockchip command/address

## AF-V3：DAYU600 evidence + plan-only

> **可做/不可做拆分(2026-08-14)**：软件半可做——PAC ResearchOnly parser、
> exact unknown list、parser fuzz、DAYU600 Profile(execute unavailable)、
> startExecution 无 bypass、bluetool 静态证据入 ledger。
> 证据半不可做——`descriptor/transcript capture` 需真实 DAYU600；
> 且无 PAC 样本与格式规范(UNI-U01 = missing)时，parser 只能做通用结构扫描，
> 这本就是 10.5 对 ResearchOnly 的定义。

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

- [ ] bluetool static evidence 纳入 ledger
- [ ] PAC parser ResearchOnly
- [ ] exact unknown list
- [ ] descriptor/transcript capture
- [ ] wrong device tests
- [ ] parser fuzz
- [ ] startExecution 无 bypass
- [ ] 未把 plan-only 记为真机刷写通过

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
