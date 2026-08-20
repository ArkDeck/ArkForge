---
id: CHG-2026-CLI-arkforge-agent-native-cli
revision: 1
status: implementing
class: capability
core_change_level: major
owner: TBD
platforms: [macos]
---

# ArkForge Agent-native CLI 与原生 RockUSB 救援面

> 本文批准前只是一份命令与权限语义提案，不开放任何新的设备写入路径。

## Why

ArkForge 已经拥有 DAYU200 的原生完整刷写 mechanics，但当前可执行入口只属于
ArkDeck authority。仓内三个面向操作者的二进制也没有形成一套产品语言：

- `arkforge-cli` 只连 public socket，只有只读查询；
- `arkforge-inspect` 直接处理本地归档；
- `arkforge-signing` 检查 macOS 签名；
- `arkforged` 暴露 daemon 启动参数，但不是用户工作流入口。

这使人和 Agent 都必须先理解进程边界、socket 种类和若干历史二进制，才能推导
「下一步做什么」。现场救援需要覆盖过去由 `rkdeveloptool` 提供的观察、读写和复位
能力，但 NRU-004 已正确删除 vendor runtime。救援面必须复用 ArkForge 自身已经通过
真机验证的 `NativeRockUsbPort`，不能重新依赖 vendor 二进制或第二套协议实现。

## Decision

新增单一前门 `arkforge`。它有两条互不继承成熟度或成功语义的执行路径：

1. **Normal flash**：`arkforge daemon start` 启动一个独立的本地 CLI authority
   supervisor 和 `arkforged` mechanics daemon。前者持有本地 target binding、签发
   exact StepPermit、完成 typed managed control；后者仍是唯一 mechanics 执行者。
   `arkforge` 命令进程通过 owner-only socket 向 supervisor 提交计划与明确批准。
   这不调用 public socket 的执行旁路，也不把 authority 塞进 mechanics daemon。
2. **Explicit rescue**：只有显式 `arkforge rescue ...` 才进入原生救援域。它复用
   `NativeRockUsbPort` 与 `RockUsbProtocol`，并拥有独立 RescuePlan/RescueReceipt，
   不生成正常
   `FlashPlan` 的成功收据，不获得 DAYU200 normal Provider 的 maturity，也永不成为
   normal flash 的自动 fallback。

所有 destructive 操作采用 `plan → apply`。没有 `--yes`、`--force`、任意 argv
透传或「选择第一台设备」。

## Command surface

~~~text
arkforge
├── doctor
├── device
│   ├── list
│   ├── show
│   ├── probe
│   └── wait
├── artifact
│   ├── import
│   ├── inspect
│   ├── list
│   └── show
├── flash
│   ├── assess
│   ├── plan
│   └── apply
├── job
│   ├── list
│   ├── show
│   ├── watch
│   ├── cancel
│   ├── reconcile
│   └── recovery
│       ├── guide
│       └── plan
├── rescue
│   ├── list
│   ├── inspect
│   ├── read
│   ├── plan
│   └── apply
├── daemon
│   ├── run
│   ├── start
│   ├── stop
│   └── status
├── signing
│   └── verify
├── completion
└── help
~~~

命令名只表达领域语义。公共面和 receipt 都不出现 `ld`、`ppt`、`rl`、`wlx`、`rd`
或 vendor argv；实现只调用 typed native protocol。

## Required semantic boundaries

### Normal flash is still authority-driven

CLI direct flash 的含义是“不依赖 ArkDeck”，不是“绕过 authority”。新 CLI
authority supervisor 必须：

- 使用独立 `authority_namespace = arkforge.cli`；
- 为一个独立 CLI runtime 配对 daemon，不抢占已配对 ArkDeck 的 controller；
- 只在内存持有 pairing secret，并通过继承 pipe 启动/配对 daemon；
- 对每一步重新验证 admission snapshot 后才签 Permit；
- 先把 Permit 原字节耐久保存，再提交；同一 pairing epoch 的传输重试只重放原字节；
- supervisor 重启时轮换 epoch，旧 epoch 中尚未消费的 Permit 永不首次消费；
- 通过 typed HDC control port 进入 Loader、等待 exact rebind、读取 postflight；
- 不把 HDC 路径、endpoint、connect key、argv 或 shell 带入回执；
- CLI/authority 中断后不重放 outcome-unknown action。

现有七轴 `MaturityKey` 描述 mechanics 组合，并不包含 authority；不能把 CLI
authority 假装成其中一个已有轴，也不能通过复用 `evidence_set_digest` 隐藏它。
本 change 新增独立 `AuthoritySupportKey`，至少覆盖 authority 实现摘要、managed
control 映射摘要和 mechanics maturity key 摘要。只有 mechanics maturity 与
authority support 两门都允许执行，才能产生 executable plan。现有 ArkDeck authority
真机通过不能自动发布 CLI authority support；CLI 实现必须先走具名 hardware
campaign，再由维护者发布确切组合。

### Rescue is an explicit, native contract

原生 rescue v1 只覆盖仓库已经真机验证过的 typed protocol 面：

| Rescue semantic action | Native implementation | Effect |
|---|---|---|
| `list-devices` | exact USB enumeration + Loader readiness | read-only |
| `read-partition-table` | typed GPT read through `READ_LBA` | read-only |
| `read-sectors` | typed `READ_LBA` | read-only |
| `write-partition` | typed, chunked `WRITE_LBA` | destructive |
| `reset-device` | typed `DEVICE_RESET` | mutating |

不开放 raw LBA write、任意 USB request、shell、子进程或 vendor 参数。以后增加动作必须
先形成 typed effect、身份规则、成功证据、故障分类和真机验证。

正常 flash 出错时只返回 typed recovery guidance，不得自动进入 rescue。

### Agent-native help is a contract

每一级命令都同时提供：

- `arkforge <path> --help`：面向人的稳定文本；
- `arkforge help [<path>...] --format json`：
  `arkforge.command-help/v1` 机器清单；
- effect 等级、必要前置条件、输入互斥/必选关系、输出 schema、退出码；
- 至少一个可复制示例和成功后的 `next_commands`；
- destructive 命令列出将覆盖的数据和所需 acknowledgement token。

结构化模式不输出颜色、进度条或提示符。错误包含稳定 `code`、`message`、
`remediation`、`retryable` 和 `next_commands`，因此 Agent 不需要匹配英文句子。

## Unreleased migration

ArkForge 尚未发布，不保留兼容 wrapper、旧参数或弃用周期。已有能力直接迁移到最新
命令树，旧 entry point 随实现删除：

| Existing entry | New entry |
|---|---|
| `arkforge-cli ... discover` | `arkforge device list` |
| `arkforge-cli ... inspect <id>` | `arkforge artifact show --artifact <id>` |
| `arkforge-cli ... assess ...` | `arkforge flash assess ...` |
| `arkforge-cli ... jobs` | `arkforge job list` |
| `arkforge-cli ... job <id>` | `arkforge job show --job <id>` |
| `arkforge-cli ... recovery-guide <id>` | `arkforge job recovery guide --job <id>` |
| `arkforge-inspect ...` | `arkforge artifact import` + `artifact inspect` |
| `arkforge-signing <file> [--release]` | `arkforge signing verify --file <file> --mode <mode>` |
| `arkforged ...` | 保留内部 mechanics daemon；用户入口为 `arkforge daemon ...` |

## Out of scope

- 不开放 DAYU600 execute；其 18 条证据门仍然全部适用。
- 不把 rescue 收据标记成 normal flash、full restore 或 ProductionVerified。
- 不提供远程 daemon、TCP listener 或多租户 controller。
- 不提供任意 USB control transfer、任意扇区写入或任意子进程 argv。
- 不在本 change 中删除旧二进制；只建立迁移和删除门。

## Safety and rollback

- 计划绑定 artifact hash、profile/version、exact device observation、authority binding、
  toolchain digest、effect set 和 execution purpose；`apply` 必须再次给出 plan digest。
- 每个数据影响都有具体 token，例如 `data-loss:userdata`；宽泛确认无效。
- rescue 使用单独 store/journal/receipt domain，避免其证据被正常路径读取成支持声明；
  它与 normal flash 复用同一个原生 RockUSB 实现，而不是复用 normal authority。
- 回滚新 CLI 不影响 `arkforged` journal 或 ArkDeck authority。已经开始的 job 按现有
  no-replay 规则处置，不能通过回滚恢复执行。
