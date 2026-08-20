# ArkForge

设备无关的刷机机械层(Rust)。把固件容器解析、芯片下载协议、USB transport、分区擦写/校验与厂商工具语义收进一个独立 daemon(`arkforged`)，ArkDeck 只保留 authority。

项目主页：[github.com/ArkDeck/ArkForge](https://github.com/ArkDeck/ArkForge)

~~~text
ArkDeck 决定：谁、对哪台设备、以哪个已发布 Operation、在什么安全边界下执行。

ArkForge 决定：该已授权语义计划如何通过具体固件格式、Provider 和 Transport 正确落地。
~~~

## 状态

DAYU200 的设备枚举、Loader 切模、读写、复位、九分区完整覆写和逐步状态均由
`arkforged` 的原生 RockUSB 实现；仓内没有 vendor 可执行调用路径。执行采用耐久
journal、精确 StepPermit、同一 transport session 的 freshness 复核，重启后不会
重放未决写入。DAYU200 profile 发布 1.0.0 complete-overwrite coverage；ArkDeck 保留
唯一 authority，使用 controller IPC 获取 admission/状态/收据并签发 permit。

DAYU600 只有 inspect 与非可执行 PlanAssessment：PAC 格式、下载协议与数据影响
全部未知(UNI-U01..U12)，17.5 的十八条证据门 0 条 PASS，见
[证据账本](docs/evidence/ledger.md)。

工具链钉在 `rust-toolchain.toml` 的 Rust 1.97.1 / Edition 2024。CI 执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

跑起 daemon 与只读 CLI：

```bash
cargo run -p arkforged --bin arkforged -- --runtime-dir /tmp/arkforge --profile profiles/dayu200.yaml --transcript transcripts/dayu200-gj4-ecamp-96effff15.yaml
```

```bash
cargo run -p arkforged --bin arkforge-cli -- --socket /tmp/arkforge/public.sock discover
```

守护进程状态在重启后仍可查询：

```bash
cargo run -p arkforged --bin arkforge-cli -- --socket /tmp/arkforge/public.sock jobs
cargo run -p arkforged --bin arkforge-cli -- --socket /tmp/arkforge/public.sock job <job-id>
cargo run -p arkforged --bin arkforge-cli -- --socket /tmp/arkforge/public.sock recovery-guide <job-id>
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
- ArkDeck Runtime 保留唯一 authority(admission / RuntimeCapability / device control / intent)；
- ArkForge 独占固件解析、计划 lowering、USB/RockUSB mechanics、耐久执行与状态投影；
- 每个 mutation/destructive action 需要 exact StepPermit；outcomeUnknown 永不 replay；
- 新 Operation/Provider/Profile 属 ArkDeck 明确要求 review 的变更，与真实产品能力同车交付。

## 命名

原案名 ArkFlash；2026-08-14 定名 ArkForge。ArkFlash 名称保留给未来面向用户的刷机 UI 产品位。
