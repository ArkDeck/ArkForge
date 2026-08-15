# Design — CHG-YYYY-NNN 规格实施文档

> 读者：在 ArkDeck 仓实现 Swift 侧的人。
> 本文只讲**怎么做**与**为什么必须这么做**；要不要做见 `proposal.md`。
>
> ArkForge 侧的对应产物都在 ArkForge 仓，路径在文中逐处给出，都能直接跑。
> **2026-08-15 更新：controller execution/admission surface 与 dispatch 均已实现**，
> 第 8 节列了逐项状态。还没有的是**在真机上跑一次**——现有端到端测试用的是
> 脚本化 tool port，不是设备（AF-V2.4）。

---

## 0. 一句话架构

~~~text
ArkDeck                                   arkforged
───────                                   ─────────
拥有 HDC、connectKey、target binding
拥有 authority：签发 StepPermit
                    ──── materializePlan ──▶   物化公私计划（已实现，只读）
                    ◀─── watchJob 事件流 ────   状态、admission 请求、回执
                    ──── submitStepPermit ─▶   验证 permit、落 intent、派发
                    ◀─── managedControl 请求──   「请你帮我进 Loader」
                    ──── submitReceipt ────▶   authority 观测到什么
                                              拥有 rkdeveloptool、封闭命令面
                                              拥有读域三态判定、durable journal
~~~

**ArkDeck 不再拥有的**：`wlx`/`rl` 的 argv、扇区地址、读窗语义、写进度解析。
**ArkForge 永远不会拥有的**：connectKey、hdc 路径、endpoint、shell、server 生命周期。

---

## 1. 要删的东西（先删，再接）

建议实现顺序把删除放在最前，因为它决定了后面每一步的形状。

### 1.1 `RockchipProviderAction` 的两个 case

`Packages/ArkDeckKit/Sources/ArkDeckWorkflows/DeviceProviders/DeviceProviderContract.swift`

~~~swift
case flashPartitions(RockchipRuntimeFlashBundle)     // 删
case verifyFlashReadback(RockchipRuntimeFlashBundle) // 删
~~~

### 1.2 `RockchipRuntimeActionHost` 中随之而去的实现

- `case .flashPartitions` / `case .verifyFlashReadback` 两个分支；
- `arguments: ["wlx", mapping.partitionName, image.stableDescriptorPath]` 的构造；
- `readSectors(executable:offsetSectors:count:...)` 与 `characterizeMediumReadDomain`；
- `RockchipWriteProgressParser`；
- `RockchipPinnedPartitionTable.span(for:)` 的扇区跨度守卫；
- `RockchipRockUSBFlashProvider.closedCommandSurface` 里的 `wlx`/`wl`/`rl`
  （`ld`/`ppt`/`rd` 也一并移交——ArkForge 侧同名封闭命令面在
  `crates/arkforge-provider/src/rockchip_execute.rs`）。

### 1.3 保留的十一个 case

其余全部是 HDC 侧的，只有 ArkDeck 能做。完整归属表在 ArkForge 仓
`adapters/arkforge-arkdeck-adapter/src/control.rs`，
每个 baseline case 都被标为 `keptByAuthority` / `keptInternal` /
`delegatedToArkForge`，并有测试断言三类之和穷尽 baseline。

### 1.4 一条不能省的收尾

`characterizeMediumReadDomain` 删掉，但它承载的教训不能跟着删。
实现 PR 需要在 `docs/` 留一条索引，指向 ArkForge 的 AD-006 与 AD-019：
2026-08-04 那九次「写入未落盘」全是冤案，来自 `rl` 在读窗外返回的 uniform filler。
2026-08-15 ArkForge 用一条独立代码路径复现了同一读窗（AD-019）。

> 这不是形式主义。删掉一段代码等于删掉写它的人当时知道的东西，
> 除非那件事被写在别处。

---

## 2. StepPermit：字节必须一致

### 2.1 权威定义

ArkForge `crates/arkforge-authority-api/src/lib.rs`，`StepPermit` 与 `permit_body()`。

签名体 = `permit_body(permit)` 的**确定性 CBOR 编码**，
tag = `HMAC-SHA256(pairingSecret, signingBody)`。

签名体**不含** `integrity_tag`——tag 覆盖 body，body 不覆盖 tag。

### 2.2 编码规则（RFC 8949 §4.2.1）

Swift 侧必须做到，一条都不能少：

| 规则 | 说明 |
|---|---|
| map key 按**编码后字节**排序 | 不是按字符串字典序。`"a"` 与 `"aa"` 的编码长度不同，排序结果可能与直觉相反 |
| 整数用最短形式 | `23` 编码为 `0x17`，不是 `0x1817` |
| 无浮点 | ArkForge `architecture.md` 15.4 在摘要模型里禁止浮点 |
| 无 tag、无不定长 | 编码器不得产生 major type 6 或不定长容器 |
| 文本按 UTF-8 原样 | 不做 NFC/NFD 归一 |

### 2.3 交叉验证向量

`permit-vectors.md`（本目录）给了三组 (permit, secret) → (signingBody 摘要, tag)。
Swift 侧的契约测试必须逐组复现。这是 `AFA-AC-2`。

对不上时的排查顺序（按经验命中率）：

1. map key 排序用了字符串序而不是编码后字节序；
2. 整数没用最短编码；
3. `singleUse` 这类 bool 被编成了 0/1 整数而不是 major type 7 的 20/21；
4. 32 字节摘要被编成了 hex 文本而不是 byte string。

ArkForge 那个测试可以打印完整 signingBody 供逐字节比对。

### 2.4 重传：重放字节，不要重新推导

~~~text
授权决定作出 → 完整 permit（含 tag）先落盘 → 才返回
重传同一个 permitID → 读出已存字节原样发出
~~~

**禁止**在重传时重新构造 permit 再签一次。两份字节不同的「同一张」permit
正是完整性标签要消除的歧义（ArkForge `architecture.md` 8.6）。
ArkForge 侧对同一个 permitID 的第二次 admission 会直接拒绝为
`IntentAlreadyRecorded`，不会创建第二个 intent。

### 2.5 PairingEpoch

任一进程重启就轮换。旧 epoch 签发的、**尚未消费**的 permit 永远不能被首次消费——
它作废，admission 重来。ArkForge 侧在 `verify_permit` 里判 `StalePairingEpoch`。

pairing secret 只在内存里，不落盘明文（ArkForge `architecture.md` 15.2）。

---

## 3. 线上契约

正本：ArkForge `proto/arkforge.proto`。以下是实现要点，不是重复定义。

### 3.1 方向：daemon 从不主动呼出

daemon 在 `watchJob` 流上**请求**，authority 回头**调用**。
每一条消息都是 client 发起的，authority 因此可以答、可以拒、可以干脆不答，
三者对 daemon 是不同的事件。

### 3.2 API 编号

| # | API | 会话 | 说明 |
|---:|---|---|---|
| 6 | `startExecution` | controller | 已实现；未配对 authority 时返回 `UNAVAILABLE` |
| 7 | `watchJob` | 任意 | 已实现。轮询而非推送——daemon 所有连接共用一把锁，一个停在那里等下一条事件的 handler 会挡住产生它的那次调用 |
| 12 | `submitStepPermit` | **controller only** | 已实现。答复一次 admission |
| 13 | `submitManagedControlReceipt` | **controller only** | 已实现。报告 authority 自己观测到什么 |

12/13 必须是 controller-only：能提交 permit 的 public 调用方，
就是一个没人配对过的 authority。ArkForge 侧
`SessionKind::may_call` 已按此实现并有测试。

### 3.3 admission 往返

~~~text
JobEvent{kind=STEP_ADMISSION_REQUESTED, admission=StepAdmissionSnapshot{request_id, …}}
        ▼
ArkDeck：拿自己的 binding 重新验证 snapshot 的每一项
        ▼
submitStepPermit{request_id, permit_cbor, integrity_tag, pairing_epoch}
   或    submitStepPermit{request_id, refusal:"…"}
~~~

**snapshot 要重新验证，不能回显。** 被原样送回的 snapshot 什么也没证明。
至少要核对：`plan_sha256` 是不是自己批准的那个计划、
`admitted_device_facts_sha256` 是不是自己当前 binding 的设备、
`observed_at_epoch_ms + snapshot_lifetime_ms` 有没有过期。

拒绝要用 `refusal` 字段明说。沉默与拒绝在 daemon 侧是两件事：
拒绝走 `CancelledSafe`，沉默走超时后的 snapshot 作废重来。

### 3.4 managed control 往返

~~~text
JobEvent{kind=MANAGED_CONTROL_REQUESTED, control_request=ManagedControlRequest{action, …}}
        ▼
ArkDeck：执行映射表里那一串 provider action
        ▼
submitManagedControlReceipt{request_id, accepted, facts, evidence_sha256}
~~~

`accepted=false` **不等于**「什么都没发生」。模式切换可能已经生效而没被观测到，
daemon 会把它记成 outcome unknown 而不是失败。要表达「确实没发生」，
用 `failure_reason` 说清楚依据。

---

## 4. ManagedDeviceControlPort 的 Swift 侧

映射表正本：ArkForge `adapters/arkforge-arkdeck-adapter/src/control.rs`。

| 语义动作 | provider action 序列 | 语义成功 |
|---|---|---|
| `ENTER_UPDATER` | `observeHDCNormalUSB` → `enterLoader` → `waitForHDCDisconnect` → `waitForLoader` → `rebindLoader` | 命令被接受 **且** 绑定身份断开 **且** 恰好一台设备以 Loader 重新绑定 |
| `REBOOT_TO_NORMAL` | `waitForBoundHDCReconnect` | 原绑定目标以相同 stable identity 回到 normal |
| `READ_PRODUCT_FACTS` | `verifyBoundBuild` | 绑定目标答出 `const.product.model` |
| `READ_BUILD_FACTS` | `verifyBoundBuild` | 绑定目标答出 `const.ohos.fullname` |

### 4.1 `ENTER_UPDATER` 是四次观测，不是一条命令

只映射 `enterLoader` 会让「命令被接受」被记成「设备已进入 Loader」。
ArkForge `architecture.md` 16.2 要求的是
「HDC accepted + expected disconnect + unique Loader rebind」三者齐备。

### 4.2 超时按实测取，不要按估计

ArkForge 2026-08-15 在真机上连续采样两次切换（AD-020）：

| 方向 | 认不出任何设备的时长 |
|---|---:|
| normal → loader | 3,725 ms |
| loader → normal | **15,579 ms** |

回 normal 的空窗 **15.6 秒**。任何短于此的 deadline 都会把健康的板子判成没回来。
现有 `reconnectDeadlineMilliseconds: 120_000` 余量充足，保持即可——
但这个数从今天起有实测依据，不再是估的。

同一次实测还确认：**serial digest 与 topology digest 两者都变**。
把 USB serial 当作跨模式稳定标识的实现会在这里认不出同一块板子。
唯一可用的跨模式锚点是 ArkDeck 自己的 stable identity。

### 4.3 回执里绝对不能出现的东西

~~~text
connectKey  hdcExecutablePath  hdcEndpoint  argv  shell  serverLifecycleAction
~~~

ArkForge 侧有断言（`control.rs` 的 `FORBIDDEN_RECEIPT_FACTS`），
daemon 收到含这些 key 的回执会拒绝。ArkDeck 侧需要对应的 secret-scan 测试，
覆盖 receipt、journal 与 UI 事件三处。

---

## 5. RuntimeJobEngine 接线

### 5.1 不变的部分

`flash.dayu200` 的 operation 契约、step 集合、UI 事件形状、
现有的 authorization/confirmation 判定——**一律不动**。
已有 journal 不需要迁移。

### 5.2 变的部分

原来：`RuntimeJobEngine` → `RockchipRuntimeActionHost` → 子进程。

现在：`RuntimeJobEngine` → `arkforged`：

1. `materializePlan`（已有的只读 API）拿到 public plan 与 plan digest；
2. `startExecution{plan_id, plan_sha256}` 开一个 job；
3. `watchJob` 订阅事件；
4. 对每个 `STEP_ADMISSION_REQUESTED`：跑现有的授权判定 → 签 permit → `submitStepPermit`；
5. 对每个 `MANAGED_CONTROL_REQUESTED`：跑映射表里的 provider action → `submitReceipt`；
6. 对每个 `ACTION_RECEIPT`：记 journal、驱动 UI。

### 5.3 postflight 期望值的来源（这条最容易做错）

期望的 build 版本**必须来自被写入的那份 `system.img`**，
不是归档文件名，不是 build log。

实测依据：2026-07-28 的 daily 归档名字写 `7.0.0.35`，它的 build log 也写 `7.0.0.35`，
而它产出的设备答 **`OpenHarmony-7.0.0.36`**（本仓 `RockchipFlashProfile.dayu200`
的注释已经记了这条，2026-08-04 在刷好的板子上确认）。

ArkForge 从镜像里提取这个值并放进计划的 postflight 期望
（`crates/arkforge-artifact/src/dayu200.rs`，AD-016）。
ArkDeck 侧读到 `ACTION_RECEIPT` 时按它比对即可，不要另起一份推导。

> 顺带：这个值在 2 GiB `system.img` 的第 320,762,067 字节。
> ArkForge 早先按「属性在文件头部」的假设设了 64 MiB 扫描上界，
> 结果在所有真实归档上都提取不到——这条假设从来没被量过。

---

## 6. 失败与恢复

### 6.1 daemon 崩溃

ArkForge 的 journal 落盘规则：与派发决定相关的记录在 `append` 返回前 `fsync`。
重启后按 `architecture.md` 13.3 的表推导处置，且**任何一行都不允许重放派发**。

ArkDeck 侧要处理的形态：

| daemon 重启后 | ArkDeck 该做什么 |
|---|---|
| 该 permit 已消费并有回执 | 拿原回执，不要重签 |
| 该 permit 消费中断、无回执 | outcome unknown。**不要**签第二张 permit |
| 该 permit 已签发但 daemon 没落 intent | 可以重传**同一个** permitID；不能签新的 |

第二行是最容易做错的：一个「重试一下」的按钮会在这里造成第二次写入。

### 6.2 掉电

ArkForge 的 fsync 只证明到**进程死亡**为止。macOS `fsync(2)` 不冲刷盘内缓存，
`F_FULLFSYNC` 需要 libc 而 ArkForge 的零依赖决定（AFD-0001）不允许。
记为已知边界（AD-017），不记为已通过的门。ArkDeck 侧不要据此声称掉电安全。

### 6.3 取消

- 只读步骤：尽快取消；
- permit 之前：`CancelledSafe`；
- 模式切换派发之后：等 rebind/reconcile；
- 写入中：排队到下一个安全边界。`wlx` 进行中**不可**中断——
  杀进程不等于安全取消，只会把结果变成 unknown。

---

## 7. 实现顺序建议

| 步 | 内容 | 可独立验证 |
|---:|---|---|
| 1 | 删两个 case 及其实现 | 编译 + grep 断言 |
| 2 | CBOR 确定性编码器 + permit 签发 | 三组交叉验证向量 |
| 3 | permit 对抗矩阵 | 七项否定用例，零派发 |
| 4 | `ManagedDeviceControlPort` | 四个动作 + secret-scan |
| 5 | `RuntimeJobEngine` 接线 | 与 ArkForge 联调（需 ArkForge 侧 AF-V2.4 接线同期完成） |
| 6 | 真机全量刷写 | `AFA-AC-6..9` |

第 2、3 步不依赖 ArkForge 侧的任何新代码，可以立刻开始并独立验收。

---

## 8. ArkForge 侧的状态（2026-08-15 更新）

controller execution/admission surface **已实现并固定**：

| 项 | 状态 |
|---|---|
| `startExecution`（API 6） | ✅ 建 job、开 per-job journal、发布第一条 admission |
| `watchJob`（API 7） | ✅ 事件流，`from_sequence` 支持断线续传 |
| `cancelJob`（API 8） | ✅ permit 之前 `CancelledSafe`；之后拒绝为 `CANCEL_NOT_SAFE` |
| `submitStepPermit`（API 12） | ✅ 逐项验证并落 durable intent |
| `submitManagedControlReceipt`（API 13） | ✅ 校验 request/action、拒绝禁止事实、落回执与 checkpoint |
| 新增消息的 Rust 编解码 | ✅ 全部 round-trip 测试 |
| permit 的 CBOR 解码 | ✅ `StepPermit::from_canonical_bytes` |

九条端到端测试在 `crates/arkforged/tests/admission_surface.rs`，
用真实归档物化的真实计划驱动整套握手。

### 8.1 ArkDeck 侧现在可以对着什么写

- **permit 编码**：`StepPermit::from_canonical_bytes` 做**往返校验**——
  解出来再编回去必须与输入逐字节相同，否则拒绝为 `NotCanonical`。
  这意味着 Swift 侧只要有一个字节的编码差异就会被当场拒绝，而不是默默通过。
  先用 `permit-vectors.md` 的三组向量对齐，再接线。
- **snapshot 新鲜度**：`SNAPSHOT_LIFETIME_MS = 60_000`。晚到的 permit 被拒为
  `SNAPSHOT_EXPIRED`，daemon 同时发布一条**新的** admission——不需要重启 job，
  重新签一张即可。
- **禁止事实**：回执里出现 `connectKey`/`hdcExecutablePath`/`hdcEndpoint`/
  `argv`/`shell`/`serverLifecycleAction` 任一，整条回执被拒为
  `RECEIPT_CARRIES_FORBIDDEN_FACTS`。不是丢字段继续。
- **`accepted: false` 的含义**：job 进入 `outcomeUnknown`，不是失败。
  要表达「确实没发生」，在 `failure_reason` 里说清依据。
- **配对**：daemon 用 `--pair-from-stdin <epoch>` 启动，authority 把 secret 写进
  它的 stdin 再关闭。没配对时 `startExecution` 与 API 12/13 一律返回
  `EXECUTION_DISABLED`，且这个判断在解析 payload **之前**——
  它是 daemon 的常驻事实，不是某一次请求的事实。

### 8.2 dispatch（2026-08-15 已实现）

写入执行也接上了：`crates/arkforged/src/dispatch.rs`。

- **在服务锁之外跑。** job registry 交出一份 `PendingDispatch`，dispatcher
  **取走**它（取走即标记 in-flight，第二个 dispatcher 拿不到同一份），
  释放锁，跑完，再回来记录。锁只在两头各持有一次短写。
  这条不是洁癖：daemon 所有连接共用一把 mutex，2 GiB 的 `wlx` 要几分钟，
  在锁里跑会冻住本该报告它的那条事件流。
- **一个 step 的全部私有动作按序跑**：只读子动作在前，唯一的 primary 在后
  （architecture.md 6.3），回执报告 primary 的结果。
  最初我只跑了第一个动作，结果 `characterize-read-domain` 跑了、
  `readback-partition` 没跑——九个目标一个判定都没有。
- **镜像在第一次写入时才 staging**，一次，之后复用。没走到写入的 job 不必先付
  4 GB 的解压代价。
- **失败分两类，这是本模块唯一的判断**：tool 被 spawn **之前**的拒绝
  → `ConfirmedNoEffect`（设备可证明未被触碰）；spawn **之后**的失败
  → `OutcomeUnknown`。搞反任一方向，要么把真实效果记成「无效果」，
  要么让每一次被拒的前置检查都变成待 reconcile 的 job。

daemon 用 `--rkdeveloptool <绝对路径>` 启动 dispatcher；不给就没有 dispatcher，
job 会停在第一个需要派发的步骤上等着（这是诚实的停，不是崩）。

### 8.3 端到端测试

`crates/arkforged/tests/admission_surface.rs` 十一条，其中
`a_job_dispatches_every_step_and_reaches_a_verdict_on_each` 用一个脚本化的
tool port 把整个计划跑完：九条 `wlx` 按 Profile 声明顺序发出、`ppt` 先于它们、
`rd` 在最后、九个 readback 全部给出 `typedSkip` 且不带任何 strength。
脚本里的 `ppt` 输出与读面行为都取自 2026-08-15 的真机实测（AD-018、AD-019）。

**仍然没有的**：真机上跑一次。测试用的是脚本 port，不是设备。

### 8.4 Readiness：机器可读，且不是「配对了就行」

daemon 的执行就绪是**两个常驻事实**，都在启动时确定，任何请求都改不了：

| 事实 | 缺了会怎样 |
|---|---|
| authority 已配对 | permit 验不了，回执没地方去 |
| fixed tool 已绑定 | 需要本 daemon 派发的步骤跑不了 |

**只配对不算就绪。** 早先的版本只看配对，结果 job 会一路走到第一个 dispatch、
花掉一张 permit、然后停在那里——那是要 reconcile 的状态，而不是「没启动」。
现在 `startExecution` 在**解析 payload 之前**就按常驻事实拒绝，
并且**一次报全部缺失项**，免得修完一个才发现还有第二个。

### 8.5 ArkDeck 侧怎么读

握手就能读到，不必先物化一个跑不了的计划：

~~~text
HelloAck {
  execution_ready:      bool
  execution_blockers:   ["NO_PAIRED_AUTHORITY", "NO_DISPATCHER"]   // 稳定码
  toolchain_id:         "rkdeveloptool"
  toolchain_sha256:     "bbd7bdc0…"
}
~~~

`execution_blockers` 为空 ⟺ `execution_ready` 为真。

**把 `toolchain_sha256` 和你计划里的 toolchain 摘要比一下。** 不一致时
`startExecution` 会拒为 `TOOLCHAIN_DIGEST_MISMATCH`——toolchain 摘要是 maturity
组合键的一部分（architecture.md 12.3），换一份工具就是在跑一个没人发布过的组合。
daemon 会拒，但你不必等它拒才知道。

### 8.6 工具摘要现在是强制比对，不再只是打印

~~~bash
arkforged --runtime-dir <dir> --profile profiles/dayu200.yaml \
  --pair-from-stdin <epoch> \
  --rkdeveloptool /absolute/path \
  --rkdeveloptool-sha256 <64 hex>
~~~

- `--rkdeveloptool-sha256` 是**必填**的，只要给了 `--rkdeveloptool`。
  绑一个没钉过的工具就是绑「碰巧在那个路径上的东西」。
- 实测字节与钉值不符 → **拒绝启动**，不是启动后跑不了。
- 三条启动路径都验过：
  - 配对无工具 → `execution: not ready (NO_DISPATCHER)`
  - 有工具无钉值 → 拒绝启动，说明理由
  - 钉值不符 → 拒绝启动，两个摘要都打出来

**这条不能证明的事**：字节相等不等于工具能跑。同一份字节带 quarantine 时会挂死在
dyld（AD-015），readiness 里任何字段都不会显示这一点。要真正确认得跑一次
`ld`，而那会碰设备，因此不在启动时自动做。

除此之外，ArkForge 侧的执行机制——封闭命令面、读域三态、staging 与写前
revalidate、durable journal、permit 单次使用——**都已完成并在真机上验证到写入前的
最后一步**，见 `../../evidence/runs/2026-08-15-dayu200-flash-rehearsal.md`。
