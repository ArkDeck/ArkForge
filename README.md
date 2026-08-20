# ArkForge

设备无关的刷机机械层与 Agent-native 命令行(Rust)。统一入口 `arkforge`
负责显式计划、CLI authority、观察与恢复；独立 daemon `arkforged` 只负责固件
解析、USB/芯片协议、分区擦写与验证。ArkDeck 可适配同一 mechanics 契约，但不再是
CLI 直接刷机的运行时依赖。

项目主页：[github.com/ArkDeck/ArkForge](https://github.com/ArkDeck/ArkForge)

~~~text
Authority（ArkDeck 或独立 arkforge.cli）决定：谁、对哪台设备、以哪个已发布
Operation、在什么安全边界下执行。

ArkForge 决定：该已授权语义计划如何通过具体固件格式、Provider 和 Transport 正确落地。
~~~

## 状态

DAYU200 的设备枚举、Loader 切模、读写、复位、九分区完整覆写和逐步状态均由
`arkforged` 的原生 RockUSB 实现；仓内没有 vendor 可执行调用路径。执行采用耐久
journal、精确 StepPermit、同一 transport session 的 freshness 复核，重启后不会
重放未决写入。DAYU200 profile 发布 1.0.0 complete-overwrite coverage；独立
`arkforge.cli` supervisor 通过 owner-only controller IPC、typed HDC 与持久 epoch
直接驱动 normal flash。原生 rescue 是另一套 plan/receipt 域，绝不自动 fallback。

CLI authority 与 native rescue 的软件面已完成；production support registry 仍为空，
等待受控 DAYU200 campaign 与维护者 exact-key review。`--hardware-campaign` 只开启
具名 campaign evidence，不会发布生产支持。

DAYU600 只有 inspect 与非可执行 PlanAssessment：PAC 格式、下载协议与数据影响
全部未知(UNI-U01..U12)，17.5 的十八条证据门 0 条 PASS，见
[证据账本](docs/evidence/ledger.md)。

工具链钉在 `rust-toolchain.toml` 的 Rust 1.97.1 / Edition 2024。CI 执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

构建后先让 Agent 读取契约并检查主机：

```bash
cargo build --workspace
target/debug/arkforge help --format json
target/debug/arkforge --runtime-dir /tmp/arkforge doctor
```

macOS 发布输入是同目录、分别签名的 `arkforge`/`arkforged` 二进制对；CLI 只启动
自身旁边的 daemon。`packaging/macos/package-arkforge.sh` 不携带任何 vendor
RockUSB 工具。

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge daemon start
target/debug/arkforge --runtime-dir /tmp/arkforge device list
```

固件先进入内容寻址存储，再按返回的 artifact ID 离线检查：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge artifact import --file ./firmware.tar.gz
target/debug/arkforge --runtime-dir /tmp/arkforge artifact inspect --artifact <artifact-id> --profile-file profiles/dayu200.yaml
```

Normal flash 的 runtime 还必须绑定绝对路径和预期摘要完全匹配的 HDC（发布包可由
签名 tool manifest 提供）；受控首轮真机验证另加 `--hardware-campaign <id>`。
工作流始终是 `assess → plan → apply`，plan 返回的摘要与 token 必须原样带回：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge flash assess --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device <observation-id> --intent full-restore
target/debug/arkforge --runtime-dir /tmp/arkforge flash plan --artifact <artifact-id> --profile org.openharmony.dayu200@1.0.0 --device <observation-id> --intent full-restore
target/debug/arkforge --runtime-dir /tmp/arkforge --output jsonl flash apply --plan <plan-id> --expect-plan-sha256 <sha256> --ack <returned-token>
```

救援必须显式进入独立的原生 RockUSB 域，不安装也不调用外部设备工具：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge rescue list
target/debug/arkforge help rescue plan --format json
```

守护进程状态在重启后仍可查询：

```bash
target/debug/arkforge --runtime-dir /tmp/arkforge job list
target/debug/arkforge --runtime-dir /tmp/arkforge job show --job <job-id>
target/debug/arkforge --runtime-dir /tmp/arkforge job recovery guide --job <job-id>
```

Agent 可直接读取机器帮助，不需要推断 socket 或历史命令名：

```bash
target/debug/arkforge help --format json
target/debug/arkforge help flash apply --format json
target/debug/arkforge completion --shell zsh
```

## 文档

- 架构正本：[docs/architecture.md](docs/architecture.md)(状态 Proposed；ArkDeck 审计基线 `2849c5c1`)
- 任务台账：[TASKS.md](TASKS.md)(AF-V1 完成；AF-V2 真机全量刷写已三过——2026-08-18 首过、08-19 原生复验、08-20 经 ArkDeck authority 完成 23/23 写入/回读/重启/postflight；AF-V3 软件半完成；AF-V4 阻塞于证据门)
- 证据账本：[docs/evidence/ledger.md](docs/evidence/ledger.md)
- 实施决定：[docs/decisions/](docs/decisions/)
- 验收证据：[docs/evidence/](docs/evidence/)

## 工程布局

```text
crates/          九个边界 crate(architecture.md 4.2；含唯一 unsafe 的 arkforge-usb)
adapters/        arkforge-arkdeck-adapter：published step 映射表
profiles/        DeviceProfile 数据(schema 中性，设备在数据里)
proto/           IPC 正本 schema
transcripts/     golden transcript(GJ-4 campaign 收据链)
packaging/macos/ 签名/entitlement/打包契约的发布输入(AFD-0003)
fuzz/            见 fuzz/README.md
```

依赖：**无第三方运行时依赖**。SHA-256、deterministic CBOR、DEFLATE、tar、
Protobuf wire codec 均在仓内实现并对公开测试向量，理由见
[AFD-0001](docs/decisions/AFD-0001-zero-dependency-core.md)。

## 目标设备

- DAYU200(Rockchip RK3568 / RockUSB)：首个生产垂直，仅由 arkforged 原生
  RockUSB typed 端口完成枚举、读写与复位；
- DAYU600(Unisoc uis7885 / PAC)：证据门(architecture.md 17.5)通过前仅 inspect 与非可执行 PlanAssessment。
  当前 0/18 通过；`arkforge-artifact::pac` 是结构观测器而非 PAC parser。

## 与 ArkDeck 的关系

- 经 `arkforge-arkdeck-adapter` 接入；Core 不依赖 ArkDeck 类型；
- ArkDeck Runtime 与 `arkforge.cli` 是彼此独立的 authority namespace/runtime，不能接管
  对方已配对的 daemon；ArkDeck 后续按 canonical CLI/IPC 契约适配；
- ArkForge 独占固件解析、计划 lowering、USB/RockUSB mechanics、耐久执行与状态投影；
- CLI normal flash 的每个 mutation/destructive action 需要 exact StepPermit；
  outcomeUnknown 永不 replay；native rescue 使用独立的一次性 intent/receipt；
- 新 Operation/Provider/Profile 属 ArkDeck 明确要求 review 的变更，与真实产品能力同车交付。

## 命名

原案名 ArkFlash；2026-08-14 定名 ArkForge。ArkFlash 名称保留给未来面向用户的刷机 UI 产品位。
