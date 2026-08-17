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
| AD-007 | macOS Rockchip 组件 entitlement 死锁：运行时校验器要求 app-sandbox+inherit 与打包契约(#1052)要求空 entitlements 互斥，以修改校验器收口 | 2026-08-04 定案；#1299 helper signing 现代化；[AFD-0003](../decisions/AFD-0003-arkforged-signing-packaging.md) | C + A(2026-08-16 本机实测) | **resolved** — 留白已对齐：`arkforged` 与捆绑工具的 entitlement 字典**都为空**，判定「为空」而非 denylist(与 ArkDeck 现行校验器语义一字不差)，由 `crates/arkforged/src/packaging.rs` 在绑定工具**之前**读 Mach-O 代码签名强制;fat 的每个 slice 都判。App Sandbox 键在 development 与 release 两个模式下都拒——不存在「这个构建里 sandbox 子进程能跑」的情况 |
| AD-008 | DAYU200 USB 身份实测：HDC-normal = `0x2207:0x5000`("HDC Device")，Loader = `0x2207:0x350a`("USB download gadget"，`ld` 报 `Mode=Loader`) | [2026-08-14 只读取证](runs/2026-08-14-dayu200-read-only-capture.md) §1、§3 | A(真机实测) | confirmed |
| AD-009 | DAYU200 跨 enter-loader 转换 **serial 与 locationID 双双变化**(loader 挂在另一 hub 之后，USB3→USB2)；`rl` 读窗边界实测落在扇区 65536，窗口外恒 `0xCC`——且读取时板子正由 `system`/`vendor` 启动运行 7.0.0.37，故窗口外 `0xCC` 现场证明不等于「未写入」 | [同上](runs/2026-08-14-dayu200-read-only-capture.md) §3.2、§5 | A(真机实测) | confirmed；AD-006 的独立复现 |
| AD-010 | rkdeveloptool pin 解析：ArkDeck 有**两个**有意不同的 pin，同一 upstream commit `304f0737…`、两个本地构建、两种 access policy——`pinnedReadOnlyDiscovery` = `bbd7bdc0…9923`(只读 `ld` 发现)、`pinnedProduction` = `038a8a0e…3611`(破坏性 flash)。后者实测逐字节命中 `~/dayu200-rehearsal/rkdeveloptool/rkdeveloptool` | [2026-08-14 只读取证](runs/2026-08-14-dayu200-read-only-capture.md) §6；`RockchipDeviceDiscovery.swift:9-28` | A(本机实测) | **resolved** — 无「签名前/后」歧义，pin 指向确定的本地构建 |
| AD-011 | `/opt/homebrew/bin/rkdeveloptool`(哈希 = `pinnedReadOnlyDiscovery` `bbd7bdc0…`)挂起的**已证原因**：该文件带 `com.apple.quarantine`，dyld 在 Gatekeeper 评估处阻塞，栈全部停在 `_dyld_start`，**从未进入 `main`**(故 `-v` 亦挂)。在副本上清除该 xattr 后同哈希立即正常运行 | [同上](runs/2026-08-14-dayu200-read-only-capture.md) §6.2 | A(本机实测，含栈采样与对照实验) | **resolved(本机已修复)** — pin 的代码注释本就写明它是「clean, **non-quarantined** build」；经用户授权删除该 xattr 后,字节与 mtime 均未变,只读发现恢复为冷启 0.25 s／热态 0.03 s,远在 profile 的 `timeout: 5` 内 |
| AD-012 | ArkDeck.app 捆绑的 rkdeveloptool 不是「第三个来路不明的构建」，而是 ArkDeck **自己的密闭可复现构建**产物(recipe `rockchip-component-build@1.0.0`)；身份由 CHG-2026-036 的 `package-receipt.json` 逐包声明 `component.signedSHA256` / `unsignedSHA256` 成对记录 | [同上](runs/2026-08-14-dayu200-read-only-capture.md) §6.2；`openspec/integrations/rockchip/bundled-component/1.0.0/recipe.json` | A(本机实测 + 仓内 recipe) | **resolved** — 非缺陷 |
| AD-013 | `rkdeveloptool ld` 对处于 **HDC-normal** 的 DAYU200 报告 `Mode=Maskrom`(PID `0x5000`)——连续三次复现 | [同上](runs/2026-08-14-dayu200-read-only-capture.md) §6.1 | A(本机实测) | **resolved(已知)** — ArkDeck 早已把这一行固化为夹具 `maskrom.stdout.bin`(`Pid=0x5000…Maskrom`，与本次实测同形)，`providerPreflightDisposition` 先判 VID/PID 再判 mode，契约测试覆盖 |
| AD-014 | ArkDeck 自建 rkdeveloptool 的密闭可复现 recipe：上游 tarball 钉 commit `304f0737…`(archive `389ba41a…`、tree `9908d5bd…`、`upstreamSourceModifications: none`)；**静态链入** GPG 验签的 libusb 1.0.30；`homebrewBuildPaths: denied`、`callerPATH: ignored`、`networkAfterFetch: denied`、`SOURCE_DATE_EPOCH` 固定；双 builder 字节一致；产物直接依赖仅七个系统库(实测 `otool -L` 与 `directDependencyAllowlist` 逐项吻合) | `openspec/integrations/rockchip/bundled-component/1.0.0/recipe.json`；`.github/workflows/rockchip-component.yml` | C(仓内 accepted) + A(本机 `otool -L` 实测) | confirmed；arkforged 打包设计输入 |
| AD-015 | **工具哈希相等不等于工具可用。** 同一份字节(`bbd7bdc0…`)带 quarantine 时挂死在 dyld、清除后正常;ArkDeck 亦有同形教训「binary hash equality does not authorize silently ignoring the source-provenance check」(2026-07-24 source-drift 记录) | [同上](runs/2026-08-14-dayu200-read-only-capture.md) §6.2；`chg-2026-026/.../blocked-capability-preflight-rkdeveloptool-source-drift-2026-07-24.md` | A/C | confirmed；见下「对 ArkForge 的后果」 |
| AD-016 | **构建事实不在系统镜像开头。** 真实 `system.img`(2 GiB)里 `const.ohos.fullname=` 位于第 320,790,684 字节，`const.product.model=`/`const.product.name=` 位于 320,762,067/320,762,092；且 `const.product.name` 的值是**带引号含空格**的 `"OpenHarmony 3.2"`。归档文件名写 7.0.0.35，镜像里写的是 `OpenHarmony-7.0.0.36`——与 ArkDeck 2026-08-04 在刷好的板子上实测到的答案一致 | [2026-08-15 彩排](runs/2026-08-15-dayu200-flash-rehearsal.md)；`crates/arkforge-artifact/tests/real_archive_parity.rs` | A(本机实测) | confirmed；此前 64 MiB 扫描上界与「按空格截断值」两处均为我的臆测，已按实测改正 |
| AD-017 | **本仓的 fsync 只证明到进程死亡为止。** journal 对 dispatch 相关记录在返回前 `sync_all()`，且 `every_torn_tail_replays_as_a_prefix_or_is_refused` 穷举了每一个可能的撕裂位点;但 macOS `fsync(2)` 不冲刷盘内缓存(那要 `F_FULLFSYNC`)，而 `F_FULLFSYNC` 需要 libc，AFD-0001 不允许 | `crates/arkforge-engine/src/durable.rs` 模块文档;architecture.md 13.2.1 | A(仓内)/D(未做掉电实验) | **open** — 记为已知边界，不记为已通过的门 |
| AD-018 | **`rkdeveloptool ppt` 的真实输出是三列、CRLF、裸十六进制、无 size 列**(`00  00002000  uboot`)。设备的表只声明每个分区的**起点**;可写入的上界只能由「到下一个分区起点的距离」推出，而它与归档声明的大小并不相同(`chip_ckm` 归档 131072 扇区，到下一分区 13017088 扇区) | [同上](runs/2026-08-15-dayu200-flash-rehearsal.md) §3 | A(本机实测) | confirmed；我按文档写的四列带 `0x` 解析器在真机上零行命中，已按实测重写 |
| AD-019 | **AD-006 的读窗被独立复现。** 与 AF-V1 capture 完全不同的代码路径实测：sector 1 读到真实数据、sector 19955712 读到 uniform `0xCC`;九个目标的三态判定中 `uboot`(8192)Verified、`resource`(28672)/`boot_linux`(40960)Failed(读到真实内容)、`ramdisk`(237568)起全部 TypedSkip。边界落在 40960 与 237568 之间，与 AD-006 记录的 65536 相容 | [同上](runs/2026-08-15-dayu200-flash-rehearsal.md) §4 | A(本机实测) | confirmed |
| AD-020 | **模式切换的空窗与身份变化实测。** normal→loader 空窗 3,725 ms;loader→normal 空窗 **15,579 ms**——任何短于此的 reconnect deadline 都会误判「设备没回来」。两个方向的 **serial digest 与 topology digest 都变**，AD-008 之后声明的 `serialPolicy`/`topologyPolicy: may-change` 两条各被独立复现一次。整个窗口内任一次采样都只匹配到一台设备 | [同上](runs/2026-08-15-dayu200-flash-rehearsal.md) §7bis | A(本机实测) | confirmed;`normal` 别名**未**在此路径验证(它是 hdc 的词汇，不是 ioreg 的) |
| AD-021 | **「配对了」不等于「能执行」。** 执行就绪是两个常驻事实：authority 已配对 **且** fixed tool 已绑定。只看配对会让 job 走到第一个 dispatch、花掉一张 permit、再停下——那是要 reconcile 的状态，不是「没启动」。现改为 `startExecution` 在解析 payload 之前按常驻事实拒绝，并一次报全部缺失项;工具摘要由 `--rkdeveloptool-sha256` **强制**比对，不符拒绝启动;计划的 toolchain 摘要不符拒为 `TOOLCHAIN_DIGEST_MISMATCH`(摘要是 maturity 组合键的一部分) | `crates/arkforge-engine/src/lib.rs` `ExecutionReadiness`;`crates/arkforged/src/main.rs`;四条启动路径实测 | A(本机实测) | confirmed;字节相等不证明工具能跑这一条已由 AD-022 闭合 |
| AD-022 | **AD-015 的启动自检。** 绑定的工具必须先通过摘要比对，再**证明它能跑**：以 device-free 的 `-v` 探测，带 5 秒超时。2026-08-15 复现并诊断了完整的 AD-015 情形——同一份字节(`bbd7bdc0…`)带 quarantine 时探测 5,009 ms 未返回、被杀掉并拒绝启动，错误里直接给出 `xattr -d com.apple.quarantine <path>`;清除 quarantine 后**摘要一字不差**、自检 207 ms 通过、`execution: ready`。另实测到 quarantine 的**第二种形态**:并非总是挂死，也可能立即退出且无任何输出——两种形态都被拒，且都附带 quarantine 证据(`/usr/bin/xattr` 尽力查询，查不到就明说查不到) | `crates/arkforged/src/dispatch.rs` `HostFixedToolPort::self_test`;四条真实启动路径 | A(本机实测) | confirmed |
| AD-023 | **ArkDeck 现在钉给破坏性 flash 的那份工具不可出厂。** `pinnedProduction` = `038a8a0e…3611`(= 2026-08-15 彩排跑的那份)的动态库闭包里有 `/opt/homebrew/opt/libusb/lib/libusb-1.0.0.dylib`——一个只有这台机器上才有的路径。ArkDeck.app 内捆绑的密闭构建 `231a05ef…c79e` 把 libusb 静态链入(AD-014 的 recipe)，闭包只剩七个系统库，且 Developer ID + Hardened Runtime + 空 entitlement 字典俱全，是三份 `rkdeveloptool` 里**唯一**满足 AFD-0003 release 条款的一份。这与 ArkDeck ADR-0003 自己写下的迁移缺口一致(「pinned discovery registry 继续描述当前 user-selected E0 路径，直到另一个 change 引入 bundled component registry 并显式迁移其消费者」)，因此不记为 ArkDeck 缺陷 | 本机 `otool -L` 三份逐一实测;`packaging/macos/package-arkforged.sh` 首次运行即拒绝彩排那份;`RockchipDeviceDiscovery.swift:21-28`、`RockchipFlashExecutionHost.swift:872/1154/1179/1356` | A(本机实测) | **open** — 对 AF-V2 的后果见下「AD-023 对 toolchain 摘要的后果」 |
| AD-024 | **ArkDeck 基线复核(architecture.md 2.1 要求的那次)。** 审计基线 `2849c5c1` 到 ArkDeck main `b5f5a52b` 共 10 个 commit。结论：(a) chg-2026-059 要拆的六个文件**一个都没动**;(b) `RuntimeJobEngine` 里两处 `flash.dayu200` 特判消失,均为**泛化而非删除**(artifact store 必需性收进 `requiresArtifactStore(reference:)`;「mapped step 无 store」由只对 flash 抛错改为对每个 mapped step 抛错,严格更 fail-closed);(c) `RuntimeJobEngine` 新增取消安全边界模型(`.drained` / `.unconfirmed` / process-group drain proof)——**这条要改设计**:dispatch 移出 ArkDeck 进程后那个进程组不再属于它,`CANCEL_NOT_SAFE` 既不是 drained 也不是 unconfirmed,而是取消**被拒绝** | ArkDeck `git log 2849c5c1..b5f5a52b`;`RuntimeJobEngine.swift:437-444`;chg-2026-059 `design.md` 第 9 节与 6.3.1 | A(仓外实测) | confirmed;复核通过,设计已按 (c) 补 6.3.1 与 `AFA-AC-10` |
| AD-025 | **成熟度环：门是死的，不是严的。** `permits_executable_plan()` 原只承认 `ProductionVerified`，而 Rockchip provider 对 DAYU200 发布 `HardwareGated`，其 blocker 自称「becomes ProductionVerified only after a real DAYU200 full-flash pass」——第一次刷写需要 executable plan，executable plan 需要 ProductionVerified，ProductionVerified 需要第一次刷写。任何新组合的首刷不可达。**发现方式本身是结论的一部分**：ArkForge 的 `arkforge-rehearse` 按 8.6 无法铸造 StepPermit，只能走到 permit 之前;ArkDeck 单仓测试全绿也照不到 daemon 的真实回答;两仓各自的测试套件都不可能发现这个环，只有让 ArkDeck 驱动一个真 `arkforged` 才能发现。解法是 `HardwareCampaign{campaign}`(具名开启、replay 永不适用、进 plan 封印)，**不是**把 `HardwareGated` 纳入 `permits_executable_plan()`——那是删门而非破环 | 本机实测：出货配置启动的 `arkforged` 报 `execution: ready` 且 `self-test: rkdeveloptool ver 1.32`，ArkDeck 真实 IPC 客户端 `startExecution` 得 `PLAN_NOT_STARTABLE: no stored plan flash.dayu200`;`ArkForgeLiveDaemonContractTests`(ArkDeck);`crates/arkforge-provider/tests/hardware_campaign.rs` | A(本机实测) | resolved — 见 `AFD-0004`;`ProductionVerified` 仍无生产发布点，把一次成功 campaign 提升为 ProductionVerified 是另一个决定，需 evidence set 支撑 |
| AD-026 | **ArkDeck 从未把 plan 放进 arkforged 的 store。** `RuntimeJobEngine.dispatchThroughArkForge` 发的是 `planID: runtime.record.request.operation`(ArkDeck 自己的操作名)与 `planSHA256: materializedPlanDigest`(ArkDeck 自己物化的摘要)，而 `arkforged` 从**它自己的** content store 解析 plan。chg-2026-059 design §5.2 第 1 步写明「先 materializePlan 拿 plan_id 与 digest」，该步从未实现：`ArkForgeDaemonClient` 既无 `importArtifact` 也无 `materializePlan`，尽管 `ArkForgeApi` 两个码都已编 | 本机实测(同上 `PLAN_NOT_STARTABLE`);ArkDeck `RuntimeJobEngine.swift:2576-2584`、`ArkForgeDaemonClient.swift`(方法清单) | A(本机实测) | **open** — 与 AD-025 叠加：补齐这两个调用是必要而不充分，无 campaign 时 `materializePlan` 仍只返回 `Unavailable` 的 assessment |
| AD-027 | **daemon 看不见真实设备。** `Service.transports` 只装 `TranscriptTransport`；`UsbTransport::with_ioreg` 早就存在，却只被 `arkforge-capture` 与 `arkforge-rehearse` 两个独立二进制使用，从未接进 `arkforged`。后果不止 `discoverDevices` 返回空：`materializePlan` 在 probe 前要先按 `observation_id` 匹配一条观测，因此**永远到不了真实硬件**；而 transcript 观测走 `ToolchainKind::Replay` → `PlanOnly`，也不可能产出 executable plan。这条与 AD-025、AD-026 叠加：三者全修好之前，真机刷写在软件侧不可达 | 本机实测：板子插着(`ioreg` 列出 `"HDC Device"`)，`arkforge-cli discover` 答 `no devices observed`；接线后同一命令答 `USB-2207-5000-01200000 mode=hdc-normal identity=serialAndTopology`。源级守卫 `architecture_guard::the_daemon_can_observe_a_real_device`，已用「拿掉接线且仍可编译」的方式验证它会红 | A(本机实测) | resolved |
| AD-028 | **工具链身份有两个真相来源。** `fixed_tool_identity()` 里硬编码 `038a8a0e…`(AD-023 判定不可出厂的那份)，而 daemon 绑定的是 `--rkdeveloptool-sha256` 给的那份。maturity 与 plan 都按常量发布/物化，于是每一个物化出的 plan 在 `startExecution` 必被拒:`TOOLCHAIN_DIGEST_MISMATCH`。**拒绝是对的**——toolchain digest 是 maturity 组合的一部分(architecture.md 12.3)，那个配对从未发布过;错的是 daemon 发布了一个它执行不了的组合、且没发布它能执行的那个。修法:`bind_dispatcher` 用**实际绑定**的工具重新发布 maturity，物化也改用它，一个真相来源 | 本机实测:ArkDeck 客户端 `startExecution` 得 `TOOLCHAIN_DIGEST_MISMATCH: the plan was materialized for toolchain 038a8a0e… and this daemon has 231a05ef… bound`;修复后 `ArkForgeLiveDaemonContractTests.testAMaterializedPlanIdentityStartsAJob` 通过(jobID 取得，`cancelledSafe` 取消，未签任何 permit) | A(本机实测) | resolved |
| DIG-001 | deterministic CBOR | RFC 8949 §4.2 | A | confirmed(仓内实现对 Appendix A 向量) |
| IPC-001 | Protobuf 演进规则 | protobuf.dev proto3 guide | A | confirmed |

### AD-015 对 ArkForge 的后果

`MaturityKey` 把 toolchain backend digest 计入组合键，这是对的——但本次证明 digest 相等
**不足以**保证工具能跑：同一份字节因 quarantine 而挂死。因此 AF-V2 的 preflight 除了比对
digest，还必须验证工具**可执行**(能在预算内返回)，并把「哈希命中但不可运行」与「哈希不符」
判为两种不同的 typed 结果——前者是宿主环境问题，后者是身份问题，补救动作完全不同。

ArkDeck 的 discovery profile 已带 `timeout: 5`，方向一致。本仓在 AF-V2 落地执行侧时须补上，
当前 AF-V1 无执行路径，故只记录不实现。

### AD-023 对 toolchain 摘要的后果

toolchain backend digest 是 maturity 组合键的一部分(architecture.md 12.3)，
所以「哪一份 `rkdeveloptool`」不是操作细节，是**发布了哪个组合**。

2026-08-15 彩排的全部证据都建立在 `038a8a0e…` 之上，而这一份**不可出厂**。
两条后果，都不是本仓单方面能决定的：

1. **真机全量刷写通过时用的是哪一份，决定了发布的是哪个组合。** 若用
   `038a8a0e…` 通过，发布的组合就钉在一份出不了厂的工具上;换成
   `231a05ef…` 出厂时，那是**另一个组合**，需要另一次真机通过。
   反过来，若直接用 `231a05ef…` 做那次真机通过，则彩排的读域/分区表/rebind
   实测仍然有效(它们是设备侧事实)，但「这份工具能跑这些命令」要重新证一次。
2. **建议在真机写入之前先换。** 换工具的代价现在是零(还没有任何写入证据)，
   在真机通过之后是一次完整重跑。

这属于 ArkDeck ADR-0003 已经写明的 bundled component registry 迁移，
本仓不能替它决定，只能把代价说清楚。

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
