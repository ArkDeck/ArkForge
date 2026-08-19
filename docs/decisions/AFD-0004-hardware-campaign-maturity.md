# AFD-0004 — HardwareCampaign：让第一次真机验收可达，而不拆掉门

- 状态：Accepted
- 日期：2026-08-17
- 关联：architecture.md 5.5、12.3、22(AF-V2)；AFD-0003；AD-023

## 背景：门是死的，不是严的

`MaturityState::permits_executable_plan()` 原本只承认 `ProductionVerified`。
Rockchip provider 对 DAYU200 发布的是 `HardwareGated`，其 blocker 原文是：

> AF-V2 requires a real DAYU200 full-flash pass through ArkForge before this
> combination can be ProductionVerified

这构成一个环：

~~~text
第一次真机刷写  需要  executable plan
executable plan 需要  ProductionVerified
ProductionVerified 需要  第一次真机刷写
~~~

任何新组合的第一次刷写因此不可达。

## 这不是读出来的，是测出来的

2026-08-17，用 ArkDeck 的真实 IPC 客户端驱动一个按出货配置启动的
`arkforged`(签名的 daemon、pin 住的 `rkdeveloptool`、部署的 dayu200.yaml)：

~~~text
signing: arm64 com.arkdeck.desktop.rkdeveloptool (runtime, team 8AQTYW5FKR, no entitlements)
self-test: rkdeveloptool ver 1.32 in 12 ms
execution: ready

startExecution → PLAN_NOT_STARTABLE: no stored plan flash.dayu200
~~~

daemon 就绪、工具跑得起来、授权已配对，仍然无法执行——因为
`materializePlan` 在 `HardwareGated` 下只会返回 `availability: Unavailable`
的 `PlanAssessment`，store 里永远不会有可启动的 plan。

值得记的是**怎么发现的**：ArkForge 的 `arkforge-rehearse` 按架构 8.6 无法
铸造 StepPermit，所以它能走到 permit 之前的每一步、且只能走到那里；
ArkDeck 的测试全绿也照不到 daemon 的真实回答。两仓各自的测试套件都不可能
发现这个环。**只有让 ArkDeck 驱动一个真 arkforged 才能发现。**

## 决定

新增 `MaturityState::HardwareCampaign { campaign: String }`，
`permits_executable_plan()` 承认它。

考虑过并否决的替代方案：把 `HardwareGated` 直接纳入
`permits_executable_plan()`。那不是破环，是删门——之后任何未测量的组合都能
执行写入，5.5 的约束整体失效。

## 三条约束，缺一不可

**一、必须具名开启。** `arkforged --hardware-campaign <id>`。缺省仍是
`HardwareGated`。缺省若能产生 campaign，这道门对没读过本文的操作员就不存在。
空标识符被拒绝：无名的验收活动没人能对结果负责。

**二、transcript replay 永不适用。** `ToolchainKind::Replay` 无论传什么都保持
`PlanOnly`，AF-V1 的理由原样成立——录像不是设备。对录像做验收活动，会产出
一份关于录像的证据却挂着板子的名字。

**三、进封印，不只进结构体。** `maturity` 加入 `PlanSealInput` 与
`plan_digest_body`，`MaturityState` 的 CBOR 增加 `campaign` 键。

第三条是这个设计的支点。StepPermit 只绑 `plan digest`，不绑别的。如果
campaign 计划与 production 计划能得到相同摘要，一次验收活动产生的 permit 与
回执就与生产运行的完全无法区分，「验收时跑通过一次」会变成「该组合受支持」，
而证据链里没有任何东西能反驳。摘要不同，这条路就被封死了。

`is_production_evidence()` 只对 `ProductionVerified` 为真：campaign 的写入是
真的，它的支持声明不是。这与 12.3、24.1 一贯的态度一致——不做超出证据的声明。

## 后果

- 新组合的第一次刷写可达，且在日志、plan 摘要、回执三处都标着是验收活动；
- `Service::new` 增加 `campaign: Option<&str>` 参数，无缺省值：它决定该 daemon
  能否执行 DAYU200 写入，藏进缺省值就是让没人做过的决定生效；
- plan 摘要因封印新增字段而整体改变。仓内无任何钉死的 plan 摘要，
  ArkDeck 侧亦无，故无迁移成本；
- `ProductionVerified` 仍无任何生产代码发布点。把一次成功的 campaign 提升为
  `ProductionVerified` 是**另一个决定**，需要 evidence set 支撑(2225 行)，
  不在本决定范围内。

## NRU-004 后续状态（2026-08-19）

本决定记录的是 vendor 迁移阶段如何打开第一次真机验收路径。ArkForge 当前只
保留原生 RockUSB transport；上文的 vendor 二进制、命令与彩排工具均为历史
观测，不是现行 runtime surface。
