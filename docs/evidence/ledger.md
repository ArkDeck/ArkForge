# ArkForge Evidence Ledger

> 正本规则见 architecture.md 24.1。本文件是可机器检查的账本：
> `crates/arkforged/tests/evidence_ledger.rs` 解析下面的表格并断言
> **十八条 DAYU600 证据门无一为 PASS**、**每条 UNI-U 未知都被 PAC parser 携带**、
> 以及**没有任何 plan-only 或 synthetic 记录被记为真机通过**。
>
> 改动本文件而不改动被引用的事实，测试会红。这是刻意的：账本的价值在于它不能
> 被一次乐观的编辑悄悄提升。

## 0. 等级(architecture.md 2.3)

| 等级 | 定义 | 可支持的结论 |
|---|---|---|
| A | 官方规范 / 官方文档 / 官方源码的固定 revision | 架构与公开协议事实 |
| B | 官方实现行为，非稳定规范 | fixed adapter；必须 pin 版本并做 contract test |
| C | ArkDeck accepted spec / Catalog / 固定 SHA 源码 | 迁移兼容与安全要求 |
| D | 社区逆向 / 实验记录 / 第三方工具 | **只**用于提出研究假设 |
| U | 未取得 / 冲突 / 不可复现 | 必须 UNKNOWN / UNAVAILABLE |

## 1. 已确认条目

| ID | 事实 | 来源 | 等级 | 状态 |
|---|---|---|---|---|
| AD-001 | ArkDeck authority / provider / recovery contract | `../../../ArkDeck/openspec/contracts/provider-contracts.md` | C | confirmed |
| AD-002 | Flash complete-overwrite supersession | `../../../ArkDeck/openspec/specs/flashing/spec.md` | C | confirmed |
| AD-003 | DAYU200 published operation / recovery contract | `../../../ArkDeck/Catalog/operations/flash.dayu200.json` | C | confirmed |
| AD-004 | BlueTool 3.3.0 同包含 `CmdDloader.exe`、UNISOC DLL 与 PAC 资源；`param get ohos.boot.hardware` 在 DAYU600 上答 `uis7885`；该路径与 DAYU200/RockUSB 是两条不同的刷机实现 | `../../../ArkDeck/openspec/changes/chg-2026-026-macos-rockchip-flash-ui/bluetool-analysis.md`(本地主机静态分析；未运行 Windows 程序，未连接设备，destructive dispatch = 0) | C/D | confirmed(**仅静态**) |
| AD-005 | 现行 Rockchip Provider / tool / profile 实现 | ArkDeckKit production sources | C | confirmed |
| AD-006 | DAYU200 板端 RockUSB 读写面不对称：`rl` 读面自扇区 65536(32 MiB)起结构性盲区、窗口外恒 uniform 0xCC；擦除介质亦读为 0xCC；`wlx` 写面全盘可达；读窗大小须每次执行实测 | GJ-4 真机 campaign ECAMP-96EFFF15 / ECAMP-31E041BC；PR #1066–#1070；`RockchipRuntimeActionHost.characterizeMediumReadDomain` | C | confirmed(真机定案) |
| AD-007 | macOS Rockchip 组件 entitlement 死锁：运行时校验器要求 app-sandbox+inherit 与打包契约(#1052)要求空 entitlements 互斥，以修改校验器收口 | 2026-08-04 定案；#1299 helper signing 现代化 | C | confirmed；arkforged 打包设计输入 |
| DIG-001 | deterministic CBOR | RFC 8949 §4.2 | A | confirmed(仓内实现对 Appendix A 向量) |
| IPC-001 | Protobuf 演进规则 | protobuf.dev proto3 guide | A | confirmed |

### AD-004 的边界

AD-004 是 DAYU600 唯一的已确认证据，它支持的结论只有一条：**DAYU600 走另一条刷机实现，
应新增独立的 Unisoc Provider 与 DeviceProfile**。它**不**支持：

- PAC 的任何格式细节——分析的是 BlueTool 的 Python 层，不是 PAC 容器；
- 任何 USB 或协议事实——未连接设备；
- 任何 CmdDloader 的行为事实——未运行 Windows 程序。

`profiles/dayu600.yaml` 的 `evidenceRefs` 只写 `AD-004`，与此一致。

## 2. DAYU600 未知清单(UNI-U01..U12)

正本在 `crates/arkforge-artifact/src/pac.rs::DAYU600_EXECUTION_UNKNOWNS`，
PAC parser 把整张表放进它产出的每一份 manifest。

| ID | 未知的事实 | 等级 |
|---|---|---|
| UNI-U01 | PAC 容器格式与版本：无规范、无样本、无授权 capture | U |
| UNI-U02 | PAC 签名与校验和方案 | U |
| UNI-U03 | FDL1/FDL2 身份、载入地址、入口点与阶段顺序 | U |
| UNI-U04 | FDL 安全握手是否存在、要求什么 | U |
| UNI-U05 | Download 模式 USB identity(VID/PID、接口、endpoint) | U |
| UNI-U06 | download 模式下可读的稳定 chip/device 唯一标识 | U |
| UNI-U07 | Download 协议 request/ACK/error/timeout 语义 | U |
| UNI-U08 | 存储几何、分区表表示、erase policy、写序与校验算法 | U |
| UNI-U09 | full restore 对 userdata / 校准 / NV / 安全存储的数据影响 | U |
| UNI-U10 | 取消与恢复语义，含写入是否可原子取消 | U |
| UNI-U11 | macOS / Linux / Windows 的 host driver 需求 | U |
| UNI-U12 | CmdDloader 与 UNISOC 库的许可与再分发条款 | U |

按 24.1，许可未知默认**不可再分发**——UNI-U12 未闭合前，本仓不得携带任何厂商二进制。

## 3. DAYU600 证据门(architecture.md 17.5)

ProductionVerified 前必须全部 PASS。当前 **0/18**。

| # | 门 | 状态 | 阻塞 |
|---:|---|---|---|
| 1 | PAC format / version | MISSING | UNI-U01 |
| 2 | signature / checksum | MISSING | UNI-U02 |
| 3 | FDL identity / address / order / security | MISSING | UNI-U03, UNI-U04 |
| 4 | exact USB identity | MISSING | UNI-U05 |
| 5 | stable chip / device identity | MISSING | UNI-U06 |
| 6 | request / ACK / error / timeout | MISSING | UNI-U07 |
| 7 | storage / erase / write / verify / reboot | MISSING | UNI-U08 |
| 8 | 每个 destructive step 的断连结果 | MISSING | UNI-U07, UNI-U08 |
| 9 | possible effect mapping | MISSING | UNI-U08, UNI-U09 |
| 10 | read-only reconcile | MISSING | UNI-U07, UNI-U08 |
| 11 | complete-overwrite coverage 或明确不支持 | MISSING | UNI-U08, UNI-U10 |
| 12 | driver / platform acceptance | MISSING | UNI-U11 |
| 13 | license / redistribution | MISSING | UNI-U12 |
| 14 | parser fuzz | MISSING | 仓内已有 research parser 的 fuzz(见下)，但该门要求的是**生产 PAC parser** 的 fuzz，而生产 parser 需先有 UNI-U01 |
| 15 | provider / transcript contract | MISSING | 需 captured transcript；现有 DAYU600 transcript 是 synthetic |
| 16 | real DAYU600 acceptance | MISSING | 无硬件 |
| 17 | ArkDeck review | MISSING | 无可 review 的产品能力 |
| 18 | 无 force / experimental bypass | **HELD** | 本仓无 bypass；这是唯一一条「持续保持」而非「取得」的门，见下 |

第 18 条与其他十七条形状不同：它不是要取得的证据，而是要**持续不违反**的性质。
守卫在 `crates/arkforge-provider/src/unisoc.rs`(materialize 无 Executable 分支)与
`crates/arkforged/tests/evidence_ledger.rs`。表中记为 HELD 而非 PASS——PASS 会读作
「这条门过了」，而它永远不会「过」，只会「仍然成立」。

## 4. 本仓 DAYU600 相关产出的证据地位

| 产出 | 地位 | 不可用于 |
|---|---|---|
| `crates/arkforge-artifact/src/pac.rs` | 结构观测器，非 PAC parser | 任何关于 PAC 字段的断言 |
| `transcripts/dayu600-research-synthetic.yaml` | **synthetic**，手写 | 任何协议、USB 或设备事实 |
| `profiles/dayu600.yaml` | 研究 profile，无可写目标 | 任何执行 |
| PAC research fuzz(1500+3600 变异输入) | 覆盖**观测器**的健壮性 | 第 14 条门(那要求生产 parser) |

`TranscriptProvenance::supports_protocol_claims()` 对 `synthetic` 与
`derived-from-published-receipts` 都返回 false;只有 `captured` 返回 true。
这是「不得把 plan-only 记为真机刷写通过」在代码里的落点。

## 5. 账本规则(architecture.md 24.1)

- 外部 URL 必须补固定 revision；指向 master/main 的链接只用于导航；
- ArkDeck 仓内证据以 `../../../ArkDeck/` 相对路径引用；
- evidence bytes / binary / artifact / capture 记录 SHA-256；
- **D / U 不能独立支持 execute**；
- ProductionVerified 必须引用 evidence set；
- evidence 状态变化版本化，不改写历史；
- **许可未知默认不可再分发**；
- **simulation / plan-only 不记 real hardware pass**。
