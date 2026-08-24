# ArkForge

简体中文 · [English](README.en.md)

ArkForge 是一个开源、设备无关的固件操作平台。它把固件解析、设备识别、刷写协议、
USB 传输、验证与恢复收敛到一套原生 Rust 工作流中，既可以独立运行，也可以作为
[ArkDeck](https://github.com/ArkDeck/ArkDeck) 的底层执行层。

它不会“找到一台设备就开始刷”，而是先把设备、固件、Profile、数据影响和执行环境
封进一份精确计划，再经过明确同意后执行，并为每一步留下可审计的记录。

```text
识别设备与固件 → 评估影响 → 封存计划 → 明确同意 → 执行 → 验证与恢复
```

## 当前状态

ArkForge 目前处于**硬件准入阶段**。生产支持注册表仍为空；普通用户请勿将它视为
一键刷机工具。

| 设备 | 能力 | 状态 |
| --- | --- | --- |
| **DAYU200**（RK3568 / RockUSB） | 固件检查、设备观察、九分区完整覆写、逐分区验证、任务恢复、原生 RockUSB 救援 | 原生执行层已完成多次真机全量刷写；独立 CLI 授权链路与救援链路仍需各自的受控真机验收和维护者审核 |
| **DAYU600**（uis7885 / PAC） | PAC 结构观察、Profile 候选、不可执行的 PlanAssessment | 仅研究与计划评估；18 条执行证据门当前 0 条通过，不提供刷写入口 |

`--hardware-campaign` 只用于具名、受控的硬件验收。它不是 `--force`，不会跳过安全门，
也不会自动发布生产支持。详细状态见[实施任务台账](TASKS.md)和
[证据账本](docs/evidence/ledger.md)。

## 构建

仓库固定使用 Rust 1.98.0 / Edition 2024。当前 runtime 面向 macOS 与 Windows x64；
Windows 的发布签名和真机证据按独立成熟度组合验收。

```bash
git clone https://github.com/ArkDeck/ArkForge.git
cd ArkForge
cargo build --workspace
target/debug/arkforge
```

不带子命令的 `arkforge` 等价于 `arkforge status`：它只报告主机、runtime、设备、
artifact、任务与 blocker，不会为了回答状态而启动 runtime 或修改设备。

## 使用 ArkForge

查看环境并发现设备：

```bash
target/debug/arkforge status
target/debug/arkforge device list --deep
```

需要 runtime 的命令会按配置自动拉起本地 `arkforged`。如果不希望自动启动，使用
`--no-auto-start`。复用的 HDC 绑定必须同时固定绝对路径与文件摘要：

```bash
target/debug/arkforge config set \
  hdc.path=/usr/local/bin/hdc hdc.sha256=<64hex>
target/debug/arkforge config show
```

固件先进入内容寻址存储。导入会返回 artifact ID、manifest 摘要、候选 Profile 与当前
可匹配的设备：

```bash
target/debug/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge artifact show \
  --artifact <artifact-id> \
  --profile-file profiles/dayu200.yaml
```

有人值守的终端里，正常刷写入口是一条命令：

```bash
target/debug/arkforge flash run --file ./firmware.tar.gz
```

ArkForge 只在 stdin、stdout、stderr 都是 TTY、输出为 human 且没有 `--no-input` 时展示
交互确认。重定向、管道和结构化输出始终走非交互路径，不会挂起等待输入。

需要分阶段评审时，先封存计划，再原样带回 plan ID、摘要和全部 acknowledgement token：

```bash
target/debug/arkforge flash plan \
  --file ./firmware.tar.gz \
  --profile org.openharmony.dayu200@1.0.0 \
  --device <observation-id>

target/debug/arkforge apply \
  --plan <plan-id> \
  --expect-plan-sha256 <sha256> \
  --ack <token>
```

在生产支持发布前，任何真实写入都还必须进入明确授权的 hardware campaign，并满足对应
设备、固件、toolchain、平台和 authority 的成熟度要求。

不要猜参数或复用历史命令。当前二进制可以输出完整的人类帮助、稳定 JSON 契约和 shell
completion：

```bash
target/debug/arkforge help --all --format json
target/debug/arkforge completion --shell zsh
```

## 安全默认值

- **精确目标**：计划绑定一次具名的设备 observation；零台、多台或身份变化都会在修改前拒绝。
- **内容寻址**：固件按摘要存储，调用方路径不会进入计划；计划同时绑定 Profile、toolchain、
  effects 与 authority。
- **精确同意**：执行必须匹配完整 plan digest 和 acknowledgement 集合，不提供宽泛的
  `--yes` 或 `--force`。
- **不盲目重试**：耐久 journal 记录执行边界；结果不确定的写入在重启后永不自动 replay。
- **显式救援**：normal flash 与 rescue 使用不同的计划、收据和证据域，不会静默降级。
- **Agent-native**：同一命令树生成稳定 JSON/JSONL、typed error、可执行修复建议和 completion。

DAYU200 的枚举、Loader 切换、分区读写、复位和 read-domain-aware verification 均由
`arkforged` 的原生 RockUSB 实现；发布包不安装或调用 vendor RockUSB 刷写工具。

## 接入方式

| 接口 | 用途 |
| --- | --- |
| `arkforge` | 面向开发者与 Agent 的统一 CLI；覆盖 status、device、artifact、flash、apply、job 与 rescue |
| `arkforged` | 本地 mechanics daemon；负责解析、协议、USB、写入、验证和耐久状态 |
| [`arkforge-client`](crates/arkforge-client) | Rust application API 与 typed client |
| [`ArkForgeSDK`](swift/ArkForgeSDK) | Swift 的 `ArkForgeProtocol` 与 `ArkForgeClient` |
| [`arkforge-arkdeck-adapter`](adapters/arkforge-arkdeck-adapter) | 让 ArkDeck 承担 authority，不向 ArkForge Core 引入 ArkDeck 类型 |

独立 CLI 和 ArkDeck 使用不同的 runtime namespace，不能接管彼此已经配对的 daemon。
Authority 决定“谁可以对哪台设备执行什么”；ArkForge mechanics 只负责把已授权计划正确地
落实到具体固件格式、Provider 与 Transport。

## 开发

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Rust runtime 没有第三方依赖。SHA-256、deterministic CBOR、DEFLATE、tar 和 Protobuf wire
codec 均在仓内实现，并使用公开测试向量验证。设计理由见
[AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md)。

## 文档

- [语言无关规范正本](spec/README.md) — 状态机、摘要模型、端口契约、错误码与 conformance fixtures
- [架构与安全边界](docs/architecture.md)
- [Agent-native CLI 设计](docs/openspec/chg-agent-native-cli/proposal.md)
- [CLI 验收矩阵](docs/openspec/chg-agent-native-cli/verification.md)
- [真机与验收证据](docs/evidence/)
- [架构决定记录](docs/decisions/)
- [Windows x64 打包与验收](packaging/windows/README.md)

## License

Apache-2.0
