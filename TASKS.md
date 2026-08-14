# ArkForge 实施任务台账

> 状态总览(2026-08-14)：四个垂直任务全部未开工。任务定义正本是 [docs/architecture.md](docs/architecture.md) 第 22 节；本文件是执行台账，若两处出入，以 architecture.md 为准并回改本文件。

| 任务 | 内容 | 依赖 | 状态 |
|---|---|---|---|
| AF-V1 | ArkForge Core + DAYU200 read-only parity | — | ⬜ 未开工 |
| AF-V2 | DAYU200 ArkForge production cutover | AF-V1 | ⬜ 未开工 |
| AF-V3 | DAYU600 evidence + plan-only | AF-V1(共用 API 面) | ⬜ 未开工 |
| AF-V4 | DAYU600 production execute | AF-V3 + AF-V2(engine) + 17.5 证据门全 PASS | ⬜ 未开工 |

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

- [ ] Rust workspace(八个边界 crate，见 architecture.md 4.2)
- [ ] neutral Authority API
- [ ] Artifact/CAS
- [ ] DAYU200 parser/profile
- [ ] Rockchip read-only probe
- [ ] PlanAssessment/FlashPlan
- [ ] deterministic digest(RFC 8949 CBOR + SHA-256)
- [ ] projection validator
- [ ] daemon read-only API
- [ ] golden transcript 库(GJ-4 campaign receipts ECAMP-96EFFF15 / ECAMP-31E041BC 为种子)

**验收**

- [ ] Core 不依赖 ArkDeck/vendor
- [ ] current DAYU200 archive facts parity
- [ ] unknown member/partition fail closed
- [ ] private action digest 覆盖
- [ ] startExecution disabled
- [ ] unit/fuzz/transcript tests
- [ ] Profile 含 readDomain 与 per-target 验证强度，与 AD-006 一致
- [ ] DAYU200 整包 CAS 导入在声明预算内(architecture.md 10.2)
- [ ] 无设备 mutation

## AF-V2：DAYU200 ArkForge production cutover

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
