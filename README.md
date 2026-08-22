# ArkForge

简体中文 · [English](README.en.md)

**为 OpenHarmony 开发板提供安全、可审计、可恢复的固件刷写。**

ArkForge 面向支持 OpenHarmony 的开发板，把不同的固件格式、芯片下载协议和 USB 传输方式收进一套一致的工作流，让开发者和 Agent 都能用同一种方式完成刷机：

```text
检查环境 → 识别设备与固件 → 评估影响 → 生成计划 → 明确确认 → 执行 → 验证与恢复
```

它提供统一命令行 `arkforge`，也可以作为 ArkDeck 的底层刷写执行层。无论从哪个入口使用，ArkForge 都不会“找到一台设备就开始刷”，而是把目标设备、固件、Profile、数据影响和执行环境一起封进计划，再按计划逐步执行并留下可审计的收据。

## 为什么需要 ArkForge

支持 OpenHarmony 的开发板来自不同芯片平台，刷机能力往往散落在厂商工具、脚本、USB 协议和产品逻辑之间。ArkForge 把这些差异收敛到清晰的边界里：

- **一个入口**：设备发现、固件导入、计划、执行、任务查询和救援都通过 `arkforge` 完成；
- **原生执行**：DAYU200 直接使用仓内实现的 RockUSB 协议，不安装或调用 vendor 刷机工具；
- **先计划，后执行**：破坏性操作必须绑定精确计划摘要和完整的数据影响确认 token；
- **不会盲目重试**：持久日志（journal）记录每一步，结果不确定的写入不会在重启后自动重放；
- **对 Agent 友好**：同一命令树同时生成给人看的帮助、稳定 JSON/JSONL、错误恢复建议和 shell completion；
- **设备无关**：新设备通过 Artifact Parser、Provider、Transport 和数据化 Device Profile 接入，不把型号分支散落到上层产品；
- **快而不放松校验**：最新执行管线会缓存已密封固件的解析结果、并行校验待写镜像，并避免对不可读区域进行无意义的整段回读。

## 当前支持状态

ArkForge 目前处于**硬件准入阶段**，还不是面向普通用户的一键刷机产品。

| 设备 | 当前能力 | 状态 |
| --- | --- | --- |
| **DAYU200**（RK3568 / RockUSB） | 固件导入与检查、设备观察、九分区完整覆写、逐分区验证、任务恢复、原生 RockUSB 救援 | 原生刷写执行层已通过多次真机全量刷写；独立 CLI 授权链路与救援链路的软件实现已完成，仍等待各自的受控真机验收和维护者审核 |
| **DAYU600**（uis7885 / PAC） | PAC 结构观察、Profile 候选和不可执行的 PlanAssessment | 仅研究与计划评估；18 条执行证据门当前 0 条通过，不提供刷写入口 |

生产支持注册表目前仍为空。`--hardware-campaign` 只用于具名、受控的硬件验收，不是跳过安全门的 `--force`，也不会自动发布生产支持。

详细进度见[实施任务台账](TASKS.md)和[证据账本](docs/evidence/ledger.md)。

## 快速体验

当前 runtime 支持 macOS 与 Windows x64；Windows 的发布签名与真机证据仍按独立组合
验收。仓库固定使用 Rust 1.98.0 / Edition 2024。先构建工作区并查看当前主机能做什么：

```bash
cargo build --workspace
target/debug/arkforge help --all --format json
target/debug/arkforge --runtime-dir /tmp/arkforge status
```

`status`（等价于不带子命令的 `arkforge`）把主机、runtime、设备、artifact、任务与
blocker 聚合成一份 `arkforge.status/v1`：无法观测的区段报告 `items: null` 与 typed
`reason`，只有完成枚举且结果为零才是 `items: []`。它永远不会顺手把 runtime 拉起来。

需要 runtime 的命令会**自动把它拉起来**，不用先手动 start：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge device list --deep
```

自动拉起会读 `config` 里的绑定，在 owner-only 启动锁下并发幂等（并发命令只会有一个
真正创建 runtime，其余校验一致后附着），并且**如实披露**：human 模式打印一行提示，
JSON 文档带 `runtime_autostarted: true`。`--no-auto-start` 恢复原来的 typed refusal。
已被 ArkDeck 配对的 runtime 永远不会被接管。`status`（和不带子命令的 `arkforge`）
刻意不自动拉起——它必须能回答「什么都没在跑」而不改变这个答案。

可复用的本机绑定用 `config` 一次配好（owner-only 存储，原子提交）：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge config set \
  hdc.path=/usr/local/bin/hdc hdc.sha256=<64hex>
target/debug/arkforge --runtime-dir /tmp/arkforge config show
```

path 与 digest 是**一个事务**：配置里不会出现没被钉住字节的可执行文件；相对路径直接拒绝；
每次启动前重新 hash，字节漂移是 typed refusal；写入失败保留旧配置。
`config show --output json` 只给绑定状态、digest 与计数，不输出任何 host/HDC 路径。
`campaign` 不是合法配置键——返回 `CAMPAIGN_NOT_PERSISTABLE`，因为能被留在配置里的
campaign 就不再意味着「这次运行经过评审」。

`device list` 逐台给出 identification 块：**兼容 profile** 与**物理型号**分开报告，
各带证据链与强度。USB VID/PID 只能证明协议人格，永远不单独证明板子；Loader 弱身份下
`model` 为 `null`。`--deep` 会对每个候选 profile 主动探测并附上返回的事实。

固件先进入内容寻址存储；导入一次就返回全部 staging 事实（CAS + manifest 摘要 +
声明该格式的 profile + 在场可刷设备）：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge --runtime-dir /tmp/arkforge artifact show \
  --artifact <artifact-id> \
  --profile-file profiles/dayu200.yaml
```

人在终端前，正常刷写是**一条命令**：

```bash
target/debug/arkforge flash ./firmware.tar.gz
```

`flash run`（`arkforge flash` 即是它）把 runtime 确保 → 内容/设备/profile/intent 解析 →
assessment → 封 plan → **同意门** → apply → 跟踪串成一步。同意门是唯一不被推断的环节。

交互门的判据很窄：**stdin、stdout、stderr 三者都是 TTY，且 `--output human`，且没给
`--no-input`**。任何一路被重定向都关闭它——stdout 接的是管道就说明读它的是程序，
而程序没法回答问题，只会挂在那里。门关着时，每个缺失的决策都是 typed refusal。

确认屏展示识别块（含证据与强度）、固件 hash、profile/intent 及其 resolution、全部
persistent effect、要接受的 token 与当次 campaign。接受方式按身份强度升级：
本构建**证不出**是哪块板子时，每次都要把 profile 声明的型号全称打出来——人工输入
不会让 `strength` 变强，只是让这个断言归你；证得出且是这块板 × 这个 profile 的
**首刷**时也要打全称；此后才降为 `y`。首刷只在任务成功终态后才被记下，失败或中断
不消耗它。

执行前，CLI authority 会先把一条独立的 `arkforge.cli-approval/v1` 记录**耐久落盘**：
精确 plan/digest/token 集、`interactive-tty` 还是 `argv` 的来源、人工型号断言、campaign
与时间。写不下去就零 dispatch——一次没人能证明被批准过的执行，比一次没发生的执行更糟。
这条记录不改 `arkforged` 的 journal 与 receipt，也不计入 mechanics evidence。

非交互路径缺 `--ack` 时返回 `ACKNOWLEDGEMENT_REQUIRED`，facts 里带**已经封好的那个
plan** 和直接执行它的 `apply` 命令——不会再物化第二个 plan。

需要分阶段 review 时仍是 `plan → apply` 两条命令。`flash plan` 一次完成导入、识别、评估与封 plan：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge flash plan \
  --file ./firmware.tar.gz \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id>
```

`--file` 是**隐式 import**：字节先进 CAS，hash 封入 plan，输出回报 artifact_id——plan 里
从不出现调用方路径。`--profile` 与 `--intent` 在能被推断时可省略：profile 取「固件声明的
格式 ∩ 设备匹配的 usbIdentities」，恰一才采用；intent 在该组合只有一个合法值时默认。
多台候选设备用 `--device <observation-id>`（精确）或 `--target <选择器>`（序列号摘要、
≥4 字符唯一前缀、已证明的型号名）消歧，两者互斥；歧义永远是 typed refusal，不会默认选一台。

**但推断永不越过身份门**：本构建无法证明目标是哪块板子时（Loader/Maskrom 下只有
VID/PID 与 mode），封 plan 必须同时显式给 `--profile` 与精确 `--device`，否则返回
`IDENTITY_CONFIRMATION_REQUIRED`。人工断言不会把 `strength` 提升为 `strong`。
`--assess-only` 只出评估、不物化 plan，即使 `executable:false` 也 exit 0。

输出是单份 `arkforge.flash-plan/v2`：`resolved`（含每项的 resolution 与识别证据）、
`assessment`、`plan` 与可直接执行的 `apply_command`。门未通过时 exit 3，同一份文档出现在
`error.facts.flash_plan` 里且 `plan:null`——失败路径与成功路径信息等价。

`plan` 不会修改设备。执行用顶层 `apply`——它是 normal flash plan 与 recovery plan
**共用的同意动词**，因为两者要求操作者做的是同一件事：对一组具名的破坏性 effect 表示同意。

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge apply \
  --plan <plan-id> \
  --expect-plan-sha256 <sha256> \
  --ack <token>
```

必须原样带回结果中的 plan ID、SHA-256 和全部 acknowledgement token：多一个 token 是
`UNEXPECTED_ACKNOWLEDGEMENT`，少一个不放行，宽泛的 `--yes` / `--force` 不存在。
rescue plan（`rescue-plan:<sha256>`）会在读 authority store **之前**按 ID 形状被拒绝并
指向 `rescue apply`——救援是独立的同意域。runtime 若正服务某个 hardware campaign，
当次必须显式给出同一个 `--hardware-campaign`：campaign 永不被继承，也永不为了迁就参数
而重启 runtime，不匹配时零 dispatch。

`watch` 不带参数时默认跟随唯一在跑的 job；没有在跑的就报告最近活动过的那个；多个在跑
是真歧义，列出候选并拒绝。`cancel --job --expect-sequence` 语义不变。

直接 CLI 刷写还要求 runtime 绑定摘要完全匹配的 HDC；在生产支持发布前，只能在明确授权的硬件 campaign 中执行。

不要猜参数或复用历史命令，让 CLI 返回当前构建的完整契约：

```bash
target/debug/arkforge help apply --format json
target/debug/arkforge completion --shell zsh
```

## 主要能力

### 统一的 Agent-native CLI

`arkforge` 覆盖完整生命周期：

```text
status      主机 / runtime / 设备 / artifact / 任务 / blocker 聚合快照
device      list [--device] [--deep] / wait
artifact    import / list / show
flash       run [FILE] / plan [--assess-only]
apply       执行 sealed plan（normal 与 recovery 共用的同意动词）
watch       [--job] 默认跟随在跑的 job
cancel      --job --expect-sequence
job         list / show / reconcile / recover
rescue      list / inspect / read / plan / apply
config      show / set / unset / add / remove
daemon      run / start / stop
signing     verify
completion
help        [<命令路径>] / --all
```

查询面按**决策点**而不是按内部资源切分：`device list` 一条覆盖原 show/probe，
`artifact show` 吸收了离线 inspect，`job show` 内嵌事件尾、全部 action receipt 与
no-replay 恢复块。复合文档里每个内嵌区段各自报告 availability——无法观测的区段是
`items: null` 加 typed reason，不会伪装成空集。

任务结果不确定（`outcomeUnknown`）时，`job recover` 用与正常路径**同一个推断引擎**物化
一份 superseding plan——「哪台设备、哪个 profile、哪包固件」在这里是同样的问题：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge job recover \
  --job <job-id> --artifact <artifact-id>
```

它输出的同样是 `arkforge.flash-plan/v2`，由顶层 `apply` 执行；`apply_command` 里除 effect
token 外还带 `recovery:supersedes-job=<job-id>`。原 job **永不 resume**：它的结局、journal
与 permit 原样保留，新计划是新 epoch 的另一件事。

每一级命令都有稳定的人类帮助和 `arkforge.command-help/v1` JSON 描述；`help --all`
（以及不带路径的结构化 `help`）一次返回整棵树的 `arkforge.command-help-index/v1`，
其中每个 leaf 与逐路径查询逐字节一致，并各自声明 `runtime_effect` 与
`facts_projections`。结构化输出不会混入颜色、进度条或提示符，错误会给出稳定 code、
修复建议和下一条可执行命令。

### 可审计的安全模型

- 设备选择必须来自精确 observation；零台、多台或身份变化都会在 mutation 前拒绝；
- 固件进入内容寻址存储，计划绑定 artifact、Profile、设备、toolchain、effects 和 authority；
- 每个 mutation/destructive step 都需要一次性 StepPermit；
- apply 必须匹配完整 plan digest 与 acknowledgement 集合，宽泛的 `--yes` 或 `--force` 不存在；
- 任务 journal 在进程重启后仍可查询，`outcomeUnknown` 永不自动 replay；
- normal flash 和 rescue 使用不同的 plan、receipt 与证据域，正常刷写失败时不会自动降级到救援。

### 原生 RockUSB 与显式救援

DAYU200 的枚举、Loader 切模、分区读写、复位和 read-domain-aware verification 都由 `arkforged` 的原生 RockUSB 实现。救援能力复用同一套 typed 协议，但只在显式 `arkforge rescue ...` 工作流中开放，不接受任意 USB request、raw LBA write、shell 或 vendor argv。

### 独立运行，也能接入 ArkDeck

独立使用时，`arkforge` 的本地 supervisor 负责 authority：绑定目标、签发精确 permit，并通过 typed HDC 完成模式切换与 postflight。`arkforged` 只负责固件解析、协议、USB、写入、验证和耐久状态。

接入 ArkDeck 时，ArkDeck 可以通过 `arkforge-arkdeck-adapter` 承担 authority。两套 runtime 使用独立 namespace，不能接管彼此已经配对的 daemon；ArkForge Core 也不依赖 ArkDeck 类型。

## 开发与验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

工作区没有第三方 Rust 运行时依赖。SHA-256、deterministic CBOR、DEFLATE、tar 和 Protobuf wire codec 均在仓内实现并使用公开测试向量验证，设计理由见 [AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md)。

macOS 发布物是一个 `ArkForge.bundle`：分别签名的 `arkforge`、`arkforged` 与发布 profiles 由 `Contents/Resources/arkforge-bundle.json` 逐成员绑定。打包入口为 [`packaging/macos/package-arkforge.sh`](packaging/macos/package-arkforge.sh)，发布包不携带 vendor RockUSB 工具。Swift 接入使用 [`swift/ArkForgeSDK`](swift/ArkForgeSDK) 的 `ArkForgeProtocol` 与 `ArkForgeClient`，无需复制 IPC codec。

Windows 发布面使用 local-only Named Pipe、当前用户 ACL、WinUSB 和 Authenticode。
打包、驱动绑定、安装/卸载及软件/真机分级验收入口见
[`packaging/windows/README.md`](packaging/windows/README.md)。生产包必须使用 Windows
Hardware Developer Program 返回的签名 catalog；仓库不会用应用证书伪装生产驱动签名。

## 进一步了解

- [架构与安全边界](docs/architecture.md)
- [Agent-native CLI 设计](docs/openspec/chg-agent-native-cli/proposal.md)
- [CLI 验收矩阵](docs/openspec/chg-agent-native-cli/verification.md)
- [实施任务台账](TASKS.md)
- [真机与验收证据](docs/evidence/)
- [架构决定记录](docs/decisions/)
