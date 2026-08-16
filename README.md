# ArkForge

设备无关的刷机机械层(Rust)。把固件容器解析、芯片下载协议、USB transport、分区擦写/校验与厂商工具语义收进一个独立 daemon(`arkforged`)，ArkDeck 只保留 authority。

~~~text
ArkDeck 决定：谁、对哪台设备、以哪个已发布 Operation、在什么安全边界下执行。

ArkForge 决定：该已授权语义计划如何通过具体固件格式、Provider 和 Transport 正确落地。
~~~

## 状态

AF-V1(Core + DAYU200 read-only parity)已完成，见
[验收证据](docs/evidence/AF-V1-acceptance.md)；AF-V3 的软件半已完成，见
[AF-V3 验收证据](docs/evidence/AF-V3-acceptance.md)。当前构建是**只读垂直**：
`startExecution` 不可用，仓内没有 USB 后端，也没有 vendor 可执行调用路径。

DAYU600 只有 inspect 与非可执行 PlanAssessment：PAC 格式、下载协议与数据影响
全部未知(UNI-U01..U12)，17.5 的十八条证据门 0 条 PASS，见
[证据账本](docs/evidence/ledger.md)。

```bash
cargo test --workspace --offline
```

跑起 daemon 与只读 CLI：

```bash
cargo run -p arkforged --bin arkforged -- --runtime-dir /tmp/arkforge --profile profiles/dayu200.yaml --transcript transcripts/dayu200-gj4-ecamp-96effff15.yaml
```

```bash
cargo run -p arkforged --bin arkforge-cli -- --socket /tmp/arkforge/public.sock discover
```

## 文档

- 架构正本：[docs/architecture.md](docs/architecture.md)(状态 Proposed；ArkDeck 审计基线 `2849c5c1`)
- 任务台账：[TASKS.md](TASKS.md)(AF-V1 完成、AF-V3 软件半完成；AF-V2/AF-V4 阻塞于真机与证据门)
- 证据账本：[docs/evidence/ledger.md](docs/evidence/ledger.md)
- 实施决定：[docs/decisions/](docs/decisions/)
- 验收证据：[docs/evidence/](docs/evidence/)

## 工程布局

```text
crates/          八个边界 crate(architecture.md 4.2)
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

- DAYU200(Rockchip RK3568 / RockUSB)：首个生产垂直，首版封装固定哈希 rkdeveloptool；
- DAYU600(Unisoc uis7885 / PAC)：证据门(architecture.md 17.5)通过前仅 inspect 与非可执行 PlanAssessment。
  当前 0/18 通过；`arkforge-artifact::pac` 是结构观测器而非 PAC parser。

## 与 ArkDeck 的关系

- 经 `arkforge-arkdeck-adapter` 接入；Core 不依赖 ArkDeck 类型；
- ArkDeck Runtime 保留唯一 authority(admission / RuntimeCapability / device lane / intent)；
- 每个 mutation/destructive action 需要 exact StepPermit；outcomeUnknown 永不 replay；
- 新 Operation/Provider/Profile 属 ArkDeck 明确要求 review 的变更，与真实产品能力同车交付。

## 命名

原案名 ArkFlash；2026-08-14 定名 ArkForge。ArkFlash 名称保留给未来面向用户的刷机 UI 产品位。
