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
验收。仓库固定使用 Rust 1.97.1 / Edition 2024。先构建工作区并查看当前主机能做什么：

```bash
cargo build --workspace
target/debug/arkforge help --format json
target/debug/arkforge --runtime-dir /tmp/arkforge doctor
```

启动本地 runtime 并查看设备：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge daemon start
target/debug/arkforge --runtime-dir /tmp/arkforge device list
```

固件会先进入内容寻址存储，再按返回的 artifact ID 离线检查：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge --runtime-dir /tmp/arkforge artifact inspect \
  --artifact <artifact-id> \
  --profile-file profiles/dayu200.yaml
```

正常刷写遵循固定的 `assess → plan → apply` 流程：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge flash assess \
  --artifact <artifact-id> \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id> \
  --intent full-restore

target/debug/arkforge --runtime-dir /tmp/arkforge flash plan \
  --artifact <artifact-id> \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id> \
  --intent full-restore
```

`plan` 不会修改设备。真正执行时，必须原样带回结果中的 plan ID、SHA-256 和全部 acknowledgement token。直接 CLI 刷写还要求 runtime 绑定摘要完全匹配的 HDC；在生产支持发布前，只能在明确授权的硬件 campaign 中执行。

不要猜参数或复用历史命令，让 CLI 返回当前构建的完整契约：

```bash
target/debug/arkforge help flash apply --format json
target/debug/arkforge completion --shell zsh
```

## 主要能力

### 统一的 Agent-native CLI

`arkforge` 覆盖完整生命周期：

```text
doctor
device      list / show / probe / wait
artifact    import / inspect / list / show
flash       assess / plan / apply
job         list / show / watch / cancel / reconcile / recovery
rescue      list / inspect / read / plan / apply
daemon      run / start / stop / status
signing     verify
completion
help
```

每一级命令都有稳定的人类帮助和 `arkforge.command-help/v1` JSON 描述。结构化输出不会混入颜色、进度条或提示符，错误会给出稳定 code、修复建议和下一条可执行命令。

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
