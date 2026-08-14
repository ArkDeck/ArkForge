# AF-V3 验收证据(软件半)

> 日期：2026-08-14
>
> 范围：architecture.md 22 `AF-V3：DAYU600 evidence + plan-only` 中**不需要真实
> DAYU600 硬件**的部分。需要硬件的部分明列在第 3 节，未做，也未记为完成。
>
> 复现：`cargo test --workspace --offline`(321 tests，全绿)。

## 1. 目标链路

```text
PAC inspect → USB discover/probe → profile candidate → PlanAssessment
  → evidence requirements → start unavailable
```

全链路通过**与 DAYU200 相同的 API** 跑通：
`crates/arkforged/tests/dayu600_api_surface.rs::the_dayu600_research_vertical_runs_over_the_same_api`
依次调用 `importArtifact` → `inspectArtifact` → `discoverDevices` → `probeDevice`
→ `materializePlan`，与 DAYU200 用例调用的是同一组 API、同一个 daemon、同一份编解码器。
Provider 与 parser 的选择由 **profile 声明的 artifact format** 与**容器自身的 framing**
决定，daemon 不判断某台设备属于哪个厂商。

## 2. 验收逐条

### 2.1 bluetool static evidence 纳入 ledger

[`docs/evidence/ledger.md`](ledger.md) 的 AD-004 条目记录了 BlueTool 3.3.0 同包
含 `CmdDloader.exe`、UNISOC DLL 与 PAC 资源，以及 `param get ohos.boot.hardware`
在 DAYU600 上答 `uis7885`，并标注 **C/D、仅静态**。

关键在于随附的「AD-004 的边界」小节：它明写这条证据**只**支持「DAYU600 走另一条
刷机实现，应新增独立 Unisoc Provider 与 DeviceProfile」这一个结论，**不**支持 PAC
格式细节、USB 事实或 CmdDloader 行为事实——因为原分析没有运行 Windows 程序、
没有连接设备。`profiles/dayu600.yaml` 的 `evidenceRefs` 因此只写 `AD-004`，
并由 `evidence_ledger.rs::the_dayu600_profile_references_only_evidence_the_ledger_confirms`
断言。

ledger 是**可机器检查**的：`crates/arkforged/tests/evidence_ledger.rs` 解析它并断言
AD-004 记录了 static 限定、未知清单与 parser 一致、以及第 5 节规则原文在位。

### 2.2 PAC parser ResearchOnly

`crates/arkforge-artifact/src/pac.rs`。它**不是 PAC parser**，是结构观测器——
因为 UNI-U01 成立：本项目没有 PAC 规范、样本或授权 capture。

它记录的是无需规范即可看见的东西，每条都附**产生它的规则**：

| 观测 | 规则 |
|---|---|
| `AsciiStringRun` / `Utf16LeStringRun` | 连续可打印字节 ≥ 4；UTF-16LE 在两种字节对齐上各扫一遍 |
| `HighEntropyRegion` / `LowEntropyRegion` | 块级 Shannon 熵对 7.5 bits/byte 启发式分类 |
| `RepeatingStride` | 某字节在每 S 字节偏移上重复 ≥ 8 次 |
| `UniformFill` | 单一字节值连续 ≥ 64 |

`CandidateKind` 里**没有** `PacHeader` / `FdlImage` / `PartitionEntry` 这类变体——
命名它们就是一次本项目没有证据的解释。`RepeatingStride` 的 basis 字符串明写
「consistent with a record table and equally consistent with padding — this is a
hypothesis, not a table」，并由测试断言这句话在位。

强制性质：

- `confidence` 恒为 `ResearchOnly`，任何输入都无法改变
  (`no_input_can_produce_a_production_manifest`，5 组差异极大的输入)；
- `partition_table` 恒为 `None` 而非 `Some(empty)`——后者会读作「容器声明了零个分区」，
  那是一个断言，而 `None` 读作「本 parser 说不出来」；
- 候选数量按 kind 封顶 256，截断被显式上报(`truncated_kinds`)，否则空尾部会读作
  「后面没有了」。

### 2.3 exact unknown list

`DAYU600_EXECUTION_UNKNOWNS`：UNI-U01..UNI-U12，共 **12 条**，逐条对应 17.1
「仍未知」列表的一个事实(格式/版本、签名校验、FDL 地址与顺序、安全握手、
USB identity、稳定芯片标识、协议 request/ACK/error/timeout、存储与分区语义、
数据影响、取消与恢复、driver、许可)。

- parser 把整张表放进**每一份** manifest(`the_unknown_list_is_complete_and_carried_into_every_manifest`)；
- ledger 第 2 节与之逐条一致，由 `every_unknown_in_the_ledger_is_carried_by_the_parser`
  双向断言(ledger 多一条或 parser 少一条都红)；
- 每条未知在 assessment 里生成一条 `EvidenceRequirement`，`minimum_grade = 'A'`——
  按 17.4，parser 观测、官方工具行为与真机事实须三方一致，D 级社区逆向不能独立闭合任何一条。

### 2.4 wrong device tests

`crates/arkforge-provider/tests/dayu600_vertical.rs` 与
`crates/arkforged/tests/dayu600_api_surface.rs`：

| 场景 | 结果 |
|---|---|
| Unisoc provider + DAYU200 设备观测(共享 `hdc-normal` 模式名) | 不因模式名相同而认领；probe 明写 `identityConfirmation = unconfirmed`(UNI-U05/U06) |
| Unisoc provider + `rockusb-loader` 观测 | 拒绝，错误信息含「will not guess」 |
| Rockchip provider + PAC 容器 | `RK-V01` 违规 |
| Unisoc provider + Rockchip 归档 | `UNI-V01` + `UNI-V02` 违规 |
| Rockchip provider + Rockchip 归档 + **DAYU600 profile** | `RK-V02` 违规——按事实拒绝，不是按名字 |
| API 层：PAC 容器 + DAYU200 profile | 不产生任何 executable plan |

### 2.5 parser fuzz

`crates/arkforge-artifact/tests/parser_fuzz.rs` 新增两个目标：

- `arbitrary_containers_never_panic_the_pac_research_parser`：4 组语料 × 900 变异 = 3600 输入；
- `no_mutated_container_upgrades_the_parser_confidence`：1500 变异，断言任何变异都不能
  把 confidence 抬出 ResearchOnly、也不能丢掉任何一条未知。

性质与 DAYU200 一致：不 panic、不挂起、不无界分配。

### 2.6 startExecution 无 bypass

四层，各自独立：

1. **类型层**：`UnisocProvider::materialize` 全函数没有构造
   `PlanMaterialization::Executable` 的分支——不存在可达的代码，不是被 flag 关掉的代码；
2. **Profile 层**：`profiles/dayu600.yaml` 的 `allowedTargets` 为空、`dataImpact` 四轴全 unknown、
   `logicalBlockSize` 与 `erasedMediumFiller` 为 `unknown`、`modeTransitions` 为空；
   `execution_blockers()` 报出 PROF-B01..B06 六条；
3. **Maturity 层**：DAYU600 组合恒为 `ResearchOnly`，`permits_executable_plan()` 为 false；
4. **API 层**：`start_execution_is_unavailable_for_dayu600_at_every_layer` 用三种 payload
   形状(空、只带 plan id、带 artifact+digest+purpose)调用 `startExecution`，全部
   `UNAVAILABLE / EXECUTION_DISABLED`。

补充一条结构性事实：assessment **没有字段**可以承载 planID，而 `startExecution`
只接受 planID——所以研究路径与执行路径之间不存在可以走通的接口。

### 2.7 未把 plan-only 记为真机刷写通过

- `transcripts/dayu600-research-synthetic.yaml` 的 provenance 是 `synthetic`，
  文件头明列它**不可**用于 UNI-U01/U05/U06/U07 的任何一条；
- `TranscriptProvenance::supports_protocol_claims()` 对 `synthetic` 与
  `derived-from-published-receipts` 都返回 false，只有 `captured` 返回 true；
- ledger 第 4 节逐项列出本仓 DAYU600 产出的证据地位与「不可用于」；
- ledger 第 3 节的十八条门 **0 条 PASS**，由 `no_dayu600_evidence_gate_is_pass` 断言。
  第 18 条(无 force/experimental bypass)记为 **HELD** 而非 PASS——它不是要取得的证据，
  而是要持续不违反的性质，记成 PASS 会读作「这条完成了」。

## 3. 未做的部分(需要真实 DAYU600)

| AF-V3 验收项 | 状态 | 原因 |
|---|---|---|
| descriptor / transcript capture | **未做** | 需真实 DAYU600。本仓的 DAYU600 transcript 是 synthetic，且代码层面不可被当作 capture |
| 17.5 第 1–13、16 条门 | **未取得** | 需 PAC 样本、官方工具观察与合法授权的 USB capture；第 16 条需真机验收 |
| 17.5 第 14 条(parser fuzz) | **未取得** | 该门要求的是**生产 PAC parser** 的 fuzz；生产 parser 需先闭合 UNI-U01。本仓 fuzz 覆盖的是研究观测器 |
| 17.5 第 15 条(provider/transcript contract) | **未取得** | 需 captured transcript |
| 17.5 第 17 条(ArkDeck review) | **未做** | 无可 review 的产品能力；且属 ArkDeck 仓 |
| ArkDeck 侧同一 UI/API 展示(21.2 Stage C) | **未做** | 属 ArkDeck 仓变更，按 7.4 须 OpenSpec + 维护者 review |

另有一处必须说清的边界：本仓所有 PAC 相关测试使用的是**合成容器**，不是 PAC 文件。
它按「文本头 + 记录状步长 + 高熵载荷 + 填充」构造，目的是让观测规则有东西可看，
**不主张它像 PAC**。UNI-U12 未闭合前，本仓也不得携带任何厂商二进制。

## 4. 对 AF-V1 的回改

为承载「一个未被测量过的设备」，profile schema 分离了两件事：

- `DeviceProfile::validate()` 仍然拒绝**错误**(未知 schema 版本、零块大小、通配 revision、
  allowed∩protected 非空、写序不连续、别名冲突)；
- `DeviceProfile::execution_blockers()` 报告**缺失的事实**(数据影响未知、块大小未测、
  filler 未测、无已测硬件版本、无可写目标、无模式转换)。

理由：未知的数据影响不是 schema 错误，是一条待取得的证据。原先它在加载期就报错，
DAYU600 profile 将无法存在，而「无法表达一个未知设备」会把项目推向「先填个 512 再说」——
恰是 24.1 要防的那种数字。执行侧未被放松：`EffectSet::validate_executable()` 仍然拒绝
未知数据影响，plan 仍然封不出来。

同时 `storage.logicalBlockSize` 与 `readDomain.erasedMediumFiller` 现在接受字面量
`unknown`。DAYU200 profile 的取值未变，其 profile digest 因此不变。
